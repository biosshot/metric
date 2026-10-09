//! Shared scoped admission guard for native signal stores.
use metric_ports::SignalStoreError;
use sqlx::{PgConnection, Row};

/// Callers supply unique sorted project IDs before taking lane-specific locks.
pub(crate) async fn lock_projects(
    connection: &mut PgConnection,
    projects: &[i32],
) -> Result<(), SignalStoreError> {
    let rows =
        sqlx::query("SELECT id,state FROM metric.projects WHERE id=ANY($1) ORDER BY id FOR SHARE")
            .bind(projects)
            .fetch_all(connection)
            .await
            .map_err(|_| SignalStoreError::Unavailable)?;
    if rows.len() != projects.len()
        || rows
            .iter()
            .any(|row| !matches!(row.try_get::<i16, _>("state"), Ok(0 | 1)))
    {
        return Err(SignalStoreError::NotFound);
    }
    Ok(())
}
