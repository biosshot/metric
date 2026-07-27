//! Optional bounded Parquet/Zstandard cold-archive orchestration.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use metric_domain::{
    Timestamp,
    archive::{
        ArchiveBatch, ArchiveBatchState, ArchiveEvent, ArchiveKind, ArchiveRecords, ArchiveSignal,
        EVENT_ARCHIVE_SCHEMA_VERSION, LOG_ARCHIVE_SCHEMA_VERSION, SPAN_ARCHIVE_SCHEMA_VERSION,
    },
    blob::{BlobKind, BlobNamespace},
};
use metric_ports::{
    ArchiveClaimRequest, ArchiveCompleteRequest, ArchiveSourceCommitRequest, ArchiveStore,
    ArchiveStoreError, BlobScanRequest, BlobStore, BlobStoreError, Clock,
};
use parquet::{
    basic::{Compression, ZstdLevel},
    data_type::{
        ByteArray, ByteArrayType, FixedLenByteArray, FixedLenByteArrayType, Int32Type, Int64Type,
    },
    file::{properties::WriterProperties, writer::SerializedFileWriter},
    schema::parser::parse_message_type,
};
use thiserror::Error;
use tokio::{
    task::JoinHandle,
    time::{MissedTickBehavior, interval},
};

use crate::shutdown::ShutdownSignal;

const MAXIMUM_EVENTS: usize = 10_000;
const MAXIMUM_TARGET_BYTES: usize = 512 * 1024 * 1024;
const MAXIMUM_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct ArchiveConfig {
    pub maximum_events: usize,
    pub target_uncompressed_bytes: usize,
    pub write_chunk_bytes: usize,
    pub poll_interval: Duration,
    pub hot_copy_delay: Duration,
    pub orphan_grace: Duration,
    pub cleanup_max_pages: usize,
}

impl Default for ArchiveConfig {
    fn default() -> Self {
        Self {
            maximum_events: 500,
            target_uncompressed_bytes: 64 * 1024 * 1024,
            write_chunk_bytes: 256 * 1024,
            poll_interval: Duration::from_secs(30),
            hot_copy_delay: Duration::ZERO,
            orphan_grace: Duration::from_secs(24 * 60 * 60),
            cleanup_max_pages: 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ArchiveError {
    #[error("archive configuration is invalid")]
    InvalidConfiguration,
    #[error("archive source or manifest data is invalid")]
    InvalidData,
    #[error("archive object publication failed integrity verification")]
    Integrity,
    #[error("archive dependency is temporarily unavailable")]
    Unavailable,
}

impl ArchiveError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "archive_invalid_configuration",
            Self::InvalidData => "archive_invalid_data",
            Self::Integrity => "archive_integrity_failed",
            Self::Unavailable => "archive_unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArchiveRunReport {
    pub claimed_records: usize,
    pub archived_records: usize,
    pub stored_bytes: u64,
}

pub struct ArchiveService {
    store: Arc<dyn ArchiveStore>,
    blobs: Arc<dyn BlobStore>,
    clock: Arc<dyn Clock>,
    config: ArchiveConfig,
}

impl ArchiveService {
    pub fn new(
        store: Arc<dyn ArchiveStore>,
        blobs: Arc<dyn BlobStore>,
        clock: Arc<dyn Clock>,
        config: ArchiveConfig,
    ) -> Result<Arc<Self>, ArchiveError> {
        validate(config)?;
        Ok(Arc::new(Self {
            store,
            blobs,
            clock,
            config,
        }))
    }

    pub async fn run_once(&self) -> Result<ArchiveRunReport, ArchiveError> {
        let mut report = ArchiveRunReport::default();
        for kind in ArchiveKind::ALL {
            let current = self.run_kind_once(kind).await?;
            report.claimed_records = report
                .claimed_records
                .saturating_add(current.claimed_records);
            report.archived_records = report
                .archived_records
                .saturating_add(current.archived_records);
            report.stored_bytes = report.stored_bytes.saturating_add(current.stored_bytes);
        }
        Ok(report)
    }

    async fn run_kind_once(&self, kind: ArchiveKind) -> Result<ArchiveRunReport, ArchiveError> {
        let started = Instant::now();
        let now = self.clock.now();
        let Some(batch) = self
            .store
            .claim(ArchiveClaimRequest {
                kind,
                now,
                maximum_events: self.config.maximum_events,
                target_uncompressed_bytes: self.config.target_uncompressed_bytes,
            })
            .await
            .map_err(map_store)?
        else {
            metrics::gauge!("metric_archive_pending_batch", "kind" => kind.name()).set(0.0);
            return Ok(ArchiveRunReport::default());
        };
        metrics::gauge!("metric_archive_pending_batch", "kind" => kind.name()).set(1.0);
        let claimed_records = batch.source_ids.len();
        let stored_bytes = match batch.state {
            ArchiveBatchState::Writing => self.publish(&batch, now).await?,
            ArchiveBatchState::Complete => 0,
        };
        let expire_at = add_duration(now, self.config.hot_copy_delay)?;
        let archived_records = self
            .store
            .commit_sources(ArchiveSourceCommitRequest {
                kind,
                segment_id: batch.segment_id,
                source_ids: batch.source_ids,
                expire_at,
            })
            .await
            .map_err(map_store)?;
        metrics::counter!(
            "metric_archive_runs_total",
            "kind" => kind.name(),
            "outcome" => "ok"
        )
        .increment(1);
        metrics::counter!("metric_archive_records_total", "kind" => kind.name())
            .increment(archived_records as u64);
        metrics::histogram!("metric_archive_run_duration_seconds", "kind" => kind.name())
            .record(started.elapsed().as_secs_f64());
        metrics::gauge!("metric_archive_pending_batch", "kind" => kind.name()).set(0.0);
        Ok(ArchiveRunReport {
            claimed_records,
            archived_records,
            stored_bytes,
        })
    }

    pub async fn cleanup_orphans_once(&self) -> Result<u64, ArchiveError> {
        let grace = i64::try_from(self.config.orphan_grace.as_millis())
            .map_err(|_| ArchiveError::InvalidConfiguration)?;
        let cutoff = Timestamp::from_unix_millis(
            self.clock
                .now()
                .unix_millis()
                .checked_sub(grace)
                .ok_or(ArchiveError::InvalidConfiguration)?,
        )
        .map_err(|_| ArchiveError::InvalidConfiguration)?;
        let mut deleted = 0_u64;
        for kind in ArchiveKind::ALL {
            let mut cursor = None;
            for _ in 0..self.config.cleanup_max_pages {
                let page = self
                    .blobs
                    .scan(BlobScanRequest {
                        namespace: BlobNamespace::archive(kind),
                        older_than: cutoff,
                        cursor,
                        limit: self.config.maximum_events,
                    })
                    .await
                    .map_err(map_blob)?;
                for object in page.objects {
                    if !self
                        .store
                        .object_referenced(&object.key)
                        .await
                        .map_err(map_store)?
                    {
                        self.blobs.delete(&object.key).await.map_err(map_blob)?;
                        deleted = deleted.saturating_add(1);
                    }
                }
                let Some(next) = page.next_cursor else {
                    break;
                };
                cursor = Some(next);
            }
        }
        metrics::counter!("metric_archive_orphan_objects_deleted_total").increment(deleted);
        Ok(deleted)
    }

    async fn publish(
        &self,
        batch: &ArchiveBatch,
        completed_at: metric_domain::Timestamp,
    ) -> Result<u64, ArchiveError> {
        if batch.records.len() != batch.source_ids.len() || batch.records.is_empty() {
            return Err(ArchiveError::InvalidData);
        }
        let kind = batch.kind;
        let records = batch.records.clone();
        let bytes = tokio::task::spawn_blocking(move || encode_batch(kind, &records))
            .await
            .map_err(|_| ArchiveError::Unavailable)??;
        let mut writer = self
            .blobs
            .begin(BlobKind::archive(kind), completed_at)
            .await
            .map_err(map_blob)?;
        for chunk in bytes.chunks(self.config.write_chunk_bytes) {
            if let Err(error) = writer.write_chunk(chunk.into()).await {
                let _ = writer.abort().await;
                return Err(map_blob(error));
            }
        }
        let object = writer
            .commit(batch.object_key.clone())
            .await
            .map_err(map_blob)?;
        if object.size != bytes.len() as u64
            || object.checksum.as_bytes() != *blake3::hash(&bytes).as_bytes()
        {
            return Err(ArchiveError::Integrity);
        }
        self.store
            .complete(ArchiveCompleteRequest {
                segment_id: batch.segment_id,
                object: object.clone(),
                completed_at,
            })
            .await
            .map_err(map_store)?;
        metrics::histogram!("metric_archive_segment_bytes", "kind" => kind.name())
            .record(object.size as f64);
        Ok(object.size)
    }
}

pub struct ArchiveTask {
    join: JoinHandle<()>,
}

impl ArchiveTask {
    pub async fn wait(self) {
        let _ = self.join.await;
    }
}

pub fn start_archive_worker(service: Arc<ArchiveService>, shutdown: ShutdownSignal) -> ArchiveTask {
    let poll_interval = service.config.poll_interval;
    ArchiveTask {
        join: tokio::spawn(async move {
            let mut tick = interval(poll_interval);
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return,
                    _ = tick.tick() => {
                        for kind in ArchiveKind::ALL {
                            if let Err(error) = service.run_kind_once(kind).await {
                                metrics::counter!(
                                    "metric_archive_runs_total",
                                    "kind" => kind.name(),
                                    "outcome" => error.code()
                                ).increment(1);
                                tracing::warn!(
                                    operation = "archive.run",
                                    kind = kind.name(),
                                    error_code = error.code(),
                                    "cold archive run failed; hot records were preserved"
                                );
                            }
                        }
                        if let Err(error) = service.cleanup_orphans_once().await {
                            metrics::counter!(
                                "metric_archive_cleanup_runs_total",
                                "outcome" => error.code()
                            ).increment(1);
                            tracing::warn!(
                                operation = "archive.cleanup",
                                error_code = error.code(),
                                "cold archive orphan cleanup failed"
                            );
                        }
                    }
                }
            }
        }),
    }
}

fn encode_batch(kind: ArchiveKind, records: &ArchiveRecords) -> Result<Vec<u8>, ArchiveError> {
    match (kind, records) {
        (ArchiveKind::Event, ArchiveRecords::Events(events)) => encode_parquet(events),
        (ArchiveKind::Log, ArchiveRecords::Logs(logs)) => encode_signal_parquet(kind, logs),
        (ArchiveKind::Span, ArchiveRecords::Spans(spans)) => encode_signal_parquet(kind, spans),
        (ArchiveKind::Session, ArchiveRecords::Sessions(sessions)) => {
            encode_signal_parquet(kind, sessions)
        }
        _ => Err(ArchiveError::InvalidData),
    }
}

pub fn encode_parquet(events: &[ArchiveEvent]) -> Result<Vec<u8>, ArchiveError> {
    if events.is_empty() || events.len() > MAXIMUM_EVENTS {
        return Err(ArchiveError::InvalidData);
    }
    let schema = Arc::new(
        parse_message_type(
            "message metric_event_archive_v1 {
                REQUIRED FIXED_LEN_BYTE_ARRAY (20) event_key;
                REQUIRED INT32 project_id;
                REQUIRED INT64 received_at_unix_ms;
                REQUIRED INT64 occurred_at_unix_ms;
                OPTIONAL FIXED_LEN_BYTE_ARRAY (16) issue_id;
                REQUIRED BYTE_ARRAY canonical_event_json (UTF8);
            }",
        )
        .map_err(|_| ArchiveError::InvalidData)?,
    );
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(
                ZstdLevel::try_new(3).map_err(|_| ArchiveError::InvalidConfiguration)?,
            ))
            .set_created_by(format!(
                "metric archive schema {EVENT_ARCHIVE_SCHEMA_VERSION}"
            ))
            .build(),
    );
    let mut output = Vec::new();
    {
        let mut writer = SerializedFileWriter::new(&mut output, schema, properties)
            .map_err(|_| ArchiveError::InvalidData)?;
        let mut row_group = writer
            .next_row_group()
            .map_err(|_| ArchiveError::InvalidData)?;

        let event_keys = events
            .iter()
            .map(|event| FixedLenByteArray::from(event.key.as_bytes().to_vec()))
            .collect::<Vec<_>>();
        write_column::<FixedLenByteArrayType>(&mut row_group, &event_keys, None)?;

        let projects = events
            .iter()
            .map(|event| event.project_id.get())
            .collect::<Vec<_>>();
        write_column::<Int32Type>(&mut row_group, &projects, None)?;

        let received = events
            .iter()
            .map(|event| event.received_at.unix_millis())
            .collect::<Vec<_>>();
        write_column::<Int64Type>(&mut row_group, &received, None)?;

        let occurred = events
            .iter()
            .map(|event| event.occurred_at.unix_millis())
            .collect::<Vec<_>>();
        write_column::<Int64Type>(&mut row_group, &occurred, None)?;

        let issues = events
            .iter()
            .filter_map(|event| {
                event
                    .issue_id
                    .map(|issue| FixedLenByteArray::from(issue.as_bytes().to_vec()))
            })
            .collect::<Vec<_>>();
        let issue_definitions = events
            .iter()
            .map(|event| i16::from(event.issue_id.is_some()))
            .collect::<Vec<_>>();
        write_column::<FixedLenByteArrayType>(&mut row_group, &issues, Some(&issue_definitions))?;

        let payloads = events
            .iter()
            .map(|event| ByteArray::from(event.canonical_payload.to_vec()))
            .collect::<Vec<_>>();
        write_column::<ByteArrayType>(&mut row_group, &payloads, None)?;

        row_group.close().map_err(|_| ArchiveError::InvalidData)?;
        writer.close().map_err(|_| ArchiveError::InvalidData)?;
    }
    if output.len() > MAXIMUM_TARGET_BYTES {
        return Err(ArchiveError::InvalidData);
    }
    Ok(output)
}

pub fn encode_signal_parquet(
    kind: ArchiveKind,
    records: &[ArchiveSignal],
) -> Result<Vec<u8>, ArchiveError> {
    if !matches!(
        kind,
        ArchiveKind::Log | ArchiveKind::Span | ArchiveKind::Session
    ) || records.is_empty()
        || records.len() > MAXIMUM_EVENTS
        || records.iter().any(|record| {
            serde_json::from_slice::<serde_json::Value>(&record.canonical_payload).is_err()
        })
    {
        return Err(ArchiveError::InvalidData);
    }
    let (schema_name, version) = match kind {
        ArchiveKind::Log => ("metric_log_archive_v1", LOG_ARCHIVE_SCHEMA_VERSION),
        ArchiveKind::Span => ("metric_span_archive_v1", SPAN_ARCHIVE_SCHEMA_VERSION),
        ArchiveKind::Session => (
            "metric_session_archive_v1",
            metric_domain::archive::SESSION_ARCHIVE_SCHEMA_VERSION,
        ),
        ArchiveKind::Event => return Err(ArchiveError::InvalidData),
    };
    let schema = Arc::new(
        parse_message_type(&format!(
            "message {schema_name} {{
                REQUIRED FIXED_LEN_BYTE_ARRAY (16) source_id;
                REQUIRED INT32 project_id;
                REQUIRED INT64 received_at_unix_ms;
                REQUIRED INT64 occurred_at_unix_ns;
                REQUIRED BYTE_ARRAY canonical_signal_json (UTF8);
            }}"
        ))
        .map_err(|_| ArchiveError::InvalidData)?,
    );
    let properties = Arc::new(
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(
                ZstdLevel::try_new(3).map_err(|_| ArchiveError::InvalidConfiguration)?,
            ))
            .set_created_by(format!("metric {} archive schema {version}", kind.name()))
            .build(),
    );
    let mut output = Vec::new();
    {
        let mut writer = SerializedFileWriter::new(&mut output, schema, properties)
            .map_err(|_| ArchiveError::InvalidData)?;
        let mut row_group = writer
            .next_row_group()
            .map_err(|_| ArchiveError::InvalidData)?;
        let ids = records
            .iter()
            .map(|record| FixedLenByteArray::from(record.id.to_vec()))
            .collect::<Vec<_>>();
        write_column::<FixedLenByteArrayType>(&mut row_group, &ids, None)?;
        let projects = records
            .iter()
            .map(|record| record.project_id.get())
            .collect::<Vec<_>>();
        write_column::<Int32Type>(&mut row_group, &projects, None)?;
        let received = records
            .iter()
            .map(|record| record.received_at.unix_millis())
            .collect::<Vec<_>>();
        write_column::<Int64Type>(&mut row_group, &received, None)?;
        let occurred = records
            .iter()
            .map(|record| record.occurred_at_ns)
            .collect::<Vec<_>>();
        write_column::<Int64Type>(&mut row_group, &occurred, None)?;
        let payloads = records
            .iter()
            .map(|record| ByteArray::from(record.canonical_payload.to_vec()))
            .collect::<Vec<_>>();
        write_column::<ByteArrayType>(&mut row_group, &payloads, None)?;
        row_group.close().map_err(|_| ArchiveError::InvalidData)?;
        writer.close().map_err(|_| ArchiveError::InvalidData)?;
    }
    if output.len() > MAXIMUM_TARGET_BYTES {
        return Err(ArchiveError::InvalidData);
    }
    Ok(output)
}

fn write_column<T: parquet::data_type::DataType>(
    row_group: &mut parquet::file::writer::SerializedRowGroupWriter<'_, &mut Vec<u8>>,
    values: &[T::T],
    definition_levels: Option<&[i16]>,
) -> Result<(), ArchiveError> {
    let mut column = row_group
        .next_column()
        .map_err(|_| ArchiveError::InvalidData)?
        .ok_or(ArchiveError::InvalidData)?;
    column
        .typed::<T>()
        .write_batch(values, definition_levels, None)
        .map_err(|_| ArchiveError::InvalidData)?;
    column.close().map_err(|_| ArchiveError::InvalidData)?;
    Ok(())
}

fn validate(config: ArchiveConfig) -> Result<(), ArchiveError> {
    let valid = (1..=MAXIMUM_EVENTS).contains(&config.maximum_events)
        && (1024..=MAXIMUM_TARGET_BYTES).contains(&config.target_uncompressed_bytes)
        && (4096..=MAXIMUM_CHUNK_BYTES).contains(&config.write_chunk_bytes)
        && !config.poll_interval.is_zero()
        && config.hot_copy_delay <= Duration::from_secs(24 * 60 * 60)
        && !config.orphan_grace.is_zero()
        && (1..=1_024).contains(&config.cleanup_max_pages);
    valid
        .then_some(())
        .ok_or(ArchiveError::InvalidConfiguration)
}

fn add_duration(
    timestamp: metric_domain::Timestamp,
    duration: Duration,
) -> Result<metric_domain::Timestamp, ArchiveError> {
    let millis =
        i64::try_from(duration.as_millis()).map_err(|_| ArchiveError::InvalidConfiguration)?;
    metric_domain::Timestamp::from_unix_millis(
        timestamp
            .unix_millis()
            .checked_add(millis)
            .ok_or(ArchiveError::InvalidConfiguration)?,
    )
    .map_err(|_| ArchiveError::InvalidConfiguration)
}

fn map_store(error: ArchiveStoreError) -> ArchiveError {
    match error {
        ArchiveStoreError::InvalidData => ArchiveError::InvalidData,
        ArchiveStoreError::Conflict => ArchiveError::Integrity,
        ArchiveStoreError::Unavailable => ArchiveError::Unavailable,
    }
}

fn map_blob(error: BlobStoreError) -> ArchiveError {
    match error {
        BlobStoreError::Corrupt => ArchiveError::Integrity,
        BlobStoreError::Invalid | BlobStoreError::TooLarge | BlobStoreError::Capacity => {
            ArchiveError::InvalidData
        }
        BlobStoreError::NotFound | BlobStoreError::Unavailable => ArchiveError::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    };

    use bytes::Bytes;
    use metric_domain::{EventId, EventKey, ProjectId, Timestamp, grouping::IssueId};
    use parquet::{
        file::reader::{FileReader, SerializedFileReader},
        record::RowAccessor,
    };

    use super::*;

    fn event(seed: u8, issue: bool) -> ArchiveEvent {
        let project_id = ProjectId::new(7).unwrap();
        ArchiveEvent {
            key: EventKey::new(project_id, EventId::from_bytes([seed; 16])),
            project_id,
            received_at: Timestamp::from_unix_millis(1_700_000_000_000 + i64::from(seed)).unwrap(),
            occurred_at: Timestamp::from_unix_millis(1_699_999_000_000 + i64::from(seed)).unwrap(),
            issue_id: issue.then(|| IssueId::from_bytes([seed; 16])),
            canonical_payload: format!("{{\"seed\":{seed}}}")
                .into_bytes()
                .into_boxed_slice(),
        }
    }

    #[test]
    fn parquet_is_zstandard_compressed_and_preserves_optional_rows() {
        let bytes = encode_parquet(&[event(1, true), event(2, false)]).unwrap();
        assert_eq!(&bytes[..4], b"PAR1");
        assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
        let reader = SerializedFileReader::new(Bytes::from(bytes)).unwrap();
        assert_eq!(reader.metadata().file_metadata().num_rows(), 2);
        assert_eq!(reader.metadata().num_row_groups(), 1);
        for column in reader.metadata().row_group(0).columns() {
            assert!(matches!(column.compression(), Compression::ZSTD(_)));
        }
    }

    #[test]
    fn log_span_and_session_parquet_preserve_canonical_rows() {
        let project_id = ProjectId::new(7).unwrap();
        let records = [
            ArchiveSignal {
                id: [1; 16],
                project_id,
                received_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
                occurred_at_ns: 1_699_999_000_000_000_000,
                canonical_payload: br#"{"body":"archived"}"#.as_slice().into(),
            },
            ArchiveSignal {
                id: [2; 16],
                project_id,
                received_at: Timestamp::from_unix_millis(1_700_000_000_001).unwrap(),
                occurred_at_ns: 1_699_999_000_000_000_001,
                canonical_payload: br#"{"op":"db.query"}"#.as_slice().into(),
            },
        ];
        for kind in [ArchiveKind::Log, ArchiveKind::Span, ArchiveKind::Session] {
            let bytes = encode_signal_parquet(kind, &records).unwrap();
            assert_eq!(&bytes[..4], b"PAR1");
            assert_eq!(&bytes[bytes.len() - 4..], b"PAR1");
            let reader = SerializedFileReader::new(Bytes::from(bytes)).unwrap();
            assert_eq!(reader.metadata().file_metadata().num_rows(), 2);
            assert!(
                reader
                    .metadata()
                    .row_group(0)
                    .columns()
                    .iter()
                    .all(|column| matches!(column.compression(), Compression::ZSTD(_)))
            );
            let recovered = reader
                .get_row_iter(None)
                .unwrap()
                .map(|row| row.unwrap().get_string(4).unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                recovered,
                [r#"{"body":"archived"}"#, r#"{"op":"db.query"}"#]
            );
        }
        assert_eq!(
            encode_signal_parquet(ArchiveKind::Event, &records),
            Err(ArchiveError::InvalidData)
        );
    }

    #[test]
    fn configuration_and_empty_segment_fail_closed() {
        assert_eq!(encode_parquet(&[]), Err(ArchiveError::InvalidData));
        assert_eq!(
            validate(ArchiveConfig {
                maximum_events: 0,
                ..ArchiveConfig::default()
            }),
            Err(ArchiveError::InvalidConfiguration)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "Phase 21 retained archive RPS/bytes baseline"]
    async fn performance_archive_writer_rps_mib_with_foreground_work() {
        const SEGMENTS: usize = 24;
        const EVENTS_PER_SEGMENT: usize = 500;
        const PAYLOAD_BYTES: usize = 2 * 1024;
        let project = ProjectId::new(7).unwrap();
        let fixture = (0..EVENTS_PER_SEGMENT)
            .map(|index| {
                let mut event_bytes = [0_u8; 16];
                event_bytes.copy_from_slice(&(index as u128 + 1).to_be_bytes());
                let mut payload = format!(
                    "{{\"event_id\":\"{}\",\"message\":\"",
                    EventId::from_bytes(event_bytes)
                )
                .into_bytes();
                payload.resize(PAYLOAD_BYTES.saturating_sub(2), b'x');
                payload.extend_from_slice(b"\"}");
                ArchiveEvent {
                    key: EventKey::new(project, EventId::from_bytes(event_bytes)),
                    project_id: project,
                    received_at: Timestamp::from_unix_millis(1_700_000_000_000 + index as i64)
                        .unwrap(),
                    occurred_at: Timestamp::from_unix_millis(1_699_999_000_000 + index as i64)
                        .unwrap(),
                    issue_id: Some(IssueId::from_bytes([index as u8; 16])),
                    canonical_payload: payload.into_boxed_slice(),
                }
            })
            .collect::<Vec<_>>();
        let peak_input_bytes = fixture
            .iter()
            .map(|event| event.canonical_payload.len() + 96)
            .sum::<usize>();
        assert!(peak_input_bytes < 64 * 1024 * 1024);

        let stop = Arc::new(AtomicBool::new(false));
        let foreground_ops = Arc::new(AtomicU64::new(0));
        let foreground = {
            let stop = Arc::clone(&stop);
            let foreground_ops = Arc::clone(&foreground_ops);
            tokio::spawn(async move {
                let input = [7_u8; 1024];
                while !stop.load(Ordering::Acquire) {
                    std::hint::black_box(blake3::hash(&input));
                    foreground_ops.fetch_add(1, Ordering::Relaxed);
                    if foreground_ops.load(Ordering::Relaxed).is_multiple_of(1_000) {
                        tokio::task::yield_now().await;
                    }
                }
            })
        };
        let started = Instant::now();
        let mut stored_bytes = 0_usize;
        for _ in 0..SEGMENTS {
            let events = fixture.clone();
            let segment = tokio::task::spawn_blocking(move || encode_parquet(&events))
                .await
                .unwrap()
                .unwrap();
            stored_bytes = stored_bytes.saturating_add(segment.len());
        }
        let elapsed = started.elapsed();
        stop.store(true, Ordering::Release);
        foreground.await.unwrap();
        let total_events = SEGMENTS * EVENTS_PER_SEGMENT;
        let archive_rps = total_events as f64 / elapsed.as_secs_f64();
        let stored_mib_per_second = stored_bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64();
        let input_mib_per_second =
            (SEGMENTS * peak_input_bytes) as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64();
        let foreground_rps = foreground_ops.load(Ordering::Acquire) as f64 / elapsed.as_secs_f64();
        eprintln!(
            "{{\"segments\":{SEGMENTS},\"events\":{total_events},\"payload_bytes\":{PAYLOAD_BYTES},\"peak_input_bytes\":{peak_input_bytes},\"stored_bytes\":{stored_bytes},\"archive_events_rps\":{archive_rps:.2},\"archive_input_mib_per_second\":{input_mib_per_second:.2},\"archive_stored_mib_per_second\":{stored_mib_per_second:.2},\"foreground_ops_rps\":{foreground_rps:.2},\"elapsed_ms\":{}}}",
            elapsed.as_millis()
        );
        assert!(archive_rps > 1.0);
        assert!(input_mib_per_second > 0.1);
        assert!(foreground_rps > 1.0);
    }
}
