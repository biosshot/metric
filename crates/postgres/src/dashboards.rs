use crate::{PostgresDatabase, codec};
use metric_domain::{
    ProjectId, Timestamp,
    auth::UserId,
    dashboards::{
        Dashboard, DashboardId, DashboardRefreshInterval, DashboardWidget, DashboardWidgetId,
        MAX_DASHBOARD_WIDGETS, SavedQuery, SavedQueryId, WidgetShape,
    },
};
use metric_ports::{DashboardStore, DashboardStoreError, PortFuture};
use serde_json::{Value, json};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};

mod query_codec;
const MAX_BODY: usize = 512 * 1024;
const MAX_ENCODED: usize = MAX_BODY + 2;

#[derive(Clone)]
pub struct PostgresDashboardStore {
    database: PostgresDatabase,
}

// All configuration kinds share scoped SQL, revision CAS and bounded decoding.
struct Configuration {
    kind: i16,
    id: [u8; 16],
    project: ProjectId,
    name: Box<str>,
    revision: u64,
    created_by: UserId,
    updated_by: UserId,
    created_at: Timestamp,
    updated_at: Timestamp,
    body: Value,
}
impl PostgresDashboardStore {
    #[must_use]
    pub fn new(database: PostgresDatabase) -> Self {
        Self { database }
    }

    async fn write(
        &self,
        value: Configuration,
        expected: Option<u64>,
    ) -> Result<(), DashboardStoreError> {
        let revision = positive(value.revision)?;
        let creator = positive(value.created_by.get())?;
        let updater = positive(value.updated_by.get())?;
        let (body, _) = codec::encode_json(&value.body, 3, 64, MAX_BODY).map_err(invalid)?;
        if body.len() > MAX_ENCODED {
            return Err(DashboardStoreError::InvalidData);
        }
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_project(&mut transaction, value.project).await?;
        let written = if let Some(expected) = expected {
            sqlx::query("UPDATE metric.dashboard_configurations SET name=$4,revision=$5,created_by=$6,updated_by=$7,created_at=$8,updated_at=$9,body=$10 WHERE project_id=$1 AND kind=$2 AND configuration_id=$3 AND revision=$11")
                .bind(value.project.get()).bind(value.kind).bind(value.id.as_slice()).bind(value.name.as_ref()).bind(revision).bind(creator).bind(updater).bind(value.created_at.unix_millis()).bind(value.updated_at.unix_millis()).bind(body).bind(positive(expected)?).execute(&mut *transaction).await.map_err(write_error)?.rows_affected()
        } else {
            sqlx::query("INSERT INTO metric.dashboard_configurations(project_id,kind,configuration_id,name,revision,created_by,updated_by,created_at,updated_at,body) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                .bind(value.project.get()).bind(value.kind).bind(value.id.as_slice()).bind(value.name.as_ref()).bind(revision).bind(creator).bind(updater).bind(value.created_at.unix_millis()).bind(value.updated_at.unix_millis()).bind(body).execute(&mut *transaction).await.map_err(write_error)?.rows_affected()
        };
        if written != 1 {
            return Err(DashboardStoreError::Conflict);
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(())
    }
    fn select(project: ProjectId, kind: i16) -> QueryBuilder<Postgres> {
        let mut query = QueryBuilder::new(
            "SELECT project_id,configuration_id,name,revision,created_by,updated_by,created_at,updated_at,CASE WHEN octet_length(body)<=",
        );
        query
            .push_bind(MAX_ENCODED as i64)
            .push(" THEN body END AS body FROM metric.dashboard_configurations WHERE project_id=")
            .push_bind(project.get())
            .push(" AND kind=")
            .push_bind(kind);
        query
    }
    async fn list(
        &self,
        project: ProjectId,
        kind: i16,
        limit: usize,
    ) -> Result<Vec<PgRow>, DashboardStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(DashboardStoreError::InvalidData);
        }
        Self::select(project, kind)
            .push(" ORDER BY updated_at DESC,configuration_id DESC LIMIT ")
            .push_bind(limit as i64)
            .build()
            .fetch_all(&self.database.pool)
            .await
            .map_err(unavailable)
    }
    async fn load(
        &self,
        project: ProjectId,
        kind: i16,
        id: [u8; 16],
    ) -> Result<PgRow, DashboardStoreError> {
        Self::select(project, kind)
            .push(" AND configuration_id=")
            .push_bind(id.to_vec())
            .build()
            .fetch_optional(&self.database.pool)
            .await
            .map_err(unavailable)?
            .ok_or(DashboardStoreError::NotFound)
    }
    async fn delete(
        &self,
        project: ProjectId,
        kind: i16,
        id: [u8; 16],
    ) -> Result<(), DashboardStoreError> {
        let mut transaction = self.database.pool.begin().await.map_err(unavailable)?;
        lock_project(&mut transaction, project).await?;
        let result=sqlx::query("DELETE FROM metric.dashboard_configurations WHERE project_id=$1 AND kind=$2 AND configuration_id=$3").bind(project.get()).bind(kind).bind(id.as_slice()).execute(&mut *transaction).await.map_err(unavailable)?;
        if result.rows_affected() != 1 {
            return Err(DashboardStoreError::NotFound);
        }
        transaction.commit().await.map_err(unavailable)?;
        Ok(())
    }
}
impl DashboardStore for PostgresDashboardStore {
    fn list_saved_queries(
        &self,
        project: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<SavedQuery>, DashboardStoreError>> {
        Box::pin(async move {
            Ok(self
                .list(project, 0, limit)
                .await?
                .iter()
                .filter_map(|row| decode_saved(row).ok())
                .collect())
        })
    }
    fn load_saved_query(
        &self,
        project: ProjectId,
        id: SavedQueryId,
    ) -> PortFuture<'_, Result<SavedQuery, DashboardStoreError>> {
        Box::pin(async move { decode_saved(&self.load(project, 0, id.as_bytes()).await?) })
    }
    fn insert_saved_query(
        &self,
        value: SavedQuery,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move { self.write(saved_configuration(&value)?, None).await })
    }
    fn replace_saved_query(
        &self,
        value: SavedQuery,
        expected_revision: u64,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move {
            self.write(saved_configuration(&value)?, Some(expected_revision))
                .await
        })
    }
    fn delete_saved_query(
        &self,
        project: ProjectId,
        id: SavedQueryId,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(self.delete(project, 0, id.as_bytes()))
    }
    fn list_dashboards(
        &self,
        project: ProjectId,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<Dashboard>, DashboardStoreError>> {
        Box::pin(async move {
            Ok(self
                .list(project, 1, limit)
                .await?
                .iter()
                .filter_map(|row| decode_dashboard(row).ok())
                .collect())
        })
    }
    fn load_dashboard(
        &self,
        project: ProjectId,
        id: DashboardId,
    ) -> PortFuture<'_, Result<Dashboard, DashboardStoreError>> {
        Box::pin(async move { decode_dashboard(&self.load(project, 1, id.as_bytes()).await?) })
    }
    fn insert_dashboard(
        &self,
        value: Dashboard,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move { self.write(dashboard_configuration(&value)?, None).await })
    }
    fn replace_dashboard(
        &self,
        value: Dashboard,
        expected_revision: u64,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(async move {
            self.write(dashboard_configuration(&value)?, Some(expected_revision))
                .await
        })
    }
    fn delete_dashboard(
        &self,
        project: ProjectId,
        id: DashboardId,
    ) -> PortFuture<'_, Result<(), DashboardStoreError>> {
        Box::pin(self.delete(project, 1, id.as_bytes()))
    }
}
fn saved_configuration(value: &SavedQuery) -> Result<Configuration, DashboardStoreError> {
    value.validate().map_err(invalid)?;
    Ok(Configuration {
        kind: 0,
        id: value.id.as_bytes(),
        project: value.project_id,
        name: value.name.clone(),
        revision: value.revision,
        created_by: value.created_by,
        updated_by: value.updated_by,
        created_at: value.created_at,
        updated_at: value.updated_at,
        body: query_codec::encode(&value.query)?,
    })
}
fn dashboard_configuration(value: &Dashboard) -> Result<Configuration, DashboardStoreError> {
    value.validate().map_err(invalid)?;
    Ok(Configuration {
        kind: 1,
        id: value.id.as_bytes(),
        project: value.project_id,
        name: value.name.clone(),
        revision: value.revision,
        created_by: value.created_by,
        updated_by: value.updated_by,
        created_at: value.created_at,
        updated_at: value.updated_at,
        body: json!({"v":1,"refresh":value.refresh_interval.as_str(),"widgets":value.widgets.iter().map(|widget|json!({"id":widget.id.to_string(),"title":widget.title,"query":widget.saved_query_id.to_string(),"shape":widget.shape.as_str()})).collect::<Vec<_>>()}),
    })
}
fn decode_saved(row: &PgRow) -> Result<SavedQuery, DashboardStoreError> {
    let record = decode(row)?;
    let value = SavedQuery {
        id: SavedQueryId::from_bytes(record.id),
        project_id: record.project,
        name: record.name,
        query: query_codec::decode(&record.body)?,
        revision: record.revision,
        created_by: record.created_by,
        updated_by: record.updated_by,
        created_at: record.created_at,
        updated_at: record.updated_at,
    };
    value.validate().map_err(invalid)?;
    Ok(value)
}
fn decode_dashboard(row: &PgRow) -> Result<Dashboard, DashboardStoreError> {
    let record = decode(row)?;
    if record.body["v"].as_u64() != Some(1) {
        return Err(DashboardStoreError::InvalidData);
    }
    let widgets = query_codec::array(&record.body["widgets"], MAX_DASHBOARD_WIDGETS)?
        .iter()
        .map(|entry| {
            Ok(DashboardWidget {
                id: DashboardWidgetId::parse(query_codec::text(&entry["id"])?).map_err(invalid)?,
                title: query_codec::text(&entry["title"])?.into(),
                saved_query_id: SavedQueryId::parse(query_codec::text(&entry["query"])?)
                    .map_err(invalid)?,
                shape: WidgetShape::parse(query_codec::text(&entry["shape"])?).map_err(invalid)?,
            })
        })
        .collect::<Result<_, DashboardStoreError>>()?;
    let value = Dashboard {
        id: DashboardId::from_bytes(record.id),
        project_id: record.project,
        name: record.name,
        widgets,
        refresh_interval: DashboardRefreshInterval::parse(query_codec::text(
            &record.body["refresh"],
        )?)
        .map_err(invalid)?,
        revision: record.revision,
        created_by: record.created_by,
        updated_by: record.updated_by,
        created_at: record.created_at,
        updated_at: record.updated_at,
    };
    value.validate().map_err(invalid)?;
    Ok(value)
}
fn decode(row: &PgRow) -> Result<Configuration, DashboardStoreError> {
    let body: Vec<u8> = row.try_get("body").map_err(invalid)?;
    let decoded = codec::decode_json(&body, MAX_BODY).map_err(invalid)?;
    Ok(Configuration {
        kind: 0,
        id: row
            .try_get::<Vec<u8>, _>("configuration_id")
            .map_err(invalid)?
            .try_into()
            .map_err(invalid)?,
        project: ProjectId::new(row.try_get("project_id").map_err(invalid)?).map_err(invalid)?,
        name: row.try_get::<String, _>("name").map_err(invalid)?.into(),
        revision: unsigned(row, "revision")?,
        created_by: UserId::new(unsigned(row, "created_by")?).map_err(invalid)?,
        updated_by: UserId::new(unsigned(row, "updated_by")?).map_err(invalid)?,
        created_at: timestamp(row, "created_at")?,
        updated_at: timestamp(row, "updated_at")?,
        body: serde_json::from_slice(&decoded).map_err(invalid)?,
    })
}
async fn lock_project(
    connection: &mut PgConnection,
    project: ProjectId,
) -> Result<(), DashboardStoreError> {
    let state: Option<i16> =
        sqlx::query_scalar("SELECT state FROM metric.projects WHERE id=$1 FOR SHARE")
            .bind(project.get())
            .fetch_optional(connection)
            .await
            .map_err(unavailable)?;
    if matches!(state, Some(0 | 1)) {
        Ok(())
    } else {
        Err(DashboardStoreError::NotFound)
    }
}
fn positive(value: u64) -> Result<i64, DashboardStoreError> {
    i64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(DashboardStoreError::InvalidData)
}
fn unsigned(row: &PgRow, name: &str) -> Result<u64, DashboardStoreError> {
    u64::try_from(row.try_get::<i64, _>(name).map_err(invalid)?)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(DashboardStoreError::InvalidData)
}
fn timestamp(row: &PgRow, name: &str) -> Result<Timestamp, DashboardStoreError> {
    Timestamp::from_unix_millis(row.try_get(name).map_err(invalid)?).map_err(invalid)
}
fn invalid<T>(_: T) -> DashboardStoreError {
    DashboardStoreError::InvalidData
}
fn unavailable<T>(_: T) -> DashboardStoreError {
    DashboardStoreError::Unavailable
}
fn write_error(error: sqlx::Error) -> DashboardStoreError {
    match error
        .as_database_error()
        .and_then(|error| error.code())
        .as_deref()
    {
        Some("23505") => DashboardStoreError::Conflict,
        Some("23503") => DashboardStoreError::NotFound,
        _ => DashboardStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
