use super::*;
use metric_domain::monitors::{
    MonitorConfig, MonitorSchedule, SealedUptimeHeaderValue, UptimeEndpoint, UptimeHeader,
    UptimeMethod, UptimeMonitorConfig,
};
use serde_json::{Value, json};

pub(super) fn encode_config(monitor: &MonitorDefinition) -> Result<(Vec<u8>, usize)> {
    monitor.validate().map_err(invalid)?;
    // Reparse publicly constructible interval variants before committing them.
    let config = &monitor.config;
    if let MonitorSchedule::Interval { minutes } = config.schedule {
        MonitorSchedule::interval(minutes).map_err(invalid)?;
    }
    let uptime = monitor.uptime.as_ref().map(|v| {
        json!([
            v.endpoint.as_str(),
            v.method.as_str(),
            v.expected_status_min,
            v.expected_status_max,
            v.timeout_seconds,
            v.max_redirects,
            v.headers
                .iter()
                .map(|h| json!([h.name, h.value.expose_ciphertext(), h.sensitive]))
                .collect::<Vec<_>>()
        ])
    });
    let value = json!([
        config.schedule.kind(),
        config.schedule.value(),
        config.checkin_margin_seconds,
        config.max_runtime_seconds,
        uptime
    ]);
    let (encoded, canonical) =
        crate::codec::encode_json(&value, 3, 64, MAX_CONFIG).map_err(invalid)?;
    Ok((encoded, canonical.len()))
}
fn array(value: &Value, len: usize) -> Result<&[Value]> {
    let items = value.as_array().ok_or(SignalStoreError::InvalidData)?;
    if items.len() != len {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(items)
}
fn text(value: &Value) -> Result<&str> {
    value.as_str().ok_or(SignalStoreError::InvalidData)
}
fn uint(value: &Value) -> Result<u32> {
    u32::try_from(value.as_u64().ok_or(SignalStoreError::InvalidData)?).map_err(invalid)
}
pub(super) fn decode_config(
    encoded: &[u8],
    size: i32,
) -> Result<(MonitorConfig, Option<UptimeMonitorConfig>)> {
    let bytes = crate::codec::decode_json(encoded, MAX_CONFIG).map_err(invalid)?;
    if usize::try_from(size).map_err(invalid)? != bytes.len() {
        return Err(SignalStoreError::InvalidData);
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(invalid)?;
    let values = array(&value, 5)?;
    let schedule = match text(&values[0])? {
        "interval" => MonitorSchedule::interval(text(&values[1])?.parse().map_err(invalid)?),
        "crontab" => MonitorSchedule::crontab(text(&values[1])?),
        _ => return Err(SignalStoreError::InvalidData),
    }
    .map_err(invalid)?;
    let config = MonitorConfig {
        schedule,
        checkin_margin_seconds: uint(&values[2])?,
        max_runtime_seconds: uint(&values[3])?,
    };
    config.validate().map_err(invalid)?;
    let uptime = if values[4].is_null() {
        None
    } else {
        let parts = array(&values[4], 7)?;
        let headers = parts[6].as_array().ok_or(SignalStoreError::InvalidData)?;
        if headers.len() > 16 {
            return Err(SignalStoreError::InvalidData);
        }
        let headers = headers
            .iter()
            .map(|v| {
                let h = array(v, 3)?;
                let bytes = h[1].as_array().ok_or(SignalStoreError::InvalidData)?;
                if bytes.len() > 2112 {
                    return Err(SignalStoreError::InvalidData);
                }
                let bytes = bytes
                    .iter()
                    .map(|v| u8::try_from(uint(v)?).map_err(invalid))
                    .collect::<Result<Vec<_>>>()?;
                Ok(UptimeHeader {
                    name: text(&h[0])?.into(),
                    value: SealedUptimeHeaderValue::new(bytes).map_err(invalid)?,
                    sensitive: h[2].as_bool().ok_or(SignalStoreError::InvalidData)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let uptime = UptimeMonitorConfig {
            endpoint: UptimeEndpoint::new(text(&parts[0])?).map_err(invalid)?,
            method: UptimeMethod::parse(text(&parts[1])?).map_err(invalid)?,
            expected_status_min: u16::try_from(uint(&parts[2])?).map_err(invalid)?,
            expected_status_max: u16::try_from(uint(&parts[3])?).map_err(invalid)?,
            timeout_seconds: uint(&parts[4])?,
            max_redirects: u8::try_from(uint(&parts[5])?).map_err(invalid)?,
            headers: headers.into_boxed_slice(),
        };
        uptime.validate().map_err(invalid)?;
        Some(uptime)
    };
    Ok((config, uptime))
}
pub(super) fn decode_monitor(row: &PgRow) -> Result<StoredMonitor> {
    let body: Vec<u8> = row.try_get("config").map_err(invalid)?;
    let (config, uptime) = decode_config(&body, row.try_get("config_size").map_err(invalid)?)?;
    let definition = MonitorDefinition {
        id: MonitorId::from_bytes(fixed(row, "monitor_id")?),
        project_id: ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?,
        slug: row.try_get::<String, _>("slug").map_err(invalid)?.into(),
        name: row.try_get::<String, _>("name").map_err(invalid)?.into(),
        environment_id: EnvironmentId::from_bytes(fixed(row, "environment_id")?),
        environment: row
            .try_get::<String, _>("environment")
            .map_err(invalid)?
            .into(),
        enabled: row.try_get("enabled").map_err(invalid)?,
        managed_by_web: row.try_get("managed_by_web").map_err(invalid)?,
        revision: u64::try_from(row.try_get::<i64, _>("revision").map_err(invalid)?)
            .map_err(invalid)?,
        config,
        uptime,
        next_expected_at: timestamp(row, "next_expected_at")?,
        last_run_id: optional_fixed(row, "last_run_id")?.map(MonitorRunId::from_bytes),
        last_status: row
            .try_get::<Option<i16>, _>("last_status")
            .map_err(invalid)?
            .map(parse_status)
            .transpose()?,
        last_check_in_at: optional_time(row, "last_check_in_at")?,
        created_at: timestamp(row, "created_at")?,
        updated_at: timestamp(row, "updated_at")?,
    };
    definition.validate().map_err(invalid)?;
    if row.try_get::<i16, _>("kind").map_err(invalid)? != i16::from(definition.is_uptime())
        || timestamp(row, "due_at")? != due(&definition)?
    {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(StoredMonitor {
        definition,
        lease: optional_time(row, "lease_until")?,
    })
}
pub(super) fn decode_run(row: &PgRow) -> Result<MonitorRun> {
    let run = MonitorRun {
        id: MonitorRunId::from_bytes(fixed(row, "run_id")?),
        project_id: ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?,
        monitor_id: MonitorId::from_bytes(fixed(row, "monitor_id")?),
        check_in_id: optional_fixed(row, "check_in_id")?,
        status: parse_status(row.try_get("status").map_err(invalid)?)?,
        source: match row.try_get::<i16, _>("source").map_err(invalid)? {
            0 => MonitorRunSource::Sdk,
            1 => MonitorRunSource::Scheduler,
            _ => return Err(SignalStoreError::InvalidData),
        },
        scheduled_for: optional_time(row, "scheduled_for")?,
        started_at: timestamp(row, "started_at")?,
        finished_at: optional_time(row, "finished_at")?,
        duration_ms: row
            .try_get::<Option<i64>, _>("duration_ms")
            .map_err(invalid)?
            .map(|v| u64::try_from(v).map_err(invalid))
            .transpose()?,
        received_at: timestamp(row, "received_at")?,
        release_id: optional_fixed(row, "release_id")?.map(ReleaseId::from_bytes),
        timeout_at: optional_time(row, "timeout_at")?,
        delete_at: optional_time(row, "delete_at")?,
        http_status: row
            .try_get::<Option<i32>, _>("http_status")
            .map_err(invalid)?
            .map(|v| u16::try_from(v).map_err(invalid))
            .transpose()?,
        uptime_failure: row
            .try_get::<Option<i16>, _>("uptime_failure")
            .map_err(invalid)?
            .map(parse_failure)
            .transpose()?,
    };
    run.validate().map_err(invalid)?;
    Ok(run)
}
fn fixed(row: &PgRow, key: &str) -> Result<[u8; 16]> {
    row.try_get::<Vec<u8>, _>(key)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn optional_fixed(row: &PgRow, key: &str) -> Result<Option<[u8; 16]>> {
    row.try_get::<Option<Vec<u8>>, _>(key)
        .map_err(invalid)?
        .map(|v| v.try_into().map_err(invalid))
        .transpose()
}
fn timestamp(row: &PgRow, key: &str) -> Result<Timestamp> {
    at(row.try_get(key).map_err(invalid)?)
}
fn optional_time(row: &PgRow, key: &str) -> Result<Option<Timestamp>> {
    row.try_get::<Option<i64>, _>(key)
        .map_err(invalid)?
        .map(at)
        .transpose()
}

pub(super) fn status(value: MonitorRunStatus) -> i16 {
    match value {
        MonitorRunStatus::InProgress => 0,
        MonitorRunStatus::Success => 1,
        MonitorRunStatus::Error => 2,
        MonitorRunStatus::Timeout => 3,
        MonitorRunStatus::Missed => 4,
    }
}
fn parse_status(value: i16) -> Result<MonitorRunStatus> {
    match value {
        0 => Ok(MonitorRunStatus::InProgress),
        1 => Ok(MonitorRunStatus::Success),
        2 => Ok(MonitorRunStatus::Error),
        3 => Ok(MonitorRunStatus::Timeout),
        4 => Ok(MonitorRunStatus::Missed),
        _ => Err(SignalStoreError::InvalidData),
    }
}
pub(super) fn failure(value: UptimeFailure) -> i16 {
    match value {
        UptimeFailure::Dns => 0,
        UptimeFailure::ForbiddenAddress => 1,
        UptimeFailure::Connect => 2,
        UptimeFailure::Timeout => 3,
        UptimeFailure::Redirect => 4,
        UptimeFailure::TooManyRedirects => 5,
        UptimeFailure::ResponseTooLarge => 6,
        UptimeFailure::UnexpectedStatus => 7,
    }
}
fn parse_failure(value: i16) -> Result<UptimeFailure> {
    match value {
        0 => Ok(UptimeFailure::Dns),
        1 => Ok(UptimeFailure::ForbiddenAddress),
        2 => Ok(UptimeFailure::Connect),
        3 => Ok(UptimeFailure::Timeout),
        4 => Ok(UptimeFailure::Redirect),
        5 => Ok(UptimeFailure::TooManyRedirects),
        6 => Ok(UptimeFailure::ResponseTooLarge),
        7 => Ok(UptimeFailure::UnexpectedStatus),
        _ => Err(SignalStoreError::InvalidData),
    }
}
