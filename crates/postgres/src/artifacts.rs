//! Organization-deduplicated metadata; application/BlobStore retain file ownership.
use crate::{PostgresDatabase, PostgresError};
use metric_domain::{
    OrganizationId, ProjectId, Timestamp, api::ProjectView, artifacts::*, debug_files::DebugId,
    finalization::ReleaseId,
};
use metric_ports::{ArtifactStore, ArtifactStoreError, PortFuture};
use sqlx::{PgConnection, Postgres, QueryBuilder, Row, postgres::PgRow};
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, ArtifactStoreError>;
const BUNDLE_COLUMNS: &str = "bundle_id,organization_id,codec_revision,sha1,CASE WHEN octet_length(bundle_debug_id)<=20 THEN bundle_debug_id END AS bundle_debug_id,CASE WHEN cardinality(debug_tokens)<=20000 THEN debug_tokens END AS debug_tokens,checksum,size,uploaded_at,generation,state,due_at,claim";
const UPLOAD_COLUMNS: &str = "upload_id,organization_id,codec_revision,sha1,CASE WHEN octet_length(chunks)<=1280 THEN chunks END AS chunks,state,attempts,final_id,error_code,created_at,updated_at";
const GC_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
#[derive(Debug, Clone, Copy, Default)]
pub struct ArtifactQuota {
    /// Zero retains the existing unlimited-quota convention.
    pub maximum_bytes_per_organization: u64,
    pub maximum_bundles_per_organization: u64,
}
#[derive(Clone)]
pub struct PostgresArtifactStore {
    database: PostgresDatabase,
    quota: ArtifactQuota,
}
impl PostgresArtifactStore {
    pub fn new(
        database: PostgresDatabase,
        quota: ArtifactQuota,
    ) -> std::result::Result<Self, PostgresError> {
        if i64::try_from(quota.maximum_bytes_per_organization).is_err()
            || i64::try_from(quota.maximum_bundles_per_organization).is_err()
        {
            return Err(PostgresError::InvalidPolicy);
        }
        Ok(Self { database, quota })
    }
    async fn resolve(
        &self,
        slug: Box<str>,
        slugs: Vec<Box<str>>,
    ) -> Result<(OrganizationId, Vec<ProjectView>)> {
        if slugs.is_empty() || slugs.len() > MAX_ARTIFACT_BINDINGS {
            return Err(ArtifactStoreError::InvalidData);
        }
        if slug.len() > 63 || slugs.iter().any(|slug| slug.len() > 63) {
            return Err(ArtifactStoreError::NotFound);
        }
        let slugs: Vec<String> = slugs.into_iter().map(Into::into).collect();
        let rows=sqlx::query("SELECT p.* FROM metric.projects p JOIN metric.organizations o ON o.id=p.organization_id WHERE o.slug=$1 AND p.slug=ANY($2) AND p.state=0 ORDER BY p.id LIMIT $3")
            .bind(slug.as_ref()).bind(&slugs).bind(MAX_ARTIFACT_BINDINGS as i64).fetch_all(&self.database.pool).await.map_err(error)?;
        if rows.len() != slugs.len() {
            return Err(ArtifactStoreError::NotFound);
        }
        let projects: Vec<ProjectView> = rows
            .iter()
            .map(|row| {
                crate::projects::decode_project(row).map_err(|_| ArtifactStoreError::InvalidData)
            })
            .collect::<Result<_>>()?;
        Ok((projects[0].organization_id, projects))
    }
    async fn merge_upload(&self, mut upload: ArtifactUpload) -> Result<ArtifactUploadRecord> {
        upload.bindings = canonical(upload.bindings, false)?;
        if !(1..=MAX_ARTIFACT_CHUNKS).contains(&upload.chunks.len()) {
            return Err(ArtifactStoreError::InvalidData);
        }
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        lock_projects(
            &mut tx,
            upload.organization_id,
            &upload.bindings,
            &[],
            false,
        )
        .await?;
        let chunks: Vec<u8> = upload
            .chunks
            .iter()
            .flat_map(|chunk| chunk.iter().copied())
            .collect();
        sqlx::query("INSERT INTO metric.artifact_uploads(upload_id,organization_id,sha1,chunks,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(upload_id) DO NOTHING")
            .bind(upload.id.as_slice()).bind(upload.organization_id.get() as i64).bind(upload.sha1.as_slice()).bind(chunks).bind(upload.created_at.unix_millis()).bind(upload.updated_at.unix_millis()).execute(&mut *tx).await.map_err(error)?;
        let row = upload_row(&mut tx, upload.id).await?;
        let mut record = decode_upload(&row, Vec::new())?;
        if record.upload.organization_id != upload.organization_id
            || record.upload.sha1 != upload.sha1
            || record.upload.chunks != upload.chunks
        {
            return Err(ArtifactStoreError::Conflict);
        }
        let existing = bindings(
            &mut tx,
            BindingOwner::Upload,
            upload.organization_id,
            upload.id,
        )
        .await?;
        let merged = canonical(
            existing
                .iter()
                .cloned()
                .chain(upload.bindings.iter().cloned())
                .collect(),
            false,
        )?;
        for binding in merged.iter().filter(|binding| !existing.contains(binding)) {
            insert_binding(
                &mut tx,
                BindingOwner::Upload,
                upload.organization_id,
                upload.id,
                binding,
            )
            .await?;
        }
        if existing != merged {
            sqlx::query("UPDATE metric.artifact_uploads SET updated_at=$2 WHERE upload_id=$1")
                .bind(upload.id.as_slice())
                .bind(upload.updated_at.unix_millis())
                .execute(&mut *tx)
                .await
                .map_err(error)?;
            record.upload.updated_at = upload.updated_at;
        }
        record.upload.bindings = merged;
        tx.commit().await.map_err(error)?;
        Ok(record)
    }
    async fn update_upload(
        &self,
        id: [u8; 16],
        state: ArtifactUploadState,
        now: Timestamp,
        final_id: Option<ArtifactBundleId>,
        code: Option<u16>,
    ) -> Result<()> {
        if code == Some(0) {
            return Err(ArtifactStoreError::InvalidData);
        }
        let expires = expiry(state, now)?;
        let result=sqlx::query("UPDATE metric.artifact_uploads SET state=$2,updated_at=$3,final_id=COALESCE($4,final_id),error_code=$5,expire_at=$6,attempts=attempts+$7 WHERE upload_id=$1")
            .bind(id.as_slice()).bind(upload_state(state)).bind(now.unix_millis()).bind(final_id.map(|id|id.as_bytes().to_vec()))
            .bind(if state==ArtifactUploadState::Pending {None} else {code.map(i32::from)}).bind(expires).bind(i64::from(state==ArtifactUploadState::Assembling))
            .execute(&self.database.pool).await.map_err(error)?;
        if result.rows_affected() != 1 {
            return Err(ArtifactStoreError::NotFound);
        }
        Ok(())
    }
    async fn generation(
        &self,
        org: OrganizationId,
        sha: [u8; 20],
        until: Timestamp,
    ) -> Result<u32> {
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        lock_organization(&mut tx, org).await?;
        let row=sqlx::query("SELECT bundle_id,generation,state FROM metric.artifact_bundles WHERE organization_id=$1 AND sha1=$2 FOR UPDATE").bind(org.get() as i64).bind(sha.as_slice()).fetch_optional(&mut *tx).await.map_err(error)?;
        let generation = if let Some(row) = row {
            let generation = as_u32(&row, "generation")?;
            match get::<i16>(&row, "state")? {
                0 | 3 => generation,
                1 => return Err(ArtifactStoreError::Busy),
                2 => {
                    let next = generation
                        .checked_add(1)
                        .ok_or(ArtifactStoreError::InvalidData)?;
                    sqlx::query("UPDATE metric.artifact_bundles SET state=3,generation=$3,due_at=$4 WHERE bundle_id=$1 AND organization_id=$2")
                        .bind(fixed::<16>(&row,"bundle_id")?.as_slice()).bind(org.get() as i64).bind(i64::from(next)).bind(until.unix_millis()).execute(&mut *tx).await.map_err(error)?;
                    next
                }
                _ => return Err(ArtifactStoreError::InvalidData),
            }
        } else {
            0
        };
        tx.commit().await.map_err(error)?;
        Ok(generation)
    }
    async fn publish(
        &self,
        id: [u8; 16],
        mut bundle: ArtifactBundle,
    ) -> Result<Vec<(ProjectId, u64)>> {
        bundle.bindings = canonical(bundle.bindings, false)?;
        if bundle.debug_id_tokens.len() > MAX_ARTIFACT_DEBUG_IDS {
            return Err(ArtifactStoreError::InvalidData);
        }
        // Start over only when a project was bound between the read of the bound
        // projects and the bundle lock.
        for _ in 0..16 {
            if let Some(revisions) = self.publish_attempt(id, &bundle).await? {
                return Ok(revisions);
            }
        }
        Err(ArtifactStoreError::Busy)
    }
    async fn publish_attempt(
        &self,
        id: [u8; 16],
        bundle: &ArtifactBundle,
    ) -> Result<Option<Vec<(ProjectId, u64)>>> {
        let size = i64::try_from(bundle.size).map_err(|_| ArtifactStoreError::InvalidData)?;
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        // A refreshed upload time advances the revisions of the projects already bound,
        // so they are locked with the ones being bound, before the organization.
        let bound = bindings(
            &mut tx,
            BindingOwner::Bundle,
            bundle.organization_id,
            bundle.id.as_bytes(),
        )
        .await?;
        let locked = lock_projects(
            &mut tx,
            bundle.organization_id,
            &bundle.bindings,
            &bound,
            true,
        )
        .await?;
        let organization = lock_organization(&mut tx, bundle.organization_id).await?;
        let upload = decode_upload(&upload_row(&mut tx, id).await?, Vec::new())?;
        if upload.upload.organization_id != bundle.organization_id
            || upload.upload.sha1 != bundle.sha1
        {
            return Err(ArtifactStoreError::Conflict);
        }
        let existing = bundle_by_sha(&mut tx, bundle.organization_id, bundle.sha1, true).await?;
        let mut reserve = false;
        let mut refreshed = false;
        let before = if let Some(row) = existing.as_ref() {
            if fixed::<16>(row, "bundle_id")? != bundle.id.as_bytes()
                || fixed::<32>(row, "checksum")? != bundle.checksum
                || as_u32(row, "generation")? != bundle.generation
            {
                return Err(ArtifactStoreError::Conflict);
            }
            match get::<i16>(row, "state")? {
                0 => {
                    refreshed = timestamp(row, "uploaded_at")? < bundle.uploaded_at;
                    bindings(
                        &mut tx,
                        BindingOwner::Bundle,
                        bundle.organization_id,
                        bundle.id.as_bytes(),
                    )
                    .await?
                }
                1 | 2 => return Err(ArtifactStoreError::Busy),
                3 => {
                    reserve = true;
                    Vec::new()
                }
                _ => return Err(ArtifactStoreError::InvalidData),
            }
        } else {
            if bundle.generation != 0 {
                return Err(ArtifactStoreError::Conflict);
            }
            reserve = true;
            Vec::new()
        };
        // Lookups take the newest uploads first: a time that moves on can change them
        // for every project already bound.
        let mut affected: BTreeSet<ProjectId> = if refreshed {
            before.iter().map(|binding| binding.project_id).collect()
        } else {
            BTreeSet::new()
        };
        if !affected.is_subset(&locked) {
            return Ok(None);
        }
        let merged = canonical(
            before
                .iter()
                .cloned()
                .chain(bundle.bindings.iter().cloned())
                .collect(),
            false,
        )?;
        if reserve {
            let bytes = unsigned(&organization, "artifact_bytes")?;
            let count = unsigned(&organization, "artifact_count")?;
            let max_bytes = if self.quota.maximum_bytes_per_organization == 0 {
                i64::MAX as u64
            } else {
                self.quota.maximum_bytes_per_organization
            };
            let max_count = if self.quota.maximum_bundles_per_organization == 0 {
                i64::MAX as u64
            } else {
                self.quota.maximum_bundles_per_organization
            };
            if bytes
                .checked_add(bundle.size)
                .is_none_or(|total| total > max_bytes)
                || count >= max_count
            {
                return Err(ArtifactStoreError::Quota);
            }
            sqlx::query("UPDATE metric.organizations SET artifact_bytes=artifact_bytes+$2,artifact_count=artifact_count+1 WHERE id=$1").bind(bundle.organization_id.get() as i64).bind(size).execute(&mut *tx).await.map_err(error)?;
        }
        let tokens: Vec<i64> = bundle
            .debug_id_tokens
            .iter()
            .map(|token| token.stored())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if existing.is_some() {
            if reserve {
                sqlx::query("UPDATE metric.artifact_bundles SET state=0,due_at=NULL,claim=NULL,deletion_operation=NULL,bundle_debug_id=$3,debug_tokens=$4,checksum=$5,size=$6,uploaded_at=$7 WHERE organization_id=$1 AND bundle_id=$2")
                    .bind(bundle.organization_id.get() as i64).bind(bundle.id.as_bytes().as_slice()).bind(bundle.bundle_debug_id.encode().into_vec()).bind(tokens).bind(bundle.checksum.as_slice()).bind(size).bind(bundle.uploaded_at.unix_millis()).execute(&mut *tx).await.map_err(error)?;
            } else {
                // A new upload of stored content refreshes its upload time, never backwards.
                sqlx::query("UPDATE metric.artifact_bundles SET due_at=NULL,deletion_operation=NULL,uploaded_at=GREATEST(uploaded_at,$3) WHERE organization_id=$1 AND bundle_id=$2").bind(bundle.organization_id.get() as i64).bind(bundle.id.as_bytes().as_slice()).bind(bundle.uploaded_at.unix_millis()).execute(&mut *tx).await.map_err(error)?;
            }
        } else {
            sqlx::query("INSERT INTO metric.artifact_bundles(bundle_id,organization_id,sha1,bundle_debug_id,debug_tokens,checksum,size,uploaded_at,generation) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)")
                .bind(bundle.id.as_bytes().as_slice()).bind(bundle.organization_id.get() as i64).bind(bundle.sha1.as_slice()).bind(bundle.bundle_debug_id.encode().into_vec()).bind(tokens).bind(bundle.checksum.as_slice()).bind(size).bind(bundle.uploaded_at.unix_millis()).bind(i64::from(bundle.generation)).execute(&mut *tx).await.map_err(error)?;
        }
        for binding in merged.iter().filter(|binding| !before.contains(binding)) {
            insert_binding(
                &mut tx,
                BindingOwner::Bundle,
                bundle.organization_id,
                bundle.id.as_bytes(),
                binding,
            )
            .await?;
            affected.insert(binding.project_id);
        }
        let revisions = bump_revisions(&mut tx, &affected).await?;
        sqlx::query("UPDATE metric.artifact_uploads SET state=2,final_id=$2,error_code=NULL,updated_at=$3,expire_at=$4 WHERE upload_id=$1 AND organization_id=$5")
            .bind(id.as_slice()).bind(bundle.id.as_bytes().as_slice()).bind(bundle.uploaded_at.unix_millis()).bind(expiry(ArtifactUploadState::Complete,bundle.uploaded_at)?).bind(bundle.organization_id.get() as i64).execute(&mut *tx).await.map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(Some(revisions))
    }
    async fn by_sha(&self, org: OrganizationId, sha: [u8; 20]) -> Result<Option<ArtifactBundle>> {
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        readonly(&mut tx).await?;
        let row = bundle_by_sha(&mut tx, org, sha, false).await?;
        let Some(row) = row else {
            return Ok(None);
        };
        if get::<i16>(&row, "state")? != 0 {
            return Ok(None);
        }
        let bundle = decode_bundle(
            &row,
            bindings(
                &mut tx,
                BindingOwner::Bundle,
                org,
                fixed(&row, "bundle_id")?,
            )
            .await?,
        )?;
        tx.commit().await.map_err(error)?;
        Ok(Some(bundle))
    }
    async fn lookup_inner(&self, request: ArtifactLookup) -> Result<Vec<ArtifactCandidate>> {
        if !(1..=20).contains(&request.limit) || request.debug_ids.len() > MAX_ARTIFACT_DEBUG_IDS {
            return Err(ArtifactStoreError::InvalidData);
        }
        if request.dist.as_ref().is_some_and(|value| {
            value.is_empty()
                || value.len() > MAX_ARTIFACT_DIST_BYTES
                || value.chars().any(char::is_control)
        }) {
            return Err(ArtifactStoreError::InvalidData);
        }
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        readonly(&mut tx).await?;
        let mut result = BTreeMap::new();
        for debug in [true, false] {
            if debug && request.debug_ids.is_empty()
                || !debug && (request.release_id.is_none() || result.len() == request.limit)
            {
                continue;
            }
            let mut query = QueryBuilder::<Postgres>::new("SELECT ");
            query
                .push(BUNDLE_COLUMNS)
                .push(" FROM metric.artifact_bundles b WHERE organization_id=")
                .push_bind(request.organization_id.get() as i64)
                .push(" AND state=0");
            if debug {
                let tokens: Vec<i64> = request
                    .debug_ids
                    .iter()
                    .map(|id| ArtifactDebugIdToken::derive(request.organization_id, id).stored())
                    .collect();
                query.push(" AND debug_tokens && ").push_bind(tokens);
            }
            query.push(" AND EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id AND r.project_id=").push_bind(request.project_id.get());
            if !debug {
                query
                    .push(" AND r.release_id=")
                    .push_bind(
                        request
                            .release_id
                            .expect("release branch checked")
                            .as_bytes()
                            .to_vec(),
                    )
                    .push(" AND r.dist IS NOT DISTINCT FROM ")
                    .push_bind(request.dist.as_deref());
            }
            query
                .push(") ORDER BY uploaded_at DESC,bundle_id DESC LIMIT ")
                .push_bind(request.limit as i64);
            let rows = query.build().fetch_all(&mut *tx).await.map_err(error)?;
            for row in rows {
                let id = fixed::<16>(&row, "bundle_id")?;
                let bundle = decode_bundle(
                    &row,
                    bindings(&mut tx, BindingOwner::Bundle, request.organization_id, id).await?,
                )?;
                result.entry(bundle.id).or_insert(ArtifactCandidate {
                    bundle,
                    resolved_with: if debug {
                        ArtifactResolution::DebugId
                    } else {
                        ArtifactResolution::Release
                    },
                });
                if result.len() == request.limit {
                    break;
                }
            }
        }
        tx.commit().await.map_err(error)?;
        Ok(result.into_values().collect())
    }
    async fn load_project(
        &self,
        project: ProjectId,
        id: ArtifactBundleId,
    ) -> Result<ArtifactBundle> {
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        readonly(&mut tx).await?;
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query.push(BUNDLE_COLUMNS).push(" FROM metric.artifact_bundles b WHERE bundle_id=").push_bind(id.as_bytes().to_vec())
            .push(" AND state=0 AND EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id AND r.project_id=").push_bind(project.get()).push(")");
        let row = query
            .build()
            .fetch_optional(&mut *tx)
            .await
            .map_err(error)?
            .ok_or(ArtifactStoreError::NotFound)?;
        let org = organization(&row)?;
        let bundle = decode_bundle(
            &row,
            bindings(&mut tx, BindingOwner::Bundle, org, id.as_bytes()).await?,
        )?;
        tx.commit().await.map_err(error)?;
        Ok(bundle)
    }
    async fn bound(&self) -> Result<Vec<(OrganizationId, ProjectId)>> {
        // Active and disabled projects; deletion owns the bindings of the others.
        let rows: Vec<(i64, i32)> = sqlx::query_as("SELECT p.organization_id,p.id FROM metric.projects p WHERE p.state IN(0,1) AND EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.project_id=p.id AND r.organization_id=p.organization_id) ORDER BY p.id")
            .fetch_all(&self.database.pool)
            .await
            .map_err(error)?;
        rows.into_iter()
            .map(|(org, id)| {
                Ok((
                    organization_id(org)?,
                    ProjectId::new(id).map_err(|_| ArtifactStoreError::InvalidData)?,
                ))
            })
            .collect()
    }
    async fn project_page(
        &self,
        org: OrganizationId,
        project: ProjectId,
        before: Option<(Timestamp, ArtifactBundleId)>,
        limit: usize,
    ) -> Result<Vec<ArtifactBundle>> {
        if !(1..=100).contains(&limit) {
            return Err(ArtifactStoreError::InvalidData);
        }
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        readonly(&mut tx).await?;
        // The organization lets artifact_organization_timeline give the order and the cursor.
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query.push(BUNDLE_COLUMNS).push(" FROM metric.artifact_bundles b WHERE organization_id=").push_bind(org.get() as i64)
            .push(" AND state=0 AND EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id AND r.project_id=").push_bind(project.get()).push(")");
        if let Some((uploaded_at, id)) = before {
            query
                .push(" AND (uploaded_at,bundle_id)<(")
                .push_bind(uploaded_at.unix_millis())
                .push(",")
                .push_bind(id.as_bytes().to_vec())
                .push(")");
        }
        query
            .push(" ORDER BY uploaded_at DESC,bundle_id DESC LIMIT ")
            .push_bind(limit as i64);
        let rows = query.build().fetch_all(&mut *tx).await.map_err(error)?;
        let mut bundles = Vec::with_capacity(rows.len());
        for row in rows {
            let id = fixed::<16>(&row, "bundle_id")?;
            bundles.push(decode_bundle(
                &row,
                bindings(&mut tx, BindingOwner::Bundle, org, id).await?,
            )?);
        }
        tx.commit().await.map_err(error)?;
        Ok(bundles)
    }
    async fn remove(
        &self,
        org: OrganizationId,
        id: ArtifactBundleId,
        binding: ArtifactBinding,
        orphan: Timestamp,
        uploaded_at: Option<Timestamp>,
    ) -> Result<Option<u64>> {
        canonical(vec![binding.clone()], false)?;
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        // Unlike publication, removal also serves a disabled project (retention).
        sqlx::query("SELECT id FROM metric.projects WHERE organization_id=$1 AND id=$2 AND state IN(0,1) FOR UPDATE")
            .bind(org.get() as i64)
            .bind(binding.project_id.get())
            .fetch_optional(&mut *tx)
            .await
            .map_err(error)?
            .ok_or(ArtifactStoreError::NotFound)?;
        lock_organization(&mut tx, org).await?;
        let row=sqlx::query("SELECT sha1,state,uploaded_at FROM metric.artifact_bundles WHERE organization_id=$1 AND bundle_id=$2 FOR UPDATE").bind(org.get() as i64).bind(id.as_bytes().as_slice()).fetch_optional(&mut *tx).await.map_err(error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        if get::<i16>(&row, "state")? != 0 {
            return Ok(None);
        }
        // A newer publication of the same content refreshed the upload time: it keeps its
        // binding.
        if let Some(expected) = uploaded_at
            && timestamp(&row, "uploaded_at")? != expected
        {
            return Ok(None);
        }
        if !delete_binding(&mut tx, BindingOwner::Bundle, org, id.as_bytes(), &binding).await? {
            return Ok(None);
        }
        let remaining = bindings(&mut tx, BindingOwner::Bundle, org, id.as_bytes()).await?;
        if remaining.is_empty() {
            sqlx::query("UPDATE metric.artifact_bundles SET due_at=$3 WHERE organization_id=$1 AND bundle_id=$2").bind(org.get() as i64).bind(id.as_bytes().as_slice()).bind(orphan.unix_millis()).execute(&mut *tx).await.map_err(error)?;
        }
        let upload:Option<Vec<u8>>=sqlx::query_scalar("SELECT upload_id FROM metric.artifact_uploads WHERE organization_id=$1 AND sha1=$2 FOR UPDATE").bind(org.get() as i64).bind(fixed::<20>(&row,"sha1")?.as_slice()).fetch_optional(&mut *tx).await.map_err(error)?;
        if let Some(upload) = upload {
            let upload: [u8; 16] = upload
                .try_into()
                .map_err(|_| ArtifactStoreError::InvalidData)?;
            delete_binding(&mut tx, BindingOwner::Upload, org, upload, &binding).await?;
            if bindings(&mut tx, BindingOwner::Upload, org, upload)
                .await?
                .is_empty()
            {
                sqlx::query(
                    "DELETE FROM metric.artifact_uploads WHERE upload_id=$1 AND organization_id=$2",
                )
                .bind(upload.as_slice())
                .bind(org.get() as i64)
                .execute(&mut *tx)
                .await
                .map_err(error)?;
            }
        }
        let revisions = bump_revisions(&mut tx, &BTreeSet::from([binding.project_id])).await?;
        tx.commit().await.map_err(error)?;
        Ok(revisions.first().map(|(_, revision)| *revision))
    }
    async fn recover(&self, limit: usize) -> Result<Vec<ArtifactUploadRecord>> {
        if !(1..=1000).contains(&limit) {
            return Err(ArtifactStoreError::InvalidData);
        }
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        readonly(&mut tx).await?;
        let mut query = QueryBuilder::<Postgres>::new("SELECT ");
        query.push(UPLOAD_COLUMNS).push(" FROM metric.artifact_uploads u WHERE state IN(0,1) AND EXISTS(SELECT 1 FROM metric.artifact_upload_bindings r JOIN metric.projects p ON p.id=r.project_id AND p.organization_id=r.organization_id WHERE r.organization_id=u.organization_id AND r.upload_id=u.upload_id AND p.state=0) ORDER BY updated_at,upload_id LIMIT ").push_bind(limit as i64);
        let rows = query.build().fetch_all(&mut *tx).await.map_err(error)?;
        let mut result = Vec::new();
        let mut bytes = 0;
        for row in rows {
            let record = decode_upload(
                &row,
                bindings(
                    &mut tx,
                    BindingOwner::Upload,
                    organization(&row)?,
                    fixed(&row, "upload_id")?,
                )
                .await?,
            )?;
            bytes += record.upload.chunks.len() * 20 + record.upload.bindings.len() * 104 + 128;
            if bytes > GC_RESPONSE_BYTES && !result.is_empty() {
                break;
            }
            result.push(record);
        }
        tx.commit().await.map_err(error)?;
        Ok(result)
    }
    async fn claim(
        &self,
        now: Timestamp,
        until: Timestamp,
        claim: [u8; 16],
        limit: usize,
    ) -> Result<Vec<ArtifactGcClaim>> {
        if !(1..=100).contains(&limit) || until <= now {
            return Err(ArtifactStoreError::InvalidData);
        }
        let candidates=sqlx::query("SELECT bundle_id,organization_id FROM metric.artifact_bundles b WHERE state IN(0,1) AND due_at<=$1 AND NOT EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id) ORDER BY due_at,bundle_id LIMIT $2")
            .bind(now.unix_millis()).bind(limit as i64).fetch_all(&self.database.pool).await.map_err(error)?;
        let orgs: Vec<i64> = candidates
            .iter()
            .map(|row| get::<i64>(row, "organization_id"))
            .collect::<Result<BTreeSet<_>>>()?
            .into_iter()
            .collect();
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        sqlx::query("SELECT id FROM metric.organizations WHERE id=ANY($1) ORDER BY id FOR UPDATE")
            .bind(&orgs)
            .fetch_all(&mut *tx)
            .await
            .map_err(error)?;
        let mut result = Vec::new();
        let mut bytes = 0;
        for candidate in candidates {
            let id = fixed::<16>(&candidate, "bundle_id")?;
            let org = organization(&candidate)?;
            let mut query = QueryBuilder::<Postgres>::new("SELECT ");
            query.push(BUNDLE_COLUMNS).push(" FROM metric.artifact_bundles b WHERE bundle_id=").push_bind(id.to_vec()).push(" AND organization_id=").push_bind(org.get() as i64)
                .push(" AND state IN(0,1) AND due_at<=").push_bind(now.unix_millis()).push(" AND NOT EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id) FOR UPDATE SKIP LOCKED");
            let Some(row) = query
                .build()
                .fetch_optional(&mut *tx)
                .await
                .map_err(error)?
            else {
                continue;
            };
            let bundle = decode_bundle(&row, Vec::new())?;
            bytes += bundle.debug_id_tokens.len() * 8 + 256;
            if bytes > GC_RESPONSE_BYTES && !result.is_empty() {
                break;
            }
            sqlx::query("UPDATE metric.artifact_bundles SET state=1,claim=$3,due_at=$4 WHERE bundle_id=$1 AND organization_id=$2").bind(id.as_slice()).bind(org.get() as i64).bind(claim.as_slice()).bind(until.unix_millis()).execute(&mut *tx).await.map_err(error)?;
            result.push(ArtifactGcClaim {
                bundle,
                claim,
                lease_until: until,
            });
        }
        tx.commit().await.map_err(error)?;
        Ok(result)
    }
    async fn finish(
        &self,
        id: ArtifactBundleId,
        generation: u32,
        claim: [u8; 16],
        until: Timestamp,
    ) -> Result<bool> {
        let org: Option<i64> = sqlx::query_scalar(
            "SELECT organization_id FROM metric.artifact_bundles WHERE bundle_id=$1",
        )
        .bind(id.as_bytes().as_slice())
        .fetch_optional(&self.database.pool)
        .await
        .map_err(error)?;
        let Some(org) = org else {
            return Ok(false);
        };
        let org = organization_id(org)?;
        let mut tx = self.database.pool.begin().await.map_err(error)?;
        let owner = lock_organization(&mut tx, org).await?;
        let row=sqlx::query("SELECT size FROM metric.artifact_bundles b WHERE bundle_id=$1 AND organization_id=$2 AND generation=$3 AND claim=$4 AND state=1 AND NOT EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id) FOR UPDATE")
            .bind(id.as_bytes().as_slice()).bind(org.get() as i64).bind(i64::from(generation)).bind(claim.as_slice()).fetch_optional(&mut *tx).await.map_err(error)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let size = unsigned(&row, "size")?;
        if unsigned(&owner, "artifact_count")? == 0 || unsigned(&owner, "artifact_bytes")? < size {
            return Err(ArtifactStoreError::InvalidData);
        }
        sqlx::query("UPDATE metric.organizations SET artifact_count=artifact_count-1,artifact_bytes=artifact_bytes-$2 WHERE id=$1").bind(org.get() as i64).bind(size as i64).execute(&mut *tx).await.map_err(error)?;
        sqlx::query(
            "UPDATE metric.artifact_bundles SET state=2,claim=NULL,deletion_operation=NULL,due_at=$2 WHERE bundle_id=$1",
        )
        .bind(id.as_bytes().as_slice())
        .bind(until.unix_millis())
        .execute(&mut *tx)
        .await
        .map_err(error)?;
        tx.commit().await.map_err(error)?;
        Ok(true)
    }
}
impl ArtifactStore for PostgresArtifactStore {
    fn resolve_projects(
        &self,
        organization_slug: Box<str>,
        project_slugs: Vec<Box<str>>,
    ) -> PortFuture<'_, Result<(OrganizationId, Vec<ProjectView>)>> {
        Box::pin(self.resolve(organization_slug, project_slugs))
    }
    fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<OrganizationId>> {
        Box::pin(async move {
            let org: Option<i64> = sqlx::query_scalar(
                "SELECT organization_id FROM metric.projects WHERE id=$1 AND state=0",
            )
            .bind(project_id.get())
            .fetch_optional(&self.database.pool)
            .await
            .map_err(error)?;
            organization_id(org.ok_or(ArtifactStoreError::NotFound)?)
        })
    }
    fn load_by_sha1(
        &self,
        organization_id: OrganizationId,
        sha1: [u8; 20],
    ) -> PortFuture<'_, Result<Option<ArtifactBundle>>> {
        Box::pin(self.by_sha(organization_id, sha1))
    }
    fn upsert_upload(
        &self,
        upload: ArtifactUpload,
    ) -> PortFuture<'_, Result<ArtifactUploadRecord>> {
        Box::pin(self.merge_upload(upload))
    }
    fn set_upload_state(
        &self,
        upload_id: [u8; 16],
        state: ArtifactUploadState,
        now: Timestamp,
        final_id: Option<ArtifactBundleId>,
        error_code: Option<u16>,
    ) -> PortFuture<'_, Result<()>> {
        Box::pin(self.update_upload(upload_id, state, now, final_id, error_code))
    }
    fn publication_generation(
        &self,
        organization_id: OrganizationId,
        sha1: [u8; 20],
        _upload_id: [u8; 16],
        reservation_until: Timestamp,
    ) -> PortFuture<'_, Result<u32>> {
        Box::pin(self.generation(organization_id, sha1, reservation_until))
    }
    fn publish_bundle(
        &self,
        upload_id: [u8; 16],
        bundle: ArtifactBundle,
    ) -> PortFuture<'_, Result<Vec<(ProjectId, u64)>>> {
        Box::pin(self.publish(upload_id, bundle))
    }
    fn lookup(&self, request: ArtifactLookup) -> PortFuture<'_, Result<Vec<ArtifactCandidate>>> {
        Box::pin(self.lookup_inner(request))
    }
    fn load_for_project(
        &self,
        project_id: ProjectId,
        bundle_id: ArtifactBundleId,
    ) -> PortFuture<'_, Result<ArtifactBundle>> {
        Box::pin(self.load_project(project_id, bundle_id))
    }
    fn bound_projects(&self) -> PortFuture<'_, Result<Vec<(OrganizationId, ProjectId)>>> {
        Box::pin(self.bound())
    }
    fn project_bundles(
        &self,
        organization_id: OrganizationId,
        project_id: ProjectId,
        before: Option<(Timestamp, ArtifactBundleId)>,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ArtifactBundle>>> {
        Box::pin(self.project_page(organization_id, project_id, before, limit))
    }
    fn remove_binding(
        &self,
        organization_id: OrganizationId,
        bundle_id: ArtifactBundleId,
        binding: ArtifactBinding,
        orphan_at: Timestamp,
        uploaded_at: Option<Timestamp>,
    ) -> PortFuture<'_, Result<Option<u64>>> {
        Box::pin(self.remove(organization_id, bundle_id, binding, orphan_at, uploaded_at))
    }
    fn recoverable_uploads(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ArtifactUploadRecord>>> {
        Box::pin(self.recover(limit))
    }
    fn claim_gc(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        claim: [u8; 16],
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ArtifactGcClaim>>> {
        Box::pin(self.claim(now, lease_until, claim, limit))
    }
    fn validate_gc_claim(
        &self,
        bundle_id: ArtifactBundleId,
        generation: u32,
        claim: [u8; 16],
        minimum_lease_until: Timestamp,
    ) -> PortFuture<'_, Result<bool>> {
        Box::pin(async move {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metric.artifact_bundles b WHERE bundle_id=$1 AND generation=$2 AND claim=$3 AND state=1 AND due_at>$4 AND NOT EXISTS(SELECT 1 FROM metric.artifact_bundle_bindings r WHERE r.organization_id=b.organization_id AND r.bundle_id=b.bundle_id))")
            .bind(bundle_id.as_bytes().as_slice()).bind(i64::from(generation)).bind(claim.as_slice()).bind(minimum_lease_until.unix_millis()).fetch_one(&self.database.pool).await.map_err(error)
        })
    }
    fn finish_gc(
        &self,
        bundle_id: ArtifactBundleId,
        generation: u32,
        claim: [u8; 16],
        tombstone_until: Timestamp,
    ) -> PortFuture<'_, Result<bool>> {
        Box::pin(self.finish(bundle_id, generation, claim, tombstone_until))
    }
}

#[derive(Clone, Copy)]
enum BindingOwner {
    Upload,
    Bundle,
}
impl BindingOwner {
    fn table(self) -> &'static str {
        match self {
            Self::Upload => "metric.artifact_upload_bindings",
            Self::Bundle => "metric.artifact_bundle_bindings",
        }
    }
    fn column(self) -> &'static str {
        match self {
            Self::Upload => "upload_id",
            Self::Bundle => "bundle_id",
        }
    }
}
async fn bindings(
    connection: &mut PgConnection,
    owner: BindingOwner,
    org: OrganizationId,
    id: [u8; 16],
) -> Result<Vec<ArtifactBinding>> {
    let mut query = QueryBuilder::<Postgres>::new("SELECT project_id,release_id,dist FROM ");
    query
        .push(owner.table())
        .push(" WHERE organization_id=")
        .push_bind(org.get() as i64)
        .push(" AND ")
        .push(owner.column())
        .push("=")
        .push_bind(id.to_vec())
        .push(" ORDER BY project_id,release_id NULLS FIRST,dist NULLS FIRST LIMIT ")
        .push_bind((MAX_ARTIFACT_BINDINGS + 1) as i64);
    let rows = query.build().fetch_all(connection).await.map_err(error)?;
    if rows.len() > MAX_ARTIFACT_BINDINGS {
        return Err(ArtifactStoreError::InvalidData);
    }
    let values = rows
        .iter()
        .map(|row| {
            ArtifactBinding::new(
                ProjectId::new(get(row, "project_id")?)
                    .map_err(|_| ArtifactStoreError::InvalidData)?,
                get::<Option<Vec<u8>>>(row, "release_id")?
                    .map(|bytes| {
                        bytes
                            .try_into()
                            .map(ReleaseId::from_bytes)
                            .map_err(|_| ArtifactStoreError::InvalidData)
                    })
                    .transpose()?,
                get::<Option<String>>(row, "dist")?.map(Into::into),
            )
            .map_err(|_| ArtifactStoreError::InvalidData)
        })
        .collect::<Result<Vec<_>>>()?;
    canonical(values, true)
}
async fn insert_binding(
    connection: &mut PgConnection,
    owner: BindingOwner,
    org: OrganizationId,
    id: [u8; 16],
    binding: &ArtifactBinding,
) -> Result<()> {
    let mut query = QueryBuilder::<Postgres>::new("INSERT INTO ");
    query
        .push(owner.table())
        .push("(organization_id,")
        .push(owner.column())
        .push(",project_id,release_id,dist) VALUES(")
        .push_bind(org.get() as i64)
        .push(",")
        .push_bind(id.to_vec())
        .push(",")
        .push_bind(binding.project_id.get())
        .push(",")
        .push_bind(binding.release_id.map(|id| id.as_bytes().to_vec()))
        .push(",")
        .push_bind(binding.dist.as_deref())
        .push(") ON CONFLICT DO NOTHING");
    query.build().execute(connection).await.map_err(error)?;
    Ok(())
}
async fn delete_binding(
    connection: &mut PgConnection,
    owner: BindingOwner,
    org: OrganizationId,
    id: [u8; 16],
    binding: &ArtifactBinding,
) -> Result<bool> {
    let mut query = QueryBuilder::<Postgres>::new("DELETE FROM ");
    query
        .push(owner.table())
        .push(" WHERE organization_id=")
        .push_bind(org.get() as i64)
        .push(" AND ")
        .push(owner.column())
        .push("=")
        .push_bind(id.to_vec())
        .push(" AND project_id=")
        .push_bind(binding.project_id.get())
        .push(" AND release_id IS NOT DISTINCT FROM ")
        .push_bind(binding.release_id.map(|id| id.as_bytes().to_vec()))
        .push(" AND dist IS NOT DISTINCT FROM ")
        .push_bind(binding.dist.as_deref());
    Ok(query
        .build()
        .execute(connection)
        .await
        .map_err(error)?
        .rows_affected()
        == 1)
}
/// Locks projects in one id-ordered pass, before the organization, as every transaction
/// that locks both does. The projects of `bindings` must be active; those of `bound`,
/// already bound to stored content, may be in any state.
async fn lock_projects(
    connection: &mut PgConnection,
    org: OrganizationId,
    bindings: &[ArtifactBinding],
    bound: &[ArtifactBinding],
    exclusive: bool,
) -> Result<BTreeSet<ProjectId>> {
    let required: BTreeSet<i32> = bindings
        .iter()
        .map(|binding| binding.project_id.get())
        .collect();
    let ids: Vec<i32> = required
        .iter()
        .copied()
        .chain(bound.iter().map(|binding| binding.project_id.get()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let query = if exclusive {
        "SELECT id,state FROM metric.projects WHERE organization_id=$1 AND id=ANY($2) ORDER BY id FOR UPDATE"
    } else {
        "SELECT id,state FROM metric.projects WHERE organization_id=$1 AND id=ANY($2) ORDER BY id FOR SHARE"
    };
    let rows: Vec<(i32, i16)> = sqlx::query_as(query)
        .bind(org.get() as i64)
        .bind(&ids)
        .fetch_all(connection)
        .await
        .map_err(error)?;
    if required.iter().any(|id| !rows.contains(&(*id, 0))) {
        return Err(ArtifactStoreError::NotFound);
    }
    rows.into_iter()
        .map(|(id, _)| ProjectId::new(id).map_err(|_| ArtifactStoreError::InvalidData))
        .collect()
}
async fn lock_organization(connection: &mut PgConnection, org: OrganizationId) -> Result<PgRow> {
    sqlx::query("SELECT * FROM metric.organizations WHERE id=$1 FOR UPDATE")
        .bind(org.get() as i64)
        .fetch_optional(connection)
        .await
        .map_err(error)?
        .ok_or(ArtifactStoreError::NotFound)
}
async fn bump_revisions(
    connection: &mut PgConnection,
    projects: &BTreeSet<ProjectId>,
) -> Result<Vec<(ProjectId, u64)>> {
    let mut result = Vec::new();
    for project in projects {
        let row=sqlx::query("UPDATE metric.projects SET artifact_revision=artifact_revision+1 WHERE id=$1 RETURNING artifact_revision").bind(project.get()).fetch_one(&mut *connection).await.map_err(error)?;
        result.push((*project, unsigned(&row, "artifact_revision")?));
    }
    Ok(result)
}
async fn upload_row(connection: &mut PgConnection, id: [u8; 16]) -> Result<PgRow> {
    let mut query = QueryBuilder::<Postgres>::new("SELECT ");
    query
        .push(UPLOAD_COLUMNS)
        .push(" FROM metric.artifact_uploads WHERE upload_id=")
        .push_bind(id.to_vec())
        .push(" FOR UPDATE");
    query
        .build()
        .fetch_optional(connection)
        .await
        .map_err(error)?
        .ok_or(ArtifactStoreError::NotFound)
}
async fn bundle_by_sha(
    connection: &mut PgConnection,
    org: OrganizationId,
    sha: [u8; 20],
    lock: bool,
) -> Result<Option<PgRow>> {
    let mut query = QueryBuilder::<Postgres>::new("SELECT ");
    query
        .push(BUNDLE_COLUMNS)
        .push(" FROM metric.artifact_bundles WHERE organization_id=")
        .push_bind(org.get() as i64)
        .push(" AND sha1=")
        .push_bind(sha.to_vec());
    if lock {
        query.push(" FOR UPDATE");
    }
    query
        .build()
        .fetch_optional(connection)
        .await
        .map_err(error)
}
async fn readonly(connection: &mut PgConnection) -> Result<()> {
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(connection)
        .await
        .map_err(error)?;
    Ok(())
}
fn canonical(mut values: Vec<ArtifactBinding>, empty: bool) -> Result<Vec<ArtifactBinding>> {
    if values.len() > MAX_ARTIFACT_BINDINGS * 2 {
        return Err(ArtifactStoreError::InvalidData);
    }
    for binding in &values {
        ArtifactBinding::new(binding.project_id, binding.release_id, binding.dist.clone())
            .map_err(|_| ArtifactStoreError::InvalidData)?;
    }
    values.sort();
    values.dedup();
    if (!empty && values.is_empty()) || values.len() > MAX_ARTIFACT_BINDINGS {
        return Err(ArtifactStoreError::InvalidData);
    }
    Ok(values)
}
fn decode_bundle(row: &PgRow, bindings: Vec<ArtifactBinding>) -> Result<ArtifactBundle> {
    revision(row)?;
    let mut tokens: Vec<i64> = get(row, "debug_tokens")?;
    if tokens.len() > MAX_ARTIFACT_DEBUG_IDS {
        return Err(ArtifactStoreError::InvalidData);
    }
    tokens.sort();
    tokens.dedup();
    Ok(ArtifactBundle {
        id: ArtifactBundleId::from_bytes(fixed(row, "bundle_id")?),
        organization_id: organization(row)?,
        bindings,
        bundle_debug_id: DebugId::decode(&get::<Vec<u8>>(row, "bundle_debug_id")?)
            .map_err(|_| ArtifactStoreError::InvalidData)?,
        debug_id_tokens: tokens
            .into_iter()
            .map(ArtifactDebugIdToken::from_stored)
            .collect(),
        checksum: fixed(row, "checksum")?,
        sha1: fixed(row, "sha1")?,
        size: unsigned(row, "size")?,
        uploaded_at: timestamp(row, "uploaded_at")?,
        generation: as_u32(row, "generation")?,
    })
}
fn decode_upload(row: &PgRow, bindings: Vec<ArtifactBinding>) -> Result<ArtifactUploadRecord> {
    revision(row)?;
    let chunks: Vec<u8> = get(row, "chunks")?;
    if chunks.is_empty()
        || chunks.len() > MAX_ARTIFACT_CHUNKS * 20
        || !chunks.len().is_multiple_of(20)
    {
        return Err(ArtifactStoreError::InvalidData);
    }
    Ok(ArtifactUploadRecord {
        upload: ArtifactUpload {
            id: fixed(row, "upload_id")?,
            organization_id: organization(row)?,
            sha1: fixed(row, "sha1")?,
            chunks: chunks
                .chunks_exact(20)
                .map(|chunk| chunk.try_into().expect("exact SHA1 chunk"))
                .collect(),
            bindings,
            created_at: timestamp(row, "created_at")?,
            updated_at: timestamp(row, "updated_at")?,
        },
        state: match get::<i16>(row, "state")? {
            0 => ArtifactUploadState::Pending,
            1 => ArtifactUploadState::Assembling,
            2 => ArtifactUploadState::Complete,
            3 => ArtifactUploadState::Failed,
            _ => return Err(ArtifactStoreError::InvalidData),
        },
        attempts: as_u32(row, "attempts")?,
        final_id: get::<Option<Vec<u8>>>(row, "final_id")?
            .map(|bytes| {
                bytes
                    .try_into()
                    .map(ArtifactBundleId::from_bytes)
                    .map_err(|_| ArtifactStoreError::InvalidData)
            })
            .transpose()?,
        error_code: get::<Option<i32>>(row, "error_code")?
            .map(|code| u16::try_from(code).map_err(|_| ArtifactStoreError::InvalidData))
            .transpose()?,
    })
}
fn get<T>(row: &PgRow, name: &str) -> Result<T>
where
    T: for<'a> sqlx::Decode<'a, Postgres> + sqlx::Type<Postgres>,
{
    row.try_get(name)
        .map_err(|_| ArtifactStoreError::InvalidData)
}
fn fixed<const N: usize>(row: &PgRow, name: &str) -> Result<[u8; N]> {
    get::<Vec<u8>>(row, name)?
        .try_into()
        .map_err(|_| ArtifactStoreError::InvalidData)
}
fn unsigned(row: &PgRow, name: &str) -> Result<u64> {
    u64::try_from(get::<i64>(row, name)?).map_err(|_| ArtifactStoreError::InvalidData)
}
fn as_u32(row: &PgRow, name: &str) -> Result<u32> {
    u32::try_from(get::<i64>(row, name)?).map_err(|_| ArtifactStoreError::InvalidData)
}
fn organization(row: &PgRow) -> Result<OrganizationId> {
    organization_id(get(row, "organization_id")?)
}
fn organization_id(value: i64) -> Result<OrganizationId> {
    OrganizationId::new(u64::try_from(value).map_err(|_| ArtifactStoreError::InvalidData)?)
        .map_err(|_| ArtifactStoreError::InvalidData)
}
fn timestamp(row: &PgRow, name: &str) -> Result<Timestamp> {
    Timestamp::from_unix_millis(get(row, name)?).map_err(|_| ArtifactStoreError::InvalidData)
}
fn revision(row: &PgRow) -> Result<()> {
    if get::<i16>(row, "codec_revision")? != 1 {
        return Err(ArtifactStoreError::InvalidData);
    }
    Ok(())
}
fn upload_state(state: ArtifactUploadState) -> i16 {
    match state {
        ArtifactUploadState::Pending => 0,
        ArtifactUploadState::Assembling => 1,
        ArtifactUploadState::Complete => 2,
        ArtifactUploadState::Failed => 3,
    }
}
fn expiry(state: ArtifactUploadState, now: Timestamp) -> Result<Option<i64>> {
    let days = match state {
        ArtifactUploadState::Complete => 1,
        ArtifactUploadState::Failed => 7,
        _ => return Ok(None),
    };
    let value = now
        .unix_millis()
        .checked_add(days * 86_400_000)
        .ok_or(ArtifactStoreError::InvalidData)?;
    Timestamp::from_unix_millis(value).map_err(|_| ArtifactStoreError::InvalidData)?;
    Ok(Some(value))
}
fn error(value: sqlx::Error) -> ArtifactStoreError {
    match value
        .as_database_error()
        .and_then(|error| error.code())
        .as_deref()
    {
        Some("23505") => ArtifactStoreError::Conflict,
        Some("23503") => ArtifactStoreError::NotFound,
        Some("23514" | "22003" | "22021") => ArtifactStoreError::InvalidData,
        _ => ArtifactStoreError::Unavailable,
    }
}

#[cfg(test)]
mod tests;
