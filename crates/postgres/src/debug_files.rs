//! Debug metadata only; existing application streaming and BlobStore own the bytes.
use crate::{PostgresDatabase, PostgresError};
use metric_domain::{
    OrganizationId, ProjectId, Timestamp,
    api::ProjectView,
    debug_files::{
        CodeId, DebugFile, DebugFileId, DebugFileType, DebugId, DebugUpload, DebugUploadRecord,
        DebugUploadState, MAX_DEBUG_CHUNKS, validate_debug_name,
    },
};
use metric_ports::{DebugFileStore, DebugFileStoreError, PortFuture};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};

type Result<T> = std::result::Result<T, DebugFileStoreError>;
// Bound variable-size fields before driver transfer, even for damaged stored data.
const FILE_COLUMNS: &str = "project_id,file_id,codec_revision,debug_id,code_id,file_type,checksum,sha1,size,CASE WHEN octet_length(name)<=255 THEN name END AS name,uploaded_at";
const UPLOAD_COLUMNS: &str = "upload_id,project_id,organization_id,codec_revision,sha1,CASE WHEN octet_length(name)<=255 THEN name END AS name,debug_id,code_id,CASE WHEN octet_length(chunks)<=5120 THEN chunks END AS chunks,state,attempts,CASE WHEN octet_length(error_code)<=64 THEN error_code END AS error_code,octet_length(error_code) AS error_length,created_at,updated_at";

#[derive(Debug, Clone, Copy)]
pub struct DebugFileQuota {
    pub maximum_bytes_per_project: u64,
    pub maximum_files_per_project: u64,
}
impl Default for DebugFileQuota {
    fn default() -> Self {
        Self {
            maximum_bytes_per_project: 10 * 1024 * 1024 * 1024,
            maximum_files_per_project: 10_000,
        }
    }
}
#[derive(Clone)]
pub struct PostgresDebugFileStore {
    database: PostgresDatabase,
    quota: DebugFileQuota,
}
impl PostgresDebugFileStore {
    pub fn new(
        database: PostgresDatabase,
        quota: DebugFileQuota,
    ) -> std::result::Result<Self, PostgresError> {
        if i64::try_from(quota.maximum_bytes_per_project).is_err()
            || i64::try_from(quota.maximum_files_per_project).is_err()
        {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self { database, quota })
    }
    async fn by_sha1(&self, project: ProjectId, sha1: [u8; 20]) -> Result<Option<DebugFile>> {
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query
            .push(FILE_COLUMNS)
            .push(" FROM metric.debug_files WHERE project_id=")
            .push_bind(project.get())
            .push(" AND sha1=")
            .push_bind(sha1.to_vec());
        query
            .build()
            .fetch_optional(&self.database.pool)
            .await
            .map_err(error)?
            .as_ref()
            .map(decode_file)
            .transpose()
    }
    async fn upsert(&self, upload: DebugUpload) -> Result<DebugUploadRecord> {
        let name = name(&upload.name)?;
        if !(1..=MAX_DEBUG_CHUNKS).contains(&upload.chunks.len()) {
            return Err(DebugFileStoreError::InvalidData);
        }
        let chunks: Vec<u8> = upload
            .chunks
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect();
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        let project = lock_project(&mut tx, upload.project_id, false).await?;
        if get::<i64>(&project, "organization_id")? != upload.organization_id.get() as i64 {
            return Err(DebugFileStoreError::NotFound);
        }
        sqlx::query("INSERT INTO metric.debug_uploads(upload_id,project_id,organization_id,sha1,name,debug_id,code_id,chunks,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT(upload_id) DO NOTHING")
            .bind(upload.id.as_slice()).bind(upload.project_id.get()).bind(upload.organization_id.get() as i64).bind(upload.sha1.as_slice()).bind(name.as_ref())
            .bind(upload.debug_id.as_ref().map(|id|id.encode().into_vec())).bind(upload.code_id.as_ref().map(|id|id.encode().into_vec())).bind(chunks)
            .bind(upload.created_at.unix_millis()).bind(upload.updated_at.unix_millis()).execute(&mut *tx).await.map_err(error)?;
        let record = load_upload(&mut tx, upload.id).await?;
        if record.upload.project_id != upload.project_id
            || record.upload.organization_id != upload.organization_id
            || record.upload.sha1 != upload.sha1
            || record.upload.chunks != upload.chunks
        {
            return Err(DebugFileStoreError::Conflict);
        }
        tx.commit().await.map_err(error)?;
        Ok(record)
    }
    async fn set_state(
        &self,
        id: [u8; 16],
        state: DebugUploadState,
        now: Timestamp,
        code: Option<Box<str>>,
    ) -> Result<()> {
        if code
            .as_ref()
            .is_some_and(|value| value.len() > 64 || value.chars().any(char::is_control))
        {
            return Err(DebugFileStoreError::InvalidData);
        }
        let owner: Option<i32> =
            sqlx::query_scalar("SELECT project_id FROM metric.debug_uploads WHERE upload_id=$1")
                .bind(id.as_slice())
                .fetch_optional(&self.database.pool)
                .await
                .map_err(error)?;
        let owner = ProjectId::new(owner.ok_or(DebugFileStoreError::NotFound)?)
            .map_err(|_| DebugFileStoreError::InvalidData)?;
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        lock_project(&mut tx, owner, false).await?;
        let affected = sqlx::query("UPDATE metric.debug_uploads SET state=$3,updated_at=$4,error_code=$5,attempts=attempts+$6 WHERE upload_id=$1 AND project_id=$2")
            .bind(id.as_slice()).bind(owner.get()).bind(state_code(state)).bind(now.unix_millis())
            .bind(if state==DebugUploadState::Pending || state==DebugUploadState::Complete { None } else { code.as_deref() })
            .bind(i64::from(state==DebugUploadState::Assembling)).execute(&mut *tx).await.map_err(error)?.rows_affected();
        if affected != 1 {
            return Err(DebugFileStoreError::NotFound);
        }
        tx.commit().await.map_err(error)
    }
    async fn publish(&self, id: [u8; 16], file: DebugFile) -> Result<u64> {
        let name = name(&file.name)?;
        let size = i64::try_from(file.size).map_err(|_| DebugFileStoreError::InvalidData)?;
        if file.debug_id.is_none() && file.code_id.is_none() {
            return Err(DebugFileStoreError::InvalidData);
        }
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        let project = lock_project(&mut tx, file.project_id, true).await?;
        let upload = load_upload(&mut tx, id).await?;
        if upload.upload.project_id != file.project_id || upload.upload.sha1 != file.sha1 {
            return Err(DebugFileStoreError::Conflict);
        }
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query
            .push(FILE_COLUMNS)
            .push(" FROM metric.debug_files WHERE project_id=")
            .push_bind(file.project_id.get())
            .push(" AND sha1=")
            .push_bind(file.sha1.to_vec());
        let existing = query
            .build()
            .fetch_optional(&mut *tx)
            .await
            .map_err(error)?;
        let revision = if let Some(existing) = existing {
            let existing = decode_file(&existing)?;
            if existing.id != file.id || existing.checksum != file.checksum {
                return Err(DebugFileStoreError::Conflict);
            }
            unsigned(&project, "debug_file_revision")?
        } else {
            let bytes = unsigned(&project, "debug_file_bytes")?;
            let files = unsigned(&project, "debug_file_count")?;
            if bytes
                .checked_add(file.size)
                .is_none_or(|value| value > self.quota.maximum_bytes_per_project)
                || files >= self.quota.maximum_files_per_project
            {
                return Err(DebugFileStoreError::Quota);
            }
            sqlx::query("INSERT INTO metric.debug_files(project_id,file_id,debug_id,code_id,file_type,checksum,sha1,size,name,uploaded_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
                .bind(file.project_id.get()).bind(file.id.as_bytes().as_slice()).bind(file.debug_id.as_ref().map(|id|id.encode().into_vec()))
                .bind(file.code_id.as_ref().map(|id|id.encode().into_vec())).bind(i16::from(file.file_type as u8)).bind(file.checksum.as_slice()).bind(file.sha1.as_slice()).bind(size)
                .bind(name.as_ref()).bind(file.uploaded_at.unix_millis()).execute(&mut *tx).await.map_err(error)?;
            let revision: i64 = sqlx::query_scalar("UPDATE metric.projects SET debug_file_revision=debug_file_revision+1,debug_file_bytes=debug_file_bytes+$2,debug_file_count=debug_file_count+1 WHERE id=$1 RETURNING debug_file_revision")
                .bind(file.project_id.get()).bind(size).fetch_one(&mut *tx).await.map_err(error)?;
            u64::try_from(revision).map_err(|_| DebugFileStoreError::InvalidData)?
        };
        sqlx::query("UPDATE metric.debug_uploads SET state=2,error_code=NULL,updated_at=$3 WHERE upload_id=$1 AND project_id=$2")
            .bind(id.as_slice()).bind(file.project_id.get()).bind(file.uploaded_at.unix_millis()).execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(revision)
    }
    async fn find(
        &self,
        project: ProjectId,
        debug: Option<DebugId>,
        code: Option<CodeId>,
        limit: usize,
    ) -> Result<Vec<DebugFile>> {
        if !(1..=20).contains(&limit) || (debug.is_none() && code.is_none()) {
            return Err(DebugFileStoreError::InvalidData);
        }
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query
            .push(FILE_COLUMNS)
            .push(" FROM metric.debug_files WHERE project_id=")
            .push_bind(project.get())
            .push(" AND (");
        if let Some(debug) = debug.as_ref() {
            query.push("debug_id=").push_bind(debug.encode().into_vec());
        }
        if debug.is_some() && code.is_some() {
            query.push(" OR ");
        }
        if let Some(code) = code {
            query.push("code_id=").push_bind(code.encode().into_vec());
        }
        query
            .push(") ORDER BY uploaded_at DESC,file_id LIMIT ")
            .push_bind(limit as i64);
        query
            .build()
            .fetch_all(&self.database.pool)
            .await
            .map_err(error)?
            .iter()
            .map(decode_file)
            .collect()
    }
    async fn load(&self, project: ProjectId, id: DebugFileId) -> Result<DebugFile> {
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query
            .push(FILE_COLUMNS)
            .push(" FROM metric.debug_files WHERE project_id=")
            .push_bind(project.get())
            .push(" AND file_id=")
            .push_bind(id.as_bytes().to_vec());
        let row = query
            .build()
            .fetch_optional(&self.database.pool)
            .await
            .map_err(error)?
            .ok_or(DebugFileStoreError::NotFound)?;
        decode_file(&row)
    }
    async fn delete(
        &self,
        project: ProjectId,
        id: DebugFileId,
    ) -> Result<Option<(DebugFile, u64)>> {
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        let owner = lock_project(&mut tx, project, true).await?;
        let mut query =
            QueryBuilder::<Postgres>::new("DELETE FROM metric.debug_files WHERE project_id=");
        query
            .push_bind(project.get())
            .push(" AND file_id=")
            .push_bind(id.as_bytes().to_vec())
            .push(" RETURNING ")
            .push(FILE_COLUMNS);
        let Some(row) = query
            .build()
            .fetch_optional(&mut *tx)
            .await
            .map_err(error)?
        else {
            return Ok(None);
        };
        let file = decode_file(&row)?;
        if unsigned(&owner, "debug_file_bytes")? < file.size
            || unsigned(&owner, "debug_file_count")? == 0
        {
            return Err(DebugFileStoreError::InvalidData);
        }
        let revision: i64 = sqlx::query_scalar("UPDATE metric.projects SET debug_file_revision=debug_file_revision+1,debug_file_bytes=debug_file_bytes-$2,debug_file_count=debug_file_count-1 WHERE id=$1 RETURNING debug_file_revision")
            .bind(project.get()).bind(file.size as i64).fetch_one(&mut *tx).await.map_err(error)?;
        let revision = u64::try_from(revision).map_err(|_| DebugFileStoreError::InvalidData)?;
        tx.commit().await.map_err(error)?;
        Ok(Some((file, revision)))
    }
    async fn recoverable(&self, limit: usize) -> Result<Vec<DebugUploadRecord>> {
        if !(1..=1000).contains(&limit) {
            return Err(DebugFileStoreError::InvalidData);
        }
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query.push(UPLOAD_COLUMNS).push(" FROM metric.debug_uploads u WHERE state IN(0,1) AND EXISTS(SELECT 1 FROM metric.projects p WHERE p.id=u.project_id AND p.state=0) ORDER BY updated_at,upload_id LIMIT ").push_bind(limit as i64);
        query
            .build()
            .fetch_all(&self.database.pool)
            .await
            .map_err(error)?
            .iter()
            .map(decode_upload)
            .collect()
    }
}
impl DebugFileStore for PostgresDebugFileStore {
    fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<OrganizationId>> {
        Box::pin(async move {
            let id: Option<i64> = sqlx::query_scalar(
                "SELECT organization_id FROM metric.projects WHERE id=$1 AND state=0",
            )
            .bind(project_id.get())
            .fetch_optional(&self.database.pool)
            .await
            .map_err(error)?;
            organization(id.ok_or(DebugFileStoreError::NotFound)?)
        })
    }
    fn resolve_project_slugs(
        &self,
        organization_slug: Box<str>,
        project_slug: Box<str>,
    ) -> PortFuture<'_, Result<(OrganizationId, ProjectView)>> {
        Box::pin(async move {
            if organization_slug.len() > 63 || project_slug.len() > 63 {
                return Err(DebugFileStoreError::NotFound);
            }
            let row = sqlx::query("SELECT p.* FROM metric.projects p JOIN metric.organizations o ON o.id=p.organization_id WHERE o.slug=$1 AND p.slug=$2 AND p.state=0")
                .bind(organization_slug.as_ref()).bind(project_slug.as_ref()).fetch_optional(&self.database.pool).await.map_err(error)?.ok_or(DebugFileStoreError::NotFound)?;
            let project = crate::projects::decode_project(&row)
                .map_err(|_| DebugFileStoreError::InvalidData)?;
            Ok((project.organization_id, project))
        })
    }
    fn load_by_sha1(
        &self,
        project_id: ProjectId,
        sha1: [u8; 20],
    ) -> PortFuture<'_, Result<Option<DebugFile>>> {
        Box::pin(self.by_sha1(project_id, sha1))
    }
    fn upsert_upload(&self, upload: DebugUpload) -> PortFuture<'_, Result<DebugUploadRecord>> {
        Box::pin(self.upsert(upload))
    }
    fn set_upload_state(
        &self,
        upload_id: [u8; 16],
        state: DebugUploadState,
        now: Timestamp,
        error_code: Option<Box<str>>,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(self.set_state(upload_id, state, now, error_code))
    }
    fn publish_debug_file(
        &self,
        upload_id: [u8; 16],
        file: DebugFile,
    ) -> PortFuture<'_, Result<u64>> {
        Box::pin(self.publish(upload_id, file))
    }
    fn find_debug_files(
        &self,
        project_id: ProjectId,
        debug_id: Option<DebugId>,
        code_id: Option<CodeId>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<DebugFile>>> {
        Box::pin(self.find(project_id, debug_id, code_id, limit))
    }
    fn load_debug_file(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> PortFuture<'_, Result<DebugFile>> {
        Box::pin(self.load(project_id, file_id))
    }
    fn delete_debug_file(
        &self,
        project_id: ProjectId,
        file_id: DebugFileId,
    ) -> PortFuture<'_, Result<Option<(DebugFile, u64)>>> {
        Box::pin(self.delete(project_id, file_id))
    }
    fn recoverable_uploads(&self, limit: usize) -> PortFuture<'_, Result<Vec<DebugUploadRecord>>> {
        Box::pin(self.recoverable(limit))
    }
}
async fn lock_project(
    connection: &mut PgConnection,
    project: ProjectId,
    exclusive: bool,
) -> Result<PgRow> {
    let query = if exclusive {
        "SELECT * FROM metric.projects WHERE id=$1 AND state=0 FOR UPDATE"
    } else {
        "SELECT * FROM metric.projects WHERE id=$1 AND state=0 FOR SHARE"
    };
    sqlx::query(query)
        .bind(project.get())
        .fetch_optional(connection)
        .await
        .map_err(error)?
        .ok_or(DebugFileStoreError::NotFound)
}
async fn load_upload(connection: &mut PgConnection, id: [u8; 16]) -> Result<DebugUploadRecord> {
    let mut query = QueryBuilder::<Postgres>::new("SELECT ");
    query
        .push(UPLOAD_COLUMNS)
        .push(" FROM metric.debug_uploads WHERE upload_id=")
        .push_bind(id.to_vec())
        .push(" FOR UPDATE");
    let row = query
        .build()
        .fetch_optional(connection)
        .await
        .map_err(error)?
        .ok_or(DebugFileStoreError::NotFound)?;
    decode_upload(&row)
}
fn decode_file(row: &PgRow) -> Result<DebugFile> {
    revision(row)?;
    let file = DebugFile {
        id: DebugFileId::from_bytes(fixed(row, "file_id")?),
        project_id: project(row)?,
        debug_id: get::<Option<Vec<u8>>>(row, "debug_id")?
            .map(|bytes| DebugId::decode(&bytes).map_err(|_| DebugFileStoreError::InvalidData))
            .transpose()?,
        code_id: get::<Option<Vec<u8>>>(row, "code_id")?
            .map(|bytes| CodeId::decode(&bytes).map_err(|_| DebugFileStoreError::InvalidData))
            .transpose()?,
        file_type: match get::<i16>(row, "file_type")? {
            0 => DebugFileType::Elf,
            1 => DebugFileType::MachO,
            2 => DebugFileType::Pe,
            3 => DebugFileType::Pdb,
            4 => DebugFileType::PortablePdb,
            5 => DebugFileType::Breakpad,
            6 => DebugFileType::SourceBundle,
            _ => return Err(DebugFileStoreError::InvalidData),
        },
        checksum: fixed(row, "checksum")?,
        sha1: fixed(row, "sha1")?,
        size: unsigned(row, "size")?,
        name: stored_name(row)?,
        uploaded_at: timestamp(row, "uploaded_at")?,
    };
    if file.debug_id.is_none() && file.code_id.is_none() {
        return Err(DebugFileStoreError::InvalidData);
    }
    Ok(file)
}
fn decode_upload(row: &PgRow) -> Result<DebugUploadRecord> {
    revision(row)?;
    let packed: Vec<u8> = get(row, "chunks")?;
    if packed.is_empty()
        || packed.len() > MAX_DEBUG_CHUNKS * 20
        || !packed.len().is_multiple_of(20)
        || get::<Option<i32>>(row, "error_length")?.is_some_and(|length| length > 64)
    {
        return Err(DebugFileStoreError::InvalidData);
    }
    let code: Option<String> = get(row, "error_code")?;
    if code
        .as_ref()
        .is_some_and(|value| value.chars().any(char::is_control))
    {
        return Err(DebugFileStoreError::InvalidData);
    }
    Ok(DebugUploadRecord {
        upload: DebugUpload {
            id: fixed(row, "upload_id")?,
            project_id: project(row)?,
            organization_id: organization(get(row, "organization_id")?)?,
            sha1: fixed(row, "sha1")?,
            name: stored_name(row)?,
            debug_id: get::<Option<Vec<u8>>>(row, "debug_id")?
                .map(|bytes| DebugId::decode(&bytes).map_err(|_| DebugFileStoreError::InvalidData))
                .transpose()?,
            code_id: get::<Option<Vec<u8>>>(row, "code_id")?
                .map(|bytes| CodeId::decode(&bytes).map_err(|_| DebugFileStoreError::InvalidData))
                .transpose()?,
            chunks: packed
                .chunks_exact(20)
                .map(|chunk| chunk.try_into().expect("exact SHA-1 chunk"))
                .collect(),
            created_at: timestamp(row, "created_at")?,
            updated_at: timestamp(row, "updated_at")?,
        },
        state: match get::<i16>(row, "state")? {
            0 => DebugUploadState::Pending,
            1 => DebugUploadState::Assembling,
            2 => DebugUploadState::Complete,
            3 => DebugUploadState::Failed,
            _ => return Err(DebugFileStoreError::InvalidData),
        },
        attempts: u32::try_from(get::<i64>(row, "attempts")?)
            .map_err(|_| DebugFileStoreError::InvalidData)?,
        error_code: code.map(Into::into),
    })
}
fn get<T>(row: &PgRow, name: &str) -> Result<T>
where
    T: for<'a> sqlx::Decode<'a, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get(name)
        .map_err(|_| DebugFileStoreError::InvalidData)
}
fn fixed<const N: usize>(row: &PgRow, name: &str) -> Result<[u8; N]> {
    get::<Vec<u8>>(row, name)?
        .try_into()
        .map_err(|_| DebugFileStoreError::InvalidData)
}
fn unsigned(row: &PgRow, name: &str) -> Result<u64> {
    u64::try_from(get::<i64>(row, name)?).map_err(|_| DebugFileStoreError::InvalidData)
}
fn project(row: &PgRow) -> Result<ProjectId> {
    ProjectId::new(get(row, "project_id")?).map_err(|_| DebugFileStoreError::InvalidData)
}
fn organization(value: i64) -> Result<OrganizationId> {
    OrganizationId::new(u64::try_from(value).map_err(|_| DebugFileStoreError::InvalidData)?)
        .map_err(|_| DebugFileStoreError::InvalidData)
}
fn timestamp(row: &PgRow, name: &str) -> Result<Timestamp> {
    Timestamp::from_unix_millis(get(row, name)?).map_err(|_| DebugFileStoreError::InvalidData)
}
fn revision(row: &PgRow) -> Result<()> {
    if get::<i16>(row, "codec_revision")? != 1 {
        return Err(DebugFileStoreError::InvalidData);
    }
    Ok(())
}
fn name(value: &str) -> Result<Box<str>> {
    validate_debug_name(value).map_err(|_| DebugFileStoreError::InvalidData)
}
fn stored_name(row: &PgRow) -> Result<Box<str>> {
    let value: String = get(row, "name")?;
    let normalized = name(&value)?;
    if normalized.as_ref() != value {
        return Err(DebugFileStoreError::InvalidData);
    }
    Ok(normalized)
}
fn state_code(state: DebugUploadState) -> i16 {
    match state {
        DebugUploadState::Pending => 0,
        DebugUploadState::Assembling => 1,
        DebugUploadState::Complete => 2,
        DebugUploadState::Failed => 3,
    }
}
fn error(value: sqlx::Error) -> DebugFileStoreError {
    match value
        .as_database_error()
        .and_then(|error| error.code())
        .as_deref()
    {
        Some("23505") => DebugFileStoreError::Conflict,
        Some("23503") => DebugFileStoreError::NotFound,
        Some("23514" | "22003" | "22021") => DebugFileStoreError::InvalidData,
        _ => DebugFileStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
