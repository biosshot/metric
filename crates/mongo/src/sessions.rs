use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use futures_util::TryStreamExt;
use metric_domain::{
    ProjectId, Timestamp,
    finalization::{EnvironmentId, ReleaseId},
    sessions::{
        ReleaseHealthBucket, SessionId, SessionRecord, SessionState, SessionUpdate,
        USER_SKETCH_BYTES, UserSketch,
    },
};
use metric_ports::{DurableOutcome, PortFuture, SessionStore, SignalStoreError};
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::{IndexOptions, UpdateOneModel},
};

const DAY_MILLIS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy)]
pub struct SessionRetention {
    pub sessions_days: u32,
    pub session_stats_hourly_days: u32,
    pub archive: bool,
}

impl Default for SessionRetention {
    fn default() -> Self {
        Self {
            sessions_days: 7,
            session_stats_hourly_days: 400,
            archive: false,
        }
    }
}

#[derive(Clone)]
pub struct MongoSessionStore {
    database: Database,
    retention: SessionRetention,
}

impl MongoSessionStore {
    #[must_use]
    pub const fn from_database(database: Database, retention: SessionRetention) -> Self {
        Self {
            database,
            retention,
        }
    }

    async fn persist_inner(
        &self,
        updates: Vec<SessionUpdate>,
    ) -> Result<Vec<DurableOutcome>, SignalStoreError> {
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        for update in &updates {
            update
                .validate()
                .map_err(|_| SignalStoreError::InvalidData)?;
        }
        let repair_ranges = health_ranges(&updates)?;

        let ids = updates
            .iter()
            .map(|update| Bson::Binary(binary(update.id.as_bytes())))
            .collect::<Vec<_>>();
        let mut cursor = self
            .database
            .collection::<Document>("sessions")
            .find(doc! { "_id": { "$in": ids } })
            .await
            .map_err(unavailable)?;
        let mut records = HashMap::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let record = decode_session(&document)?;
            records.insert(record.id, record);
        }
        let before = records.clone();
        let mut changed = HashSet::new();
        let mut outcomes = Vec::with_capacity(updates.len());
        for update in updates {
            let id = update.id;
            let was_changed = if let Some(record) = records.get_mut(&id) {
                record.merge(update).map_err(|error| match error {
                    metric_domain::sessions::SessionValueError::Conflict => {
                        SignalStoreError::Conflict
                    }
                    _ => SignalStoreError::InvalidData,
                })?
            } else {
                records.insert(
                    id,
                    SessionRecord::from_update(update)
                        .map_err(|_| SignalStoreError::InvalidData)?,
                );
                true
            };
            if was_changed {
                changed.insert(id);
                outcomes.push(DurableOutcome::Accepted);
            } else {
                outcomes.push(DurableOutcome::Duplicate);
            }
        }

        if changed.is_empty() {
            self.repair_health_ranges(&repair_ranges).await?;
            return Ok(outcomes);
        }
        let namespace = self.database.collection::<Document>("sessions").namespace();
        let mut models = Vec::with_capacity(changed.len());
        for id in &changed {
            let record = records.get(id).ok_or(SignalStoreError::InvalidData)?;
            let (set, unset) = encode_session(record, self.retention)?;
            models.push(
                UpdateOneModel::builder()
                    .namespace(namespace.clone())
                    .filter(doc! { "_id": binary(id.as_bytes()) })
                    .update(doc! { "$set": set, "$unset": unset })
                    .upsert(true)
                    .build(),
            );
        }
        self.database
            .client()
            .bulk_write(models)
            .ordered(false)
            .await
            .map_err(unavailable)?;

        if self
            .apply_health_transitions(&before, &records, &changed)
            .await
            .is_err()
        {
            self.repair_health_ranges(&repair_ranges).await?;
        }
        Ok(outcomes)
    }

    async fn repair_health_ranges(
        &self,
        ranges: &HashMap<ProjectId, (Timestamp, Timestamp)>,
    ) -> Result<(), SignalStoreError> {
        for (project_id, (from, until)) in ranges {
            self.rebuild_inner(*project_id, *from, *until).await?;
        }
        Ok(())
    }

    async fn apply_health_transitions(
        &self,
        before: &HashMap<SessionId, SessionRecord>,
        after: &HashMap<SessionId, SessionRecord>,
        changed: &HashSet<SessionId>,
    ) -> Result<(), SignalStoreError> {
        for id in changed {
            if let Some(old) = before.get(id) {
                self.apply_health_delta(old, -1).await?;
            }
            self.apply_health_delta(after.get(id).ok_or(SignalStoreError::InvalidData)?, 1)
                .await?;
        }
        Ok(())
    }

    async fn apply_health_delta(
        &self,
        record: &SessionRecord,
        delta: i64,
    ) -> Result<(), SignalStoreError> {
        let hour_millis = record.started_at.unix_millis().div_euclid(3_600_000) * 3_600_000;
        let hour =
            Timestamp::from_unix_millis(hour_millis).map_err(|_| SignalStoreError::InvalidData)?;
        let id = health_id(
            record.project_id,
            record.release_id,
            record.environment_id,
            hour,
        );
        let expiry = timestamp_add_days(hour, self.retention.session_stats_hourly_days)?;
        let crashed = i64::from(record.state == SessionState::Crashed) * delta;
        let abnormal = i64::from(record.state == SessionState::Abnormal) * delta;
        let exited = i64::from(record.state == SessionState::Exited) * delta;
        let set_on_insert = doc! {
            "p": record.project_id.get(),
            "r": binary(record.release_id.as_bytes()),
            "e": binary(record.environment_id.as_bytes()),
            "h": date(hour),
            "x": date(expiry),
        };
        let mut update = doc! {
            "$setOnInsert": set_on_insert,
            "$inc": {
                "n": delta,
                "c": crashed,
                "a": abnormal,
                "o": exited,
            }
        };
        if delta > 0 {
            if let Some(user) = record.user_digest {
                let bit_index =
                    usize::from(u16::from_be_bytes([user[0], user[1]])) % (USER_SKETCH_BYTES * 8);
                let word = bit_index / 64;
                let mask = 1_u64 << (bit_index % 64);
                let mut bits =
                    doc! { format!("u{word}"): { "or": i64::from_le_bytes(mask.to_le_bytes()) } };
                if record.state == SessionState::Crashed {
                    bits.insert(
                        format!("v{word}"),
                        doc! { "or": i64::from_le_bytes(mask.to_le_bytes()) },
                    );
                }
                update.insert("$bit", bits);
            }
        }
        self.database
            .collection::<Document>("session_stats_hourly")
            .update_one(doc! { "_id": binary(id) }, update)
            .upsert(true)
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn load_inner(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
    ) -> Result<SessionRecord, SignalStoreError> {
        let document = self
            .database
            .collection::<Document>("sessions")
            .find_one(doc! { "_id": binary(session_id.as_bytes()), "p": project_id.get() })
            .await
            .map_err(unavailable)?
            .ok_or(SignalStoreError::NotFound)?;
        decode_session(&document)
    }

    async fn health_inner(
        &self,
        project_id: ProjectId,
        release_id: ReleaseId,
        from: Timestamp,
        until: Timestamp,
    ) -> Result<Vec<ReleaseHealthBucket>, SignalStoreError> {
        if from >= until {
            return Err(SignalStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("session_stats_hourly")
            .find(doc! {
                "p": project_id.get(),
                "r": binary(release_id.as_bytes()),
                "h": { "$gte": date(from), "$lt": date(until) },
                "n": { "$gt": 0_i64 },
            })
            .sort(doc! { "h": 1, "e": 1 })
            .limit(10_000)
            .await
            .map_err(unavailable)?;
        let mut values = Vec::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let environment_id = EnvironmentId::from_bytes(fixed_binary(&document, "e")?);
            let environment = self
                .database
                .collection::<Document>("environments")
                .find_one(doc! {
                    "_id": binary(environment_id.as_bytes()),
                    "project_id": project_id.get(),
                })
                .projection(doc! { "name": 1 })
                .await
                .map_err(unavailable)?
                .and_then(|value| value.get_str("name").ok().map(str::to_owned))
                .unwrap_or_else(|| hex::encode(environment_id.as_bytes()));
            let user_sketch = decode_sketch(&document);
            let crashed_user_sketch = decode_named_sketch(&document, 'v');
            values.push(ReleaseHealthBucket {
                hour: timestamp(&document, "h")?,
                environment_id,
                environment: environment.into_boxed_str(),
                sessions: nonnegative_u64(&document, "n")?,
                crashed: nonnegative_u64(&document, "c")?,
                abnormal: nonnegative_u64(&document, "a")?,
                exited: nonnegative_u64(&document, "o")?,
                approximate_users: user_sketch.estimate(),
                approximate_crashed_users: crashed_user_sketch.estimate(),
                user_sketch,
                crashed_user_sketch,
            });
        }
        Ok(values)
    }

    async fn rebuild_inner(
        &self,
        project_id: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> Result<u64, SignalStoreError> {
        if from >= until {
            return Err(SignalStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("sessions")
            .find(doc! {
                "p": project_id.get(),
                "s": { "$gte": date(from), "$lt": date(until) },
            })
            .await
            .map_err(unavailable)?;
        let mut buckets = HashMap::<
            (ReleaseId, EnvironmentId, i64),
            (u64, u64, u64, u64, UserSketch, UserSketch),
        >::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let record = decode_session(&document)?;
            let hour = record.started_at.unix_millis().div_euclid(3_600_000) * 3_600_000;
            let bucket = buckets
                .entry((record.release_id, record.environment_id, hour))
                .or_insert((0, 0, 0, 0, UserSketch::default(), UserSketch::default()));
            bucket.0 = bucket.0.saturating_add(1);
            bucket.1 = bucket
                .1
                .saturating_add(u64::from(record.state == SessionState::Crashed));
            bucket.2 = bucket
                .2
                .saturating_add(u64::from(record.state == SessionState::Abnormal));
            bucket.3 = bucket
                .3
                .saturating_add(u64::from(record.state == SessionState::Exited));
            if let Some(user) = record.user_digest {
                bucket.4.insert(user);
                if record.state == SessionState::Crashed {
                    bucket.5.insert(user);
                }
            }
        }
        let stats = self.database.collection::<Document>("session_stats_hourly");
        stats
            .delete_many(doc! {
                "p": project_id.get(),
                "h": { "$gte": date(from), "$lt": date(until) },
            })
            .await
            .map_err(unavailable)?;
        let mut documents = Vec::with_capacity(buckets.len());
        for (
            (release, environment, hour_millis),
            (sessions, crashed, abnormal, exited, sketch, crashed_sketch),
        ) in buckets
        {
            let hour = Timestamp::from_unix_millis(hour_millis)
                .map_err(|_| SignalStoreError::InvalidData)?;
            documents.push(encode_health(
                project_id,
                release,
                environment,
                hour,
                sessions,
                crashed,
                abnormal,
                exited,
                sketch,
                crashed_sketch,
                self.retention.session_stats_hourly_days,
            )?);
        }
        let count = u64::try_from(documents.len()).map_err(|_| SignalStoreError::InvalidData)?;
        if !documents.is_empty() {
            stats
                .insert_many(documents)
                .ordered(false)
                .await
                .map_err(unavailable)?;
        }
        Ok(count)
    }
}

impl SessionStore for MongoSessionStore {
    fn persist_sessions(
        &self,
        updates: Vec<SessionUpdate>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(self.persist_inner(updates))
    }

    fn load_session(
        &self,
        project_id: ProjectId,
        session_id: SessionId,
    ) -> PortFuture<'_, Result<SessionRecord, SignalStoreError>> {
        Box::pin(self.load_inner(project_id, session_id))
    }

    fn terminalize_stale_sessions(
        &self,
        now: Timestamp,
        maximum_active_age: Duration,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
        Box::pin(async move {
            let age_millis = i64::try_from(maximum_active_age.as_millis())
                .map_err(|_| SignalStoreError::InvalidData)?;
            let cutoff = Timestamp::from_unix_millis(
                now.unix_millis()
                    .checked_sub(age_millis)
                    .ok_or(SignalStoreError::InvalidData)?,
            )
            .map_err(|_| SignalStoreError::InvalidData)?;
            let mut cursor = self
                .database
                .collection::<Document>("sessions")
                .find(doc! {
                    "q": SessionState::Ok.code(),
                    "f": { "$exists": false },
                    "l": { "$lte": date(cutoff) },
                })
                .limit(10_000)
                .await
                .map_err(unavailable)?;
            let mut updates = Vec::new();
            while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
                let record = decode_session(&document)?;
                let updated_at = Timestamp::from_unix_millis(
                    record
                        .last_update
                        .unix_millis()
                        .checked_add(age_millis)
                        .ok_or(SignalStoreError::InvalidData)?,
                )
                .map_err(|_| SignalStoreError::InvalidData)?;
                updates.push(SessionUpdate {
                    id: record.id,
                    project_id: record.project_id,
                    release_id: record.release_id,
                    environment_id: record.environment_id,
                    started_at: record.started_at,
                    updated_at,
                    state: SessionState::Abnormal,
                    sequence: record.sequence,
                    duration_ms: u64::try_from(
                        updated_at
                            .unix_millis()
                            .saturating_sub(record.started_at.unix_millis()),
                    )
                    .ok(),
                    user_digest: record.user_digest,
                });
            }
            let outcomes = self.persist_inner(updates).await?;
            Ok(outcomes
                .into_iter()
                .filter(|outcome| *outcome == DurableOutcome::Accepted)
                .count() as u64)
        })
    }

    fn release_health(
        &self,
        project_id: ProjectId,
        release_id: ReleaseId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<Vec<ReleaseHealthBucket>, SignalStoreError>> {
        Box::pin(self.health_inner(project_id, release_id, from, until))
    }

    fn rebuild_session_stats(
        &self,
        project_id: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
        Box::pin(self.rebuild_inner(project_id, from, until))
    }
}

fn encode_session(
    record: &SessionRecord,
    retention: SessionRetention,
) -> Result<(Document, Document), SignalStoreError> {
    let mut set = doc! {
        "_id": binary(record.id.as_bytes()),
        "p": record.project_id.get(),
        "r": binary(record.release_id.as_bytes()),
        "e": binary(record.environment_id.as_bytes()),
        "s": date(record.started_at),
        "l": date(record.last_update),
        "q": record.state.code(),
    };
    let mut unset = Document::new();
    optional_i64(&mut set, &mut unset, "n", record.sequence)?;
    optional_date(&mut set, &mut unset, "f", record.finished_at);
    optional_i64(&mut set, &mut unset, "d", record.duration_ms)?;
    optional_binary(&mut set, &mut unset, "u", record.user_digest);
    if let Some(finished) = record.finished_at {
        let expiry = timestamp_add_days(finished, retention.sessions_days)?;
        let field = if retention.archive { "h" } else { "x" };
        let other = if retention.archive { "x" } else { "h" };
        set.insert(field, date(expiry));
        unset.insert(other, "");
    } else {
        unset.insert("f", "");
        unset.insert("d", "");
        unset.insert("h", "");
        unset.insert("x", "");
    }
    unset.insert("z", "");
    Ok((set, unset))
}

fn decode_session(document: &Document) -> Result<SessionRecord, SignalStoreError> {
    Ok(SessionRecord {
        id: SessionId::from_bytes(fixed_binary(document, "_id")?),
        project_id: ProjectId::new(document.get_i32("p").map_err(invalid)?)
            .map_err(|_| SignalStoreError::InvalidData)?,
        release_id: ReleaseId::from_bytes(fixed_binary(document, "r")?),
        environment_id: EnvironmentId::from_bytes(fixed_binary(document, "e")?),
        started_at: timestamp(document, "s")?,
        last_update: timestamp(document, "l")?,
        state: SessionState::from_code(document.get_i32("q").map_err(invalid)?)
            .map_err(|_| SignalStoreError::InvalidData)?,
        sequence: optional_u64(document, "n")?,
        finished_at: optional_timestamp(document, "f")?,
        duration_ms: optional_u64(document, "d")?,
        user_digest: optional_fixed_binary(document, "u")?,
    })
}

pub(crate) async fn create_session_indexes(
    database: &Database,
) -> Result<(), mongodb::error::Error> {
    let sessions = database.collection::<Document>("sessions");
    sessions
        .create_index(
            IndexModel::builder()
                .keys(doc! { "p": 1, "_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name("session_project_identity".to_owned())
                        .build(),
                )
                .build(),
        )
        .await?;
    sessions
        .create_index(ttl_index("x", "session_expiry"))
        .await?;
    sessions
        .create_index(
            IndexModel::builder()
                .keys(doc! { "h": 1, "_id": 1 })
                .options(
                    IndexOptions::builder()
                        .name("session_archive_due".to_owned())
                        .partial_filter_expression(doc! { "h": { "$exists": true } })
                        .build(),
                )
                .build(),
        )
        .await?;
    database
        .collection::<Document>("session_stats_hourly")
        .create_index(ttl_index("x", "session_stats_expiry"))
        .await?;
    database
        .collection::<Document>("session_stats_hourly")
        .create_index(
            IndexModel::builder()
                .keys(doc! { "p": 1, "r": 1, "h": 1 })
                .options(
                    IndexOptions::builder()
                        .name("session_health_release_timeline".to_owned())
                        .build(),
                )
                .build(),
        )
        .await?;
    Ok(())
}

pub(crate) fn session_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "r", "e", "s", "l", "q"],
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int" },
            "r": { "bsonType": "binData" },
            "e": { "bsonType": "binData" },
            "s": { "bsonType": "date" },
            "l": { "bsonType": "date" },
            "q": { "bsonType": "int", "minimum": 0, "maximum": 3 },
            "n": { "bsonType": "long", "minimum": 0 },
            "f": { "bsonType": "date" },
            "d": { "bsonType": "long", "minimum": 0 },
            "u": { "bsonType": "binData" },
            "h": { "bsonType": "date" },
            "z": { "bsonType": "binData" },
            "x": { "bsonType": "date" },
        },
        "additionalProperties": false,
    }}
}

pub(crate) fn session_stats_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object",
        "required": ["_id", "p", "r", "e", "h", "n", "c", "a", "o", "x"],
        "properties": {
            "_id": { "bsonType": "binData" },
            "p": { "bsonType": "int" },
            "r": { "bsonType": "binData" },
            "e": { "bsonType": "binData" },
            "h": { "bsonType": "date" },
            "n": { "bsonType": "long" },
            "c": { "bsonType": "long" },
            "a": { "bsonType": "long" },
            "o": { "bsonType": "long" },
            "u0": { "bsonType": "long" }, "u1": { "bsonType": "long" },
            "u2": { "bsonType": "long" }, "u3": { "bsonType": "long" },
            "u4": { "bsonType": "long" }, "u5": { "bsonType": "long" },
            "u6": { "bsonType": "long" }, "u7": { "bsonType": "long" },
            "u8": { "bsonType": "long" }, "u9": { "bsonType": "long" },
            "u10": { "bsonType": "long" }, "u11": { "bsonType": "long" },
            "u12": { "bsonType": "long" }, "u13": { "bsonType": "long" },
            "u14": { "bsonType": "long" }, "u15": { "bsonType": "long" },
            "v0": { "bsonType": "long" }, "v1": { "bsonType": "long" },
            "v2": { "bsonType": "long" }, "v3": { "bsonType": "long" },
            "v4": { "bsonType": "long" }, "v5": { "bsonType": "long" },
            "v6": { "bsonType": "long" }, "v7": { "bsonType": "long" },
            "v8": { "bsonType": "long" }, "v9": { "bsonType": "long" },
            "v10": { "bsonType": "long" }, "v11": { "bsonType": "long" },
            "v12": { "bsonType": "long" }, "v13": { "bsonType": "long" },
            "v14": { "bsonType": "long" }, "v15": { "bsonType": "long" },
            "x": { "bsonType": "date" },
        },
        "additionalProperties": false,
    }}
}

fn health_id(
    project_id: ProjectId,
    release_id: ReleaseId,
    environment_id: EnvironmentId,
    hour: Timestamp,
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"metric/session-health-hour/v1");
    hasher.update(&project_id.get().to_be_bytes());
    hasher.update(&release_id.as_bytes());
    hasher.update(&environment_id.as_bytes());
    hasher.update(&hour.unix_millis().to_be_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hasher.finalize().as_bytes()[..16]);
    bytes
}

fn health_ranges(
    updates: &[SessionUpdate],
) -> Result<HashMap<ProjectId, (Timestamp, Timestamp)>, SignalStoreError> {
    let mut ranges = HashMap::new();
    for update in updates {
        let hour = update.started_at.unix_millis().div_euclid(3_600_000) * 3_600_000;
        let from = Timestamp::from_unix_millis(hour).map_err(|_| SignalStoreError::InvalidData)?;
        let until = Timestamp::from_unix_millis(
            hour.checked_add(3_600_000)
                .ok_or(SignalStoreError::InvalidData)?,
        )
        .map_err(|_| SignalStoreError::InvalidData)?;
        ranges
            .entry(update.project_id)
            .and_modify(|range: &mut (Timestamp, Timestamp)| {
                range.0 = range.0.min(from);
                range.1 = range.1.max(until);
            })
            .or_insert((from, until));
    }
    Ok(ranges)
}

#[allow(clippy::too_many_arguments)]
fn encode_health(
    project_id: ProjectId,
    release_id: ReleaseId,
    environment_id: EnvironmentId,
    hour: Timestamp,
    sessions: u64,
    crashed: u64,
    abnormal: u64,
    exited: u64,
    sketch: UserSketch,
    crashed_sketch: UserSketch,
    retention_days: u32,
) -> Result<Document, SignalStoreError> {
    let mut document = doc! {
        "_id": binary(health_id(project_id, release_id, environment_id, hour)),
        "p": project_id.get(),
        "r": binary(release_id.as_bytes()),
        "e": binary(environment_id.as_bytes()),
        "h": date(hour),
        "n": i64::try_from(sessions).map_err(|_| SignalStoreError::InvalidData)?,
        "c": i64::try_from(crashed).map_err(|_| SignalStoreError::InvalidData)?,
        "a": i64::try_from(abnormal).map_err(|_| SignalStoreError::InvalidData)?,
        "o": i64::try_from(exited).map_err(|_| SignalStoreError::InvalidData)?,
        "x": date(timestamp_add_days(hour, retention_days)?),
    };
    for (index, bytes) in sketch.as_bytes().chunks_exact(8).enumerate() {
        let word = u64::from_le_bytes(bytes.try_into().expect("eight-byte chunk"));
        if word != 0 {
            document.insert(format!("u{index}"), i64::from_le_bytes(word.to_le_bytes()));
        }
    }
    for (index, bytes) in crashed_sketch.as_bytes().chunks_exact(8).enumerate() {
        let word = u64::from_le_bytes(bytes.try_into().expect("eight-byte chunk"));
        if word != 0 {
            document.insert(format!("v{index}"), i64::from_le_bytes(word.to_le_bytes()));
        }
    }
    Ok(document)
}

fn decode_sketch(document: &Document) -> UserSketch {
    decode_named_sketch(document, 'u')
}

fn decode_named_sketch(document: &Document, prefix: char) -> UserSketch {
    let mut bytes = [0; USER_SKETCH_BYTES];
    for index in 0..USER_SKETCH_BYTES / 8 {
        let word = document.get_i64(format!("{prefix}{index}")).unwrap_or(0);
        bytes[index * 8..index * 8 + 8].copy_from_slice(&word.to_le_bytes());
    }
    UserSketch::from_bytes(bytes)
}

fn nonnegative_u64(document: &Document, key: &str) -> Result<u64, SignalStoreError> {
    u64::try_from(document.get_i64(key).map_err(invalid)?)
        .map_err(|_| SignalStoreError::InvalidData)
}

fn ttl_index(field: &str, name: &str) -> IndexModel {
    IndexModel::builder()
        .keys(doc! { field: 1 })
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .expire_after(Duration::ZERO)
                .build(),
        )
        .build()
}

fn binary(bytes: impl AsRef<[u8]>) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.as_ref().to_vec(),
    }
}

fn fixed_binary<const N: usize>(
    document: &Document,
    key: &str,
) -> Result<[u8; N], SignalStoreError> {
    document
        .get_binary_generic(key)
        .map_err(invalid)?
        .as_slice()
        .try_into()
        .map_err(|_| SignalStoreError::InvalidData)
}

fn optional_fixed_binary<const N: usize>(
    document: &Document,
    key: &str,
) -> Result<Option<[u8; N]>, SignalStoreError> {
    match document.get(key) {
        None => Ok(None),
        Some(Bson::Binary(binary)) if binary.subtype == BinarySubtype::Generic => binary
            .bytes
            .as_slice()
            .try_into()
            .map(Some)
            .map_err(|_| SignalStoreError::InvalidData),
        Some(_) => Err(SignalStoreError::InvalidData),
    }
}

fn optional_binary<const N: usize>(
    set: &mut Document,
    unset: &mut Document,
    key: &str,
    value: Option<[u8; N]>,
) {
    if let Some(value) = value {
        set.insert(key, binary(value));
    } else {
        unset.insert(key, "");
    }
}

fn optional_i64(
    set: &mut Document,
    unset: &mut Document,
    key: &str,
    value: Option<u64>,
) -> Result<(), SignalStoreError> {
    if let Some(value) = value {
        set.insert(
            key,
            i64::try_from(value).map_err(|_| SignalStoreError::InvalidData)?,
        );
    } else {
        unset.insert(key, "");
    }
    Ok(())
}

fn optional_u64(document: &Document, key: &str) -> Result<Option<u64>, SignalStoreError> {
    match document.get(key) {
        None => Ok(None),
        Some(Bson::Int64(value)) => u64::try_from(*value)
            .map(Some)
            .map_err(|_| SignalStoreError::InvalidData),
        Some(_) => Err(SignalStoreError::InvalidData),
    }
}

fn optional_date(set: &mut Document, unset: &mut Document, key: &str, value: Option<Timestamp>) {
    if let Some(value) = value {
        set.insert(key, date(value));
    } else {
        unset.insert(key, "");
    }
}

fn timestamp(document: &Document, key: &str) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(key)
            .map_err(invalid)?
            .timestamp_millis(),
    )
    .map_err(|_| SignalStoreError::InvalidData)
}

fn optional_timestamp(
    document: &Document,
    key: &str,
) -> Result<Option<Timestamp>, SignalStoreError> {
    match document.get(key) {
        None => Ok(None),
        Some(Bson::DateTime(value)) => Timestamp::from_unix_millis(value.timestamp_millis())
            .map(Some)
            .map_err(|_| SignalStoreError::InvalidData),
        Some(_) => Err(SignalStoreError::InvalidData),
    }
}

fn date(timestamp: Timestamp) -> DateTime {
    DateTime::from_millis(timestamp.unix_millis())
}

fn timestamp_add_days(timestamp: Timestamp, days: u32) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(
        timestamp
            .unix_millis()
            .checked_add(i64::from(days).saturating_mul(DAY_MILLIS))
            .ok_or(SignalStoreError::InvalidData)?,
    )
    .map_err(|_| SignalStoreError::InvalidData)
}

fn unavailable(_: mongodb::error::Error) -> SignalStoreError {
    SignalStoreError::Unavailable
}

fn invalid<T>(_: T) -> SignalStoreError {
    SignalStoreError::InvalidData
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(state: SessionState) -> SessionRecord {
        SessionRecord {
            id: SessionId::from_bytes([1; 16]),
            project_id: ProjectId::new(42).unwrap(),
            release_id: ReleaseId::from_bytes([2; 16]),
            environment_id: EnvironmentId::from_bytes([3; 16]),
            started_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
            last_update: Timestamp::from_unix_millis(1_700_000_001_000).unwrap(),
            state,
            sequence: Some(2),
            finished_at: state
                .is_terminal()
                .then(|| Timestamp::from_unix_millis(1_700_000_001_000).unwrap()),
            duration_ms: state.is_terminal().then_some(1_000),
            user_digest: Some([4; 16]),
        }
    }

    #[test]
    fn compact_session_codec_omits_inapplicable_fields_and_meets_budget() {
        let (active, unset) =
            encode_session(&record(SessionState::Ok), SessionRetention::default()).unwrap();
        assert!(!active.contains_key("f"));
        assert!(!active.contains_key("x"));
        assert!(unset.contains_key("x"));

        let (terminal, _) =
            encode_session(&record(SessionState::Crashed), SessionRetention::default()).unwrap();
        assert!(terminal.contains_key("f"));
        assert!(terminal.contains_key("x"));
        assert!(!terminal.contains_key("h"));
        let encoded = mongodb::bson::to_vec(&terminal).unwrap();
        let required_index_key_bytes = 16 + (4 + 16) + 8 + 8;
        assert!(
            encoded.len() + required_index_key_bytes <= 384,
            "Session BSON plus required index keys is {} bytes",
            encoded.len() + required_index_key_bytes
        );
    }

    #[test]
    fn ttl_and_archive_modes_are_mutually_safe() {
        let terminal = record(SessionState::Exited);
        let (ttl, _) = encode_session(
            &terminal,
            SessionRetention {
                archive: false,
                ..SessionRetention::default()
            },
        )
        .unwrap();
        let (archive, _) = encode_session(
            &terminal,
            SessionRetention {
                archive: true,
                ..SessionRetention::default()
            },
        )
        .unwrap();
        assert!(ttl.contains_key("x") && !ttl.contains_key("h"));
        assert!(archive.contains_key("h") && !archive.contains_key("x"));
    }

    #[test]
    fn health_codec_keeps_fixed_user_sketches() {
        let mut users = UserSketch::default();
        users.insert([1; 16]);
        let mut crashed = UserSketch::default();
        crashed.insert([1; 16]);
        let document = encode_health(
            ProjectId::new(42).unwrap(),
            ReleaseId::from_bytes([2; 16]),
            EnvironmentId::from_bytes([3; 16]),
            Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
            1,
            1,
            0,
            0,
            users,
            crashed,
            400,
        )
        .unwrap();
        assert_eq!(decode_sketch(&document).estimate(), 1);
        assert_eq!(decode_named_sketch(&document, 'v').estimate(), 1);
    }
}
