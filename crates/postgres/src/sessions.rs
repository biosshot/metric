use crate::{PostgresDatabase, PostgresError, codec};
use metric_domain::{
    ProjectId, Timestamp,
    finalization::{EnvironmentId, ReleaseId},
    sessions::{
        ReleaseHealthBucket, SessionId, SessionRecord, SessionState, SessionUpdate,
        SessionValueError, USER_SKETCH_BYTES, UserSketch,
    },
};
use metric_ports::{DurableOutcome, PortFuture, SessionStore, SignalStoreError};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
const MAX_BATCH: usize = 10_000;

#[derive(Clone, Copy, Debug)]
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
pub struct PostgresSessionStore {
    database: PostgresDatabase,
    retention: SessionRetention,
}

impl PostgresSessionStore {
    pub fn new(
        database: PostgresDatabase,
        retention: SessionRetention,
    ) -> Result<Self, PostgresError> {
        if !(1..=3650).contains(&retention.sessions_days)
            || !(1..=3650).contains(&retention.session_stats_hourly_days)
        {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self {
            database,
            retention,
        })
    }
    async fn persist(
        &self,
        updates: Vec<SessionUpdate>,
        stale: Option<(i64, i64)>,
    ) -> Result<Vec<DurableOutcome>, SignalStoreError> {
        if updates.len() > MAX_BATCH {
            return Err(SignalStoreError::InvalidData);
        }
        if updates.is_empty() {
            return Ok(vec![]);
        }
        for update in &updates {
            update.validate().map_err(invalid)?;
        }
        let projects: Vec<_> = updates
            .iter()
            .map(|value| value.project_id.get())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_projects(&mut transaction, &projects).await?;
        let mut keys: Vec<_> = updates
            .iter()
            .map(|value| (value.project_id.get(), value.id.as_bytes()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut records = BTreeMap::new();
        let mut before = BTreeMap::new();
        for (project, id) in keys.drain(..) {
            let row = select_sessions()
                .push(" WHERE project_id=")
                .push_bind(project)
                .push(" AND session_id=")
                .push_bind(id.to_vec())
                .push(" FOR UPDATE")
                .build()
                .fetch_optional(&mut *transaction)
                .await
                .map_err(unavailable)?;
            if let Some(row) = row {
                let record = decode(&row)?;
                before.insert((project, id), record.clone());
                records.insert((project, id), record);
            }
        }
        let mut outcomes = Vec::with_capacity(updates.len());
        let mut changed = BTreeSet::new();
        for mut update in updates {
            let key = (update.project_id.get(), update.id.as_bytes());
            if let Some((cutoff, age)) = stale {
                let Some(record) = records.get(&key) else {
                    outcomes.push(DurableOutcome::Duplicate);
                    continue;
                };
                if record.state != SessionState::Ok || record.last_update.unix_millis() > cutoff {
                    outcomes.push(DurableOutcome::Duplicate);
                    continue;
                }
                // Re-evaluate under the project/session locks, so a stale snapshot
                // cannot terminalize a Session refreshed while the worker waited.
                update.started_at = record.started_at;
                update.updated_at = at(record
                    .last_update
                    .unix_millis()
                    .checked_add(age)
                    .ok_or(SignalStoreError::InvalidData)?)?;
                update.sequence = record.sequence;
                update.user_digest = record.user_digest;
                update.duration_ms = Some(
                    u64::try_from(
                        update.updated_at.unix_millis() - record.started_at.unix_millis(),
                    )
                    .map_err(invalid)?,
                );
            }
            let applied = if let Some(record) = records.get_mut(&key) {
                record.merge(update).map_err(merge_error)?
            } else {
                records.insert(key, SessionRecord::from_update(update).map_err(invalid)?);
                true
            };
            if applied {
                changed.insert(key);
                outcomes.push(DurableOutcome::Accepted);
            } else {
                outcomes.push(DurableOutcome::Duplicate);
            }
        }
        for key in changed {
            let record = records.get(&key).ok_or(SignalStoreError::InvalidData)?;
            if let Some(previous) = before.get(&key) {
                self.health_delta(&mut transaction, previous, -1).await?;
            }
            save(&mut transaction, record, self.retention).await?;
            self.health_delta(&mut transaction, record, 1).await?;
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(outcomes)
    }
    async fn health_delta(
        &self,
        connection: &mut PgConnection,
        record: &SessionRecord,
        delta: i64,
    ) -> Result<(), SignalStoreError> {
        let hour = record.started_at.unix_millis().div_euclid(HOUR) * HOUR;
        if delta > 0 {
            sqlx::query("INSERT INTO metric.session_stats_hourly(project_id,release_id,environment_id,hour,expire_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(project_id,release_id,environment_id,hour) DO NOTHING")
                .bind(record.project_id.get()).bind(record.release_id.as_bytes().as_slice()).bind(record.environment_id.as_bytes().as_slice()).bind(hour).bind(expiry(hour,self.retention.session_stats_hourly_days)?).execute(&mut *connection).await.map_err(write_error)?;
        }
        let row=sqlx::query("SELECT sessions,crashed,abnormal,exited,user_sketch::text AS user_sketch,crashed_user_sketch::text AS crashed_user_sketch FROM metric.session_stats_hourly WHERE project_id=$1 AND release_id=$2 AND environment_id=$3 AND hour=$4 FOR UPDATE")
            .bind(record.project_id.get()).bind(record.release_id.as_bytes().as_slice()).bind(record.environment_id.as_bytes().as_slice()).bind(hour).fetch_optional(&mut *connection).await.map_err(unavailable)?.ok_or(SignalStoreError::InvalidData)?;
        let count = |name: &str, increment: i64| -> Result<i64, SignalStoreError> {
            row.try_get::<i64, _>(name)
                .map_err(invalid)?
                .checked_add(increment)
                .filter(|value| *value >= 0)
                .ok_or(SignalStoreError::InvalidData)
        };
        let sessions = count("sessions", delta)?;
        let crashed = count(
            "crashed",
            delta * i64::from(record.state == SessionState::Crashed),
        )?;
        let abnormal = count(
            "abnormal",
            delta * i64::from(record.state == SessionState::Abnormal),
        )?;
        let exited = count(
            "exited",
            delta * i64::from(record.state == SessionState::Exited),
        )?;
        let mut users = sketch(&row, "user_sketch")?;
        let mut crashed_users = sketch(&row, "crashed_user_sketch")?;
        if delta > 0
            && let Some(user) = record.user_digest
        {
            users.insert(user);
            if record.state == SessionState::Crashed {
                crashed_users.insert(user);
            }
        }
        sqlx::query("UPDATE metric.session_stats_hourly SET sessions=$5,crashed=$6,abnormal=$7,exited=$8,user_sketch=$9::bit(1024),crashed_user_sketch=$10::bit(1024) WHERE project_id=$1 AND release_id=$2 AND environment_id=$3 AND hour=$4")
            .bind(record.project_id.get()).bind(record.release_id.as_bytes().as_slice()).bind(record.environment_id.as_bytes().as_slice()).bind(hour).bind(sessions).bind(crashed).bind(abnormal).bind(exited).bind(sketch_text(users)).bind(sketch_text(crashed_users)).execute(connection).await.map_err(write_error)?;
        Ok(())
    }
}

impl SessionStore for PostgresSessionStore {
    fn persist_sessions(
        &self,
        updates: Vec<SessionUpdate>,
    ) -> PortFuture<'_, Result<Vec<DurableOutcome>, SignalStoreError>> {
        Box::pin(self.persist(updates, None))
    }
    fn load_session(
        &self,
        project: ProjectId,
        id: SessionId,
    ) -> PortFuture<'_, Result<SessionRecord, SignalStoreError>> {
        Box::pin(async move {
            let row = select_sessions()
                .push(" WHERE project_id=")
                .push_bind(project.get())
                .push(" AND session_id=")
                .push_bind(id.as_bytes().to_vec())
                .build()
                .fetch_optional(&self.database.pool)
                .await
                .map_err(unavailable)?
                .ok_or(SignalStoreError::NotFound)?;
            decode(&row)
        })
    }
    fn terminalize_stale_sessions(
        &self,
        now: Timestamp,
        maximum_active_age: Duration,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
        Box::pin(async move {
            let age = i64::try_from(maximum_active_age.as_millis()).map_err(invalid)?;
            if age <= 0 {
                return Err(SignalStoreError::InvalidData);
            }
            let cutoff = now
                .unix_millis()
                .checked_sub(age)
                .ok_or(SignalStoreError::InvalidData)?;
            at(cutoff)?;
            let rows=select_sessions().push(" WHERE state=0 AND last_update<=").push_bind(cutoff).push(" AND EXISTS(SELECT 1 FROM metric.projects p WHERE p.id=metric.sessions.project_id AND p.state<2) ORDER BY last_update,project_id,session_id LIMIT 10000").build().fetch_all(&self.database.pool).await.map_err(unavailable)?;
            let mut updates = Vec::with_capacity(rows.len());
            for row in rows {
                let record = decode(&row)?;
                updates.push(SessionUpdate {
                    id: record.id,
                    project_id: record.project_id,
                    release_id: record.release_id,
                    environment_id: record.environment_id,
                    started_at: record.started_at,
                    updated_at: record.last_update,
                    state: SessionState::Abnormal,
                    sequence: record.sequence,
                    duration_ms: None,
                    user_digest: record.user_digest,
                });
            }
            Ok(self
                .persist(updates, Some((cutoff, age)))
                .await?
                .into_iter()
                .filter(|result| *result == DurableOutcome::Accepted)
                .count() as u64)
        })
    }
    fn release_health(
        &self,
        project: ProjectId,
        release: ReleaseId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<Vec<ReleaseHealthBucket>, SignalStoreError>> {
        Box::pin(async move {
            if from >= until {
                return Err(SignalStoreError::InvalidData);
            }
            let rows=sqlx::query("SELECT s.hour,s.environment_id,e.name,s.sessions,s.crashed,s.abnormal,s.exited,s.user_sketch::text AS user_sketch,s.crashed_user_sketch::text AS crashed_user_sketch FROM metric.session_stats_hourly s LEFT JOIN metric.environments e ON e.project_id=s.project_id AND e.environment_id=s.environment_id WHERE s.project_id=$1 AND s.release_id=$2 AND s.hour>=$3 AND s.hour<$4 AND s.sessions>0 ORDER BY s.hour,s.environment_id LIMIT 10000")
            .bind(project.get()).bind(release.as_bytes().as_slice()).bind(from.unix_millis()).bind(until.unix_millis()).fetch_all(&self.database.pool).await.map_err(unavailable)?;
            rows.iter()
                .map(|row| {
                    let environment = EnvironmentId::from_bytes(fixed(row, "environment_id")?);
                    let name: Option<Vec<u8>> = row.try_get("name").map_err(invalid)?;
                    let name = name
                        .map(codec::decode_utf8)
                        .transpose()
                        .map_err(invalid)?
                        .unwrap_or_else(|| {
                            environment
                                .as_bytes()
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect()
                        });
                    let users = sketch(row, "user_sketch")?;
                    let crashed_users = sketch(row, "crashed_user_sketch")?;
                    Ok(ReleaseHealthBucket {
                        hour: timestamp(row, "hour")?,
                        environment_id: environment,
                        environment: name.into(),
                        sessions: unsigned(row, "sessions")?,
                        crashed: unsigned(row, "crashed")?,
                        abnormal: unsigned(row, "abnormal")?,
                        exited: unsigned(row, "exited")?,
                        approximate_users: users.estimate(),
                        approximate_crashed_users: crashed_users.estimate(),
                        user_sketch: users,
                        crashed_user_sketch: crashed_users,
                    })
                })
                .collect()
        })
    }
    fn rebuild_session_stats(
        &self,
        project: ProjectId,
        from: Timestamp,
        until: Timestamp,
    ) -> PortFuture<'_, Result<u64, SignalStoreError>> {
        Box::pin(async move {
            let (from, until) = rebuild_bounds(from, until)?;
            let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
            lock_projects(&mut transaction, &[project.get()]).await?;
            sqlx::query("DELETE FROM metric.session_stats_hourly WHERE project_id=$1 AND hour>=$2 AND hour<$3").bind(project.get()).bind(from).bind(until).execute(&mut *transaction).await.map_err(unavailable)?;
            // Fixed-size native bit aggregation preserves the same domain sketch bit
            // index. No Session rows or unbounded accumulator are transferred to Rust.
            let result=sqlx::query("INSERT INTO metric.session_stats_hourly(project_id,release_id,environment_id,hour,sessions,crashed,abnormal,exited,user_sketch,crashed_user_sketch,expire_at) SELECT $1,release_id,environment_id,hour,count(*),count(*) FILTER(WHERE state=2),count(*) FILTER(WHERE state=3),count(*) FILTER(WHERE state=1),bit_or(user_bits),COALESCE(bit_or(user_bits) FILTER(WHERE state=2),B'0'::bit(1024)),hour+$4 FROM (SELECT release_id,environment_id,state,floor(started_at::numeric/3600000)::bigint*3600000 AS hour,CASE WHEN user_digest IS NULL THEN B'0'::bit(1024) ELSE set_bit(B'0'::bit(1024),(get_byte(user_digest,0)*256+get_byte(user_digest,1))%1024,1) END AS user_bits FROM metric.sessions WHERE project_id=$1 AND started_at>=$2 AND started_at<$3) source GROUP BY release_id,environment_id,hour")
            .bind(project.get()).bind(from).bind(until).bind(i64::from(self.retention.session_stats_hourly_days)*DAY).execute(&mut *transaction).await.map_err(write_error)?;
            transaction.commit().await.map_err(unavailable)?;
            Ok(result.rows_affected())
        })
    }
}

fn select_sessions() -> QueryBuilder<Postgres> {
    QueryBuilder::new(
        "SELECT project_id,session_id,release_id,environment_id,started_at,last_update,state,sequence,finished_at,duration_ms,user_digest FROM metric.sessions",
    )
}
async fn lock_projects(
    connection: &mut PgConnection,
    projects: &[i32],
) -> Result<(), SignalStoreError> {
    crate::signal_storage::lock_projects(connection, projects).await?;
    // Per-project mutexes serialize Session writes, repairs and stale changes;
    // shared project locks still permit unrelated Event/Issue traffic.
    sqlx::query("INSERT INTO metric.session_project_locks(project_id) SELECT unnest($1::integer[]) ORDER BY 1 ON CONFLICT(project_id) DO NOTHING").bind(projects).execute(&mut *connection).await.map_err(unavailable)?;
    sqlx::query("SELECT project_id FROM metric.session_project_locks WHERE project_id=ANY($1) ORDER BY project_id FOR UPDATE").bind(projects).fetch_all(connection).await.map_err(unavailable)?;
    Ok(())
}
async fn save(
    connection: &mut PgConnection,
    record: &SessionRecord,
    retention: SessionRetention,
) -> Result<(), SignalStoreError> {
    let expire = record
        .finished_at
        .map(|time| expiry(time.unix_millis(), retention.sessions_days))
        .transpose()?;
    sqlx::query("INSERT INTO metric.sessions(project_id,session_id,release_id,environment_id,started_at,last_update,state,sequence,finished_at,duration_ms,user_digest,expire_at,archive_due) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) ON CONFLICT(project_id,session_id) DO UPDATE SET started_at=EXCLUDED.started_at,last_update=EXCLUDED.last_update,state=EXCLUDED.state,sequence=EXCLUDED.sequence,finished_at=EXCLUDED.finished_at,duration_ms=EXCLUDED.duration_ms,user_digest=EXCLUDED.user_digest,expire_at=EXCLUDED.expire_at,archive_due=EXCLUDED.archive_due,archive_segment=NULL")
        .bind(record.project_id.get()).bind(record.id.as_bytes().as_slice()).bind(record.release_id.as_bytes().as_slice()).bind(record.environment_id.as_bytes().as_slice()).bind(record.started_at.unix_millis()).bind(record.last_update.unix_millis()).bind(record.state.code() as i16).bind(record.sequence.map(|value|value.to_be_bytes().to_vec())).bind(record.finished_at.map(|value|value.unix_millis())).bind(record.duration_ms.map(|value|i64::try_from(value).map_err(invalid)).transpose()?).bind(record.user_digest.map(|value|value.to_vec())).bind(if retention.archive {None}else{expire}).bind(if retention.archive {expire}else{None}).execute(connection).await.map_err(write_error)?;
    Ok(())
}
pub(crate) fn decode(row: &PgRow) -> Result<SessionRecord, SignalStoreError> {
    let sequence: Option<Vec<u8>> = row.try_get("sequence").map_err(invalid)?;
    let user: Option<Vec<u8>> = row.try_get("user_digest").map_err(invalid)?;
    let value = SessionRecord {
        id: SessionId::from_bytes(fixed(row, "session_id")?),
        project_id: ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?,
        release_id: ReleaseId::from_bytes(fixed(row, "release_id")?),
        environment_id: EnvironmentId::from_bytes(fixed(row, "environment_id")?),
        started_at: timestamp(row, "started_at")?,
        last_update: timestamp(row, "last_update")?,
        state: SessionState::from_code(i32::from(row.try_get::<i16, _>("state").map_err(invalid)?))
            .map_err(invalid)?,
        sequence: sequence
            .map(|bytes| Ok(u64::from_be_bytes(bytes.try_into().map_err(invalid)?)))
            .transpose()?,
        finished_at: row
            .try_get::<Option<i64>, _>("finished_at")
            .map_err(invalid)?
            .map(at)
            .transpose()?,
        duration_ms: row
            .try_get::<Option<i64>, _>("duration_ms")
            .map_err(invalid)?
            .map(|value| u64::try_from(value).map_err(invalid))
            .transpose()?,
        user_digest: user
            .map(|bytes| bytes.try_into().map_err(invalid))
            .transpose()?,
    };
    if value.last_update < value.started_at
        || value.state.is_terminal() != value.finished_at.is_some()
        || value
            .finished_at
            .is_some_and(|time| time < value.started_at || time > value.last_update)
        || (value.finished_at.is_none() && value.duration_ms.is_some())
    {
        return Err(SignalStoreError::InvalidData);
    }
    Ok(value)
}
fn sketch(row: &PgRow, name: &str) -> Result<UserSketch, SignalStoreError> {
    let text: String = row.try_get(name).map_err(invalid)?;
    if text.len() != USER_SKETCH_BYTES * 8 {
        return Err(SignalStoreError::InvalidData);
    }
    let mut bytes = [0; USER_SKETCH_BYTES];
    for (index, bit) in text.bytes().enumerate() {
        match bit {
            b'0' => {}
            b'1' => bytes[index / 8] |= 1 << (index % 8),
            _ => return Err(SignalStoreError::InvalidData),
        }
    }
    Ok(UserSketch::from_bytes(bytes))
}
fn sketch_text(value: UserSketch) -> String {
    (0..USER_SKETCH_BYTES * 8)
        .map(|index| {
            if value.as_bytes()[index / 8] & (1 << (index % 8)) == 0 {
                '0'
            } else {
                '1'
            }
        })
        .collect()
}
fn rebuild_bounds(from: Timestamp, until: Timestamp) -> Result<(i64, i64), SignalStoreError> {
    if from >= until {
        return Err(SignalStoreError::InvalidData);
    }
    let upper = until.unix_millis();
    Ok((
        from.unix_millis().div_euclid(HOUR) * HOUR,
        upper.div_euclid(HOUR) * HOUR + i64::from(upper.rem_euclid(HOUR) != 0) * HOUR,
    ))
}
fn expiry(time: i64, days: u32) -> Result<i64, SignalStoreError> {
    let value = time
        .checked_add(i64::from(days) * DAY)
        .ok_or(SignalStoreError::InvalidData)?;
    at(value)?;
    Ok(value)
}
fn at(value: i64) -> Result<Timestamp, SignalStoreError> {
    Timestamp::from_unix_millis(value).map_err(invalid)
}
fn timestamp(row: &PgRow, name: &str) -> Result<Timestamp, SignalStoreError> {
    at(row.try_get(name).map_err(invalid)?)
}
fn fixed<const N: usize>(row: &PgRow, name: &str) -> Result<[u8; N], SignalStoreError> {
    row.try_get::<Vec<u8>, _>(name)
        .map_err(invalid)?
        .try_into()
        .map_err(invalid)
}
fn unsigned(row: &PgRow, name: &str) -> Result<u64, SignalStoreError> {
    u64::try_from(row.try_get::<i64, _>(name).map_err(invalid)?).map_err(invalid)
}
fn merge_error(error: SessionValueError) -> SignalStoreError {
    if error == SessionValueError::Conflict {
        SignalStoreError::Conflict
    } else {
        SignalStoreError::InvalidData
    }
}
fn invalid<T>(_: T) -> SignalStoreError {
    SignalStoreError::InvalidData
}
fn unavailable<T>(_: T) -> SignalStoreError {
    SignalStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> SignalStoreError {
    if matches!(
        error
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("22003" | "23514")
    ) {
        SignalStoreError::InvalidData
    } else {
        SignalStoreError::Unavailable
    }
}

#[cfg(test)]
mod tests;
