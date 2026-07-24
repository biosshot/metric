use std::collections::{BTreeMap, BTreeSet};

use faultkeep_domain::{
    OrganizationId, ProjectId, Timestamp,
    api::ProjectView,
    artifacts::{
        ArtifactBinding, ArtifactBundle, ArtifactBundleId, ArtifactCandidate, ArtifactDebugIdToken,
        ArtifactGcClaim, ArtifactLookup, ArtifactResolution, ArtifactUpload, ArtifactUploadRecord,
        ArtifactUploadState, MAX_ARTIFACT_BINDINGS, MAX_ARTIFACT_CHUNKS, MAX_ARTIFACT_DEBUG_IDS,
    },
    debug_files::DebugId,
    finalization::ReleaseId,
};
use faultkeep_ports::{ArtifactStore, ArtifactStoreError, PortFuture};
use futures_util::TryStreamExt;
use mongodb::{
    Database, IndexModel,
    bson::{Binary, Bson, DateTime, Document, doc, spec::BinarySubtype},
    options::{IndexOptions, ReturnDocument},
};

use crate::decode_project_view;

#[derive(Debug, Clone, Copy, Default)]
pub struct ArtifactQuota {
    pub maximum_bytes_per_organization: u64,
    pub maximum_bundles_per_organization: u64,
}

#[derive(Clone)]
pub struct MongoArtifactStore {
    database: Database,
    quota: ArtifactQuota,
}

impl MongoArtifactStore {
    #[must_use]
    pub fn from_database(database: Database, quota: ArtifactQuota) -> Self {
        Self { database, quota }
    }

    async fn resolve(
        &self,
        organization_slug: &str,
        project_slugs: &[Box<str>],
    ) -> Result<(OrganizationId, Vec<ProjectView>), ArtifactStoreError> {
        if project_slugs.is_empty() || project_slugs.len() > MAX_ARTIFACT_BINDINGS {
            return Err(ArtifactStoreError::InvalidData);
        }
        let organizations = self.database.collection::<Document>("organizations");
        let organization = organizations
            .find_one(doc! { "slug": organization_slug })
            .await
            .map_err(unavailable)?
            .ok_or(ArtifactStoreError::NotFound)?;
        let organization_id = OrganizationId::new(
            u64::try_from(
                organization
                    .get_i64("_id")
                    .map_err(|_| ArtifactStoreError::InvalidData)?,
            )
            .map_err(|_| ArtifactStoreError::InvalidData)?,
        )
        .map_err(|_| ArtifactStoreError::InvalidData)?;
        let slugs = project_slugs.iter().map(AsRef::as_ref).collect::<Vec<_>>();
        let mut cursor = self
            .database
            .collection::<Document>("projects")
            .find(doc! {
                "organization_id": i64::try_from(organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?,
                "slug": { "$in": &slugs },
                "state": { "$in": ["active", "disabled"] },
            })
            .await
            .map_err(unavailable)?;
        let mut by_slug = BTreeMap::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let slug = document
                .get_str("slug")
                .map_err(|_| ArtifactStoreError::InvalidData)?
                .to_owned();
            by_slug.insert(
                slug,
                decode_project_view(&document).map_err(|_| ArtifactStoreError::InvalidData)?,
            );
        }
        let mut projects = Vec::with_capacity(project_slugs.len());
        for slug in project_slugs {
            let project = by_slug
                .remove(slug.as_ref())
                .ok_or(ArtifactStoreError::NotFound)?;
            projects.push(project);
        }
        projects.sort_by_key(|project| project.id);
        projects.dedup_by_key(|project| project.id);
        Ok((organization_id, projects))
    }

    async fn raw_by_sha1(
        &self,
        organization_id: OrganizationId,
        sha1: [u8; 20],
    ) -> Result<Option<Document>, ArtifactStoreError> {
        self.database
            .collection::<Document>("artifact_bundles")
            .find_one(doc! {
                "o": i64::try_from(organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?,
                "h": binary(&sha1),
            })
            .await
            .map_err(unavailable)
    }

    async fn merge_upload(
        &self,
        upload: ArtifactUpload,
    ) -> Result<ArtifactUploadRecord, ArtifactStoreError> {
        let collection = self.database.collection::<Document>("artifact_uploads");
        collection
            .update_one(
                doc! { "_id": binary(&upload.id) },
                doc! { "$setOnInsert": encode_upload(&upload)? },
            )
            .upsert(true)
            .await
            .map_err(unavailable)?;
        for _ in 0..16 {
            let document = collection
                .find_one(doc! { "_id": binary(&upload.id) })
                .await
                .map_err(unavailable)?
                .ok_or(ArtifactStoreError::Unavailable)?;
            let record = decode_upload(&document)?;
            if record.upload.organization_id != upload.organization_id
                || record.upload.sha1 != upload.sha1
                || record.upload.chunks != upload.chunks
            {
                return Err(ArtifactStoreError::Conflict);
            }
            let bindings = canonical_bindings(
                record
                    .upload
                    .bindings
                    .iter()
                    .cloned()
                    .chain(upload.bindings.iter().cloned()),
            )?;
            if bindings == record.upload.bindings {
                return Ok(record);
            }
            let result = collection
                .update_one(
                    doc! { "_id": binary(&upload.id), "b": encode_bindings(&record.upload.bindings)? },
                    doc! { "$set": { "b": encode_bindings(&bindings)?, "u": datetime(upload.updated_at) } },
                )
                .await
                .map_err(unavailable)?;
            if result.modified_count == 1 {
                return collection
                    .find_one(doc! { "_id": binary(&upload.id) })
                    .await
                    .map_err(unavailable)?
                    .ok_or(ArtifactStoreError::Unavailable)
                    .and_then(|document| decode_upload(&document));
            }
        }
        Err(ArtifactStoreError::Busy)
    }

    async fn update_upload(
        &self,
        upload_id: [u8; 16],
        state: ArtifactUploadState,
        now: Timestamp,
        final_id: Option<ArtifactBundleId>,
        error_code: Option<u16>,
    ) -> Result<(), ArtifactStoreError> {
        let mut set = doc! { "u": datetime(now) };
        let mut unset = Document::new();
        match upload_state_code(state) {
            None => {
                unset.insert("s", "");
                unset.insert("e", "");
                unset.insert("q", "");
            }
            Some(code) => {
                set.insert("s", code);
                if matches!(
                    state,
                    ArtifactUploadState::Complete | ArtifactUploadState::Failed
                ) {
                    set.insert(
                        "e",
                        datetime(add_duration(
                            now,
                            if state == ArtifactUploadState::Complete {
                                std::time::Duration::from_secs(24 * 60 * 60)
                            } else {
                                std::time::Duration::from_secs(7 * 24 * 60 * 60)
                            },
                        )?),
                    );
                }
            }
        }
        if let Some(id) = final_id {
            set.insert("f", binary(&id.as_bytes()));
        }
        if let Some(code) = error_code {
            set.insert("q", i32::from(code));
        }
        let mut update = doc! { "$set": set };
        if !unset.is_empty() {
            update.insert("$unset", unset);
        }
        if state == ArtifactUploadState::Assembling {
            update.insert("$inc", doc! { "a": 1_i32 });
        }
        let result = self
            .database
            .collection::<Document>("artifact_uploads")
            .update_one(doc! { "_id": binary(&upload_id) }, update)
            .await
            .map_err(unavailable)?;
        (result.matched_count == 1)
            .then_some(())
            .ok_or(ArtifactStoreError::NotFound)
    }

    async fn generation(
        &self,
        organization_id: OrganizationId,
        sha1: [u8; 20],
        reservation_until: Timestamp,
    ) -> Result<u32, ArtifactStoreError> {
        let Some(document) = self.raw_by_sha1(organization_id, sha1).await? else {
            return Ok(0);
        };
        let generation = stored_generation(&document)?;
        match document.get_i32("s") {
            Err(_) if !document.contains_key("s") => Ok(generation),
            Ok(1) => Err(ArtifactStoreError::Busy),
            Ok(2) => {
                let next = generation
                    .checked_add(1)
                    .ok_or(ArtifactStoreError::InvalidData)?;
                let result = self
                    .database
                    .collection::<Document>("artifact_bundles")
                    .find_one_and_update(
                        doc! { "_id": document.get("_id").cloned().ok_or(ArtifactStoreError::InvalidData)?, "s": 2_i32, "v": generation_filter(generation) },
                        doc! { "$set": { "s": 3_i32, "v": i32::try_from(next).map_err(|_| ArtifactStoreError::InvalidData)?, "e": datetime(reservation_until) } },
                    )
                    .return_document(ReturnDocument::After)
                    .await
                    .map_err(unavailable)?;
                result.map_or(Err(ArtifactStoreError::Busy), |_| Ok(next))
            }
            Ok(3) => Ok(generation),
            _ => Err(ArtifactStoreError::InvalidData),
        }
    }

    async fn publish(
        &self,
        upload_id: [u8; 16],
        mut bundle: ArtifactBundle,
    ) -> Result<Vec<(ProjectId, u64)>, ArtifactStoreError> {
        bundle.bindings = canonical_bindings(bundle.bindings)?;
        let collection = self.database.collection::<Document>("artifact_bundles");
        if let Some(document) = self
            .raw_by_sha1(bundle.organization_id, bundle.sha1)
            .await?
        {
            validate_content_identity(&document, &bundle)?;
            let state = document.get_i32("s").ok();
            if state == Some(1) || state == Some(2) {
                return Err(ArtifactStoreError::Busy);
            }
            let existing = if state == Some(3) {
                Vec::new()
            } else {
                decode_bindings(&document)?
            };
            let merged = canonical_bindings(
                existing
                    .iter()
                    .cloned()
                    .chain(bundle.bindings.iter().cloned()),
            )?;
            let affected = affected_projects(&existing, &merged);
            bundle.bindings = merged;
            let filter = if state == Some(3) {
                doc! { "_id": binary(&bundle.id.as_bytes()), "s": 3_i32, "v": i32::try_from(bundle.generation).map_err(|_| ArtifactStoreError::InvalidData)? }
            } else {
                doc! { "_id": binary(&bundle.id.as_bytes()), "s": { "$exists": false } }
            };
            if state == Some(3) {
                self.reserve_quota(bundle.organization_id, bundle.size)
                    .await?;
            }
            let result = collection
                .replace_one(filter, encode_bundle(&bundle)?)
                .await
                .map_err(|_| ArtifactStoreError::Unavailable);
            if result.is_err() && state == Some(3) {
                let _ = self
                    .release_quota(bundle.organization_id, bundle.size)
                    .await;
            }
            let result = result?;
            if result.matched_count != 1 {
                if state == Some(3) {
                    let _ = self
                        .release_quota(bundle.organization_id, bundle.size)
                        .await;
                }
                return Err(ArtifactStoreError::Busy);
            }
            let revisions = self.bump_revisions(&affected).await?;
            self.update_upload(
                upload_id,
                ArtifactUploadState::Complete,
                bundle.uploaded_at,
                Some(bundle.id),
                None,
            )
            .await?;
            return Ok(revisions);
        }

        self.reserve_quota(bundle.organization_id, bundle.size)
            .await?;
        if let Err(error) = collection.insert_one(encode_bundle(&bundle)?).await {
            let _ = self
                .release_quota(bundle.organization_id, bundle.size)
                .await;
            if error.to_string().contains("E11000") {
                return Err(ArtifactStoreError::Conflict);
            }
            return Err(ArtifactStoreError::Unavailable);
        }
        let affected = bundle
            .bindings
            .iter()
            .map(|binding| binding.project_id)
            .collect::<BTreeSet<_>>();
        let revisions = self.bump_revisions(&affected).await?;
        self.update_upload(
            upload_id,
            ArtifactUploadState::Complete,
            bundle.uploaded_at,
            Some(bundle.id),
            None,
        )
        .await?;
        Ok(revisions)
    }

    async fn reserve_quota(
        &self,
        organization_id: OrganizationId,
        size: u64,
    ) -> Result<(), ArtifactStoreError> {
        let size = i64::try_from(size).map_err(|_| ArtifactStoreError::InvalidData)?;
        let max_bytes = if self.quota.maximum_bytes_per_organization == 0 {
            i64::MAX
        } else {
            i64::try_from(self.quota.maximum_bytes_per_organization)
                .map_err(|_| ArtifactStoreError::InvalidData)?
        };
        let max_count = if self.quota.maximum_bundles_per_organization == 0 {
            i64::MAX
        } else {
            i64::try_from(self.quota.maximum_bundles_per_organization)
                .map_err(|_| ArtifactStoreError::InvalidData)?
        };
        self.database
            .collection::<Document>("organizations")
            .find_one_and_update(
                doc! {
                    "_id": i64::try_from(organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?,
                    "$expr": { "$and": [
                        { "$lte": [{ "$add": [{ "$ifNull": ["$ab", 0_i64] }, size] }, max_bytes] },
                        { "$lt": [{ "$ifNull": ["$ac", 0_i64] }, max_count] },
                    ] }
                },
                doc! { "$inc": { "ab": size, "ac": 1_i64 } },
            )
            .await
            .map_err(unavailable)?
            .map(|_| ())
            .ok_or(ArtifactStoreError::Quota)
    }

    async fn release_quota(
        &self,
        organization_id: OrganizationId,
        size: u64,
    ) -> Result<(), ArtifactStoreError> {
        self.database
            .collection::<Document>("organizations")
            .update_one(
                doc! { "_id": i64::try_from(organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)? },
                doc! { "$inc": { "ab": -i64::try_from(size).map_err(|_| ArtifactStoreError::InvalidData)?, "ac": -1_i64 } },
            )
            .await
            .map_err(unavailable)?;
        Ok(())
    }

    async fn bump_revisions(
        &self,
        projects: &BTreeSet<ProjectId>,
    ) -> Result<Vec<(ProjectId, u64)>, ArtifactStoreError> {
        let mut revisions = Vec::with_capacity(projects.len());
        for project_id in projects {
            let document = self
                .database
                .collection::<Document>("projects")
                .find_one_and_update(
                    doc! { "_id": project_id.get() },
                    doc! { "$inc": { "ar": 1_i64 } },
                )
                .return_document(ReturnDocument::After)
                .await
                .map_err(unavailable)?
                .ok_or(ArtifactStoreError::NotFound)?;
            revisions.push((*project_id, artifact_revision(&document)?));
        }
        Ok(revisions)
    }

    async fn lookup_candidates(
        &self,
        request: ArtifactLookup,
    ) -> Result<Vec<ArtifactCandidate>, ArtifactStoreError> {
        if !(1..=20).contains(&request.limit) || request.debug_ids.len() > MAX_ARTIFACT_DEBUG_IDS {
            return Err(ArtifactStoreError::InvalidData);
        }
        let collection = self.database.collection::<Document>("artifact_bundles");
        let mut candidates = BTreeMap::new();
        if !request.debug_ids.is_empty() {
            let tokens = request
                .debug_ids
                .iter()
                .map(|id| ArtifactDebugIdToken::derive(request.organization_id, id).stored())
                .collect::<Vec<_>>();
            let mut cursor = collection
                .find(doc! {
                    "o": i64::try_from(request.organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?,
                    "k": { "$in": tokens },
                    "b": { "$elemMatch": { "p": request.project_id.get() } },
                    "s": { "$exists": false },
                })
                .sort(doc! { "u": -1, "_id": -1 })
                .limit(i64::try_from(request.limit).unwrap_or(20))
                .await
                .map_err(unavailable)?;
            while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
                let bundle = decode_bundle(&document)?;
                candidates.insert(
                    bundle.id,
                    ArtifactCandidate {
                        bundle,
                        resolved_with: ArtifactResolution::DebugId,
                    },
                );
            }
        }
        if candidates.len() < request.limit {
            if let Some(release_id) = request.release_id {
                let mut element =
                    doc! { "p": request.project_id.get(), "r": binary(&release_id.as_bytes()) };
                if let Some(dist) = &request.dist {
                    element.insert("d", dist.as_ref());
                } else {
                    element.insert("d", doc! { "$exists": false });
                }
                let mut cursor = collection
                    .find(doc! {
                        "o": i64::try_from(request.organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?,
                        "b": { "$elemMatch": element },
                        "s": { "$exists": false },
                    })
                    .sort(doc! { "u": -1, "_id": -1 })
                    .limit(i64::try_from(request.limit).unwrap_or(20))
                    .await
                    .map_err(unavailable)?;
                while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
                    let bundle = decode_bundle(&document)?;
                    candidates.entry(bundle.id).or_insert(ArtifactCandidate {
                        bundle,
                        resolved_with: ArtifactResolution::Release,
                    });
                    if candidates.len() == request.limit {
                        break;
                    }
                }
            }
        }
        Ok(candidates.into_values().collect())
    }

    async fn remove(
        &self,
        organization_id: OrganizationId,
        bundle_id: ArtifactBundleId,
        binding: ArtifactBinding,
        orphan_at: Timestamp,
    ) -> Result<Option<u64>, ArtifactStoreError> {
        let collection = self.database.collection::<Document>("artifact_bundles");
        for _ in 0..16 {
            let Some(document) = collection
                .find_one(doc! { "_id": binary(&bundle_id.as_bytes()), "o": i64::try_from(organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?, "s": { "$exists": false } })
                .await
                .map_err(unavailable)?
            else {
                return Ok(None);
            };
            let existing = decode_bindings(&document)?;
            if !existing.contains(&binding) {
                return Ok(None);
            }
            let remaining = existing
                .iter()
                .filter(|candidate| **candidate != binding)
                .cloned()
                .collect::<Vec<_>>();
            let old = encode_bindings(&existing)?;
            let update = if remaining.is_empty() {
                doc! { "$unset": { "b": "" }, "$set": { "e": datetime(orphan_at) } }
            } else {
                doc! { "$set": { "b": encode_bindings(&remaining)? } }
            };
            let result = collection
                .update_one(
                    doc! { "_id": binary(&bundle_id.as_bytes()), "b": old },
                    update,
                )
                .await
                .map_err(unavailable)?;
            if result.modified_count == 1 {
                self.remove_upload_binding(organization_id, fixed(&document, "h")?, &binding)
                    .await?;
                return self
                    .bump_revisions(&BTreeSet::from([binding.project_id]))
                    .await
                    .map(|values| values.first().map(|(_, revision)| *revision));
            }
        }
        Err(ArtifactStoreError::Busy)
    }

    async fn remove_upload_binding(
        &self,
        organization_id: OrganizationId,
        sha1: [u8; 20],
        binding: &ArtifactBinding,
    ) -> Result<(), ArtifactStoreError> {
        let collection = self.database.collection::<Document>("artifact_uploads");
        let Some(document) = collection
            .find_one(doc! {
                "o": i64::try_from(organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?,
                "h": binary(&sha1),
            })
            .await
            .map_err(unavailable)?
        else {
            return Ok(());
        };
        let existing = decode_bindings(&document)?;
        let remaining = existing
            .iter()
            .filter(|candidate| *candidate != binding)
            .cloned()
            .collect::<Vec<_>>();
        if remaining.len() == existing.len() {
            return Ok(());
        }
        let id = document
            .get("_id")
            .cloned()
            .ok_or(ArtifactStoreError::InvalidData)?;
        let old = encode_bindings(&existing)?;
        if remaining.is_empty() {
            collection
                .delete_one(doc! { "_id": id, "b": old })
                .await
                .map_err(unavailable)?;
        } else {
            collection
                .update_one(
                    doc! { "_id": id, "b": old },
                    doc! { "$set": { "b": encode_bindings(&remaining)? } },
                )
                .await
                .map_err(unavailable)?;
        }
        Ok(())
    }

    async fn recoverable(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactUploadRecord>, ArtifactStoreError> {
        if !(1..=1000).contains(&limit) {
            return Err(ArtifactStoreError::InvalidData);
        }
        let mut cursor = self
            .database
            .collection::<Document>("artifact_uploads")
            .find(doc! { "$or": [{ "s": { "$exists": false } }, { "s": 1_i32 }] })
            .sort(doc! { "u": 1, "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(1000))
            .await
            .map_err(unavailable)?;
        let mut records = Vec::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            records.push(decode_upload(&document)?);
        }
        Ok(records)
    }

    async fn claim(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        claim: [u8; 16],
        limit: usize,
    ) -> Result<Vec<ArtifactGcClaim>, ArtifactStoreError> {
        if !(1..=100).contains(&limit) {
            return Err(ArtifactStoreError::InvalidData);
        }
        let collection = self.database.collection::<Document>("artifact_bundles");
        let mut cursor = collection
            .find(doc! {
                "b": { "$exists": false },
                "e": { "$lte": datetime(now) },
                "$or": [{ "s": { "$exists": false } }, { "s": 1_i32 }],
            })
            .sort(doc! { "e": 1, "_id": 1 })
            .limit(i64::try_from(limit).unwrap_or(100))
            .await
            .map_err(unavailable)?;
        let mut claimed = Vec::new();
        while let Some(document) = cursor.try_next().await.map_err(unavailable)? {
            let bundle = decode_bundle(&document)?;
            let result = collection
                .update_one(
                    doc! { "_id": binary(&bundle.id.as_bytes()), "b": { "$exists": false }, "e": { "$lte": datetime(now) }, "$or": [{ "s": { "$exists": false } }, { "s": 1_i32 }] },
                    doc! { "$set": { "s": 1_i32, "c": binary(&claim), "e": datetime(lease_until) } },
                )
                .await
                .map_err(unavailable)?;
            if result.modified_count == 1 {
                claimed.push(ArtifactGcClaim {
                    bundle,
                    claim,
                    lease_until,
                });
            }
        }
        Ok(claimed)
    }

    async fn finish(
        &self,
        bundle_id: ArtifactBundleId,
        generation: u32,
        claim: [u8; 16],
        tombstone_until: Timestamp,
    ) -> Result<bool, ArtifactStoreError> {
        let collection = self.database.collection::<Document>("artifact_bundles");
        let Some(document) = collection
            .find_one(doc! { "_id": binary(&bundle_id.as_bytes()), "s": 1_i32, "c": binary(&claim), "v": generation_filter(generation) })
            .await
            .map_err(unavailable)?
        else {
            return Ok(false);
        };
        let organization = organization_id(&document)?;
        let sha1 = fixed::<20>(&document, "h")?;
        let size = u64::try_from(
            document
                .get_i64("z")
                .map_err(|_| ArtifactStoreError::InvalidData)?,
        )
        .map_err(|_| ArtifactStoreError::InvalidData)?;
        let mut tombstone = doc! {
            "_id": binary(&bundle_id.as_bytes()),
            "o": i64::try_from(organization.get()).map_err(|_| ArtifactStoreError::InvalidData)?,
            "h": binary(&sha1),
            "s": 2_i32,
            "e": datetime(tombstone_until),
        };
        if generation != 0 {
            tombstone.insert(
                "v",
                i32::try_from(generation).map_err(|_| ArtifactStoreError::InvalidData)?,
            );
        }
        let result = collection
            .replace_one(
                doc! { "_id": binary(&bundle_id.as_bytes()), "s": 1_i32, "c": binary(&claim) },
                tombstone,
            )
            .await
            .map_err(unavailable)?;
        if result.modified_count == 1 {
            self.release_quota(organization, size).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl ArtifactStore for MongoArtifactStore {
    fn resolve_projects(
        &self,
        organization_slug: Box<str>,
        project_slugs: Vec<Box<str>>,
    ) -> PortFuture<'_, Result<(OrganizationId, Vec<ProjectView>), ArtifactStoreError>> {
        Box::pin(async move { self.resolve(&organization_slug, &project_slugs).await })
    }

    fn project_organization(
        &self,
        project_id: ProjectId,
    ) -> PortFuture<'_, Result<OrganizationId, ArtifactStoreError>> {
        Box::pin(async move {
            let document = self
                .database
                .collection::<Document>("projects")
                .find_one(doc! { "_id": project_id.get(), "state": { "$ne": "deleted" } })
                .await
                .map_err(unavailable)?
                .ok_or(ArtifactStoreError::NotFound)?;
            organization_id(&document)
        })
    }

    fn load_by_sha1(
        &self,
        organization_id: OrganizationId,
        sha1: [u8; 20],
    ) -> PortFuture<'_, Result<Option<ArtifactBundle>, ArtifactStoreError>> {
        Box::pin(async move {
            self.raw_by_sha1(organization_id, sha1)
                .await?
                .map(|document| decode_bundle(&document))
                .transpose()
        })
    }

    fn upsert_upload(
        &self,
        upload: ArtifactUpload,
    ) -> PortFuture<'_, Result<ArtifactUploadRecord, ArtifactStoreError>> {
        Box::pin(self.merge_upload(upload))
    }

    fn set_upload_state(
        &self,
        upload_id: [u8; 16],
        state: ArtifactUploadState,
        now: Timestamp,
        final_id: Option<ArtifactBundleId>,
        error_code: Option<u16>,
    ) -> PortFuture<'_, Result<(), ArtifactStoreError>> {
        Box::pin(self.update_upload(upload_id, state, now, final_id, error_code))
    }

    fn publication_generation(
        &self,
        organization_id: OrganizationId,
        sha1: [u8; 20],
        _upload_id: [u8; 16],
        reservation_until: Timestamp,
    ) -> PortFuture<'_, Result<u32, ArtifactStoreError>> {
        Box::pin(self.generation(organization_id, sha1, reservation_until))
    }

    fn publish_bundle(
        &self,
        upload_id: [u8; 16],
        bundle: ArtifactBundle,
    ) -> PortFuture<'_, Result<Vec<(ProjectId, u64)>, ArtifactStoreError>> {
        Box::pin(self.publish(upload_id, bundle))
    }
    fn lookup(
        &self,
        request: ArtifactLookup,
    ) -> PortFuture<'_, Result<Vec<ArtifactCandidate>, ArtifactStoreError>> {
        Box::pin(self.lookup_candidates(request))
    }

    fn load_for_project(
        &self,
        project_id: ProjectId,
        bundle_id: ArtifactBundleId,
    ) -> PortFuture<'_, Result<ArtifactBundle, ArtifactStoreError>> {
        Box::pin(async move {
            self.database.collection::<Document>("artifact_bundles")
                .find_one(doc! { "_id": binary(&bundle_id.as_bytes()), "b": { "$elemMatch": { "p": project_id.get() } }, "s": { "$exists": false } })
                .await.map_err(unavailable)?.ok_or(ArtifactStoreError::NotFound).and_then(|document| decode_bundle(&document))
        })
    }

    fn remove_binding(
        &self,
        organization_id: OrganizationId,
        bundle_id: ArtifactBundleId,
        binding: ArtifactBinding,
        orphan_at: Timestamp,
    ) -> PortFuture<'_, Result<Option<u64>, ArtifactStoreError>> {
        Box::pin(self.remove(organization_id, bundle_id, binding, orphan_at))
    }
    fn recoverable_uploads(
        &self,
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ArtifactUploadRecord>, ArtifactStoreError>> {
        Box::pin(self.recoverable(limit))
    }
    fn claim_gc(
        &self,
        now: Timestamp,
        lease_until: Timestamp,
        claim: [u8; 16],
        limit: usize,
    ) -> PortFuture<'_, Result<Vec<ArtifactGcClaim>, ArtifactStoreError>> {
        Box::pin(self.claim(now, lease_until, claim, limit))
    }
    fn validate_gc_claim(
        &self,
        bundle_id: ArtifactBundleId,
        generation: u32,
        claim: [u8; 16],
        minimum_lease_until: Timestamp,
    ) -> PortFuture<'_, Result<bool, ArtifactStoreError>> {
        Box::pin(async move {
            self.database
                .collection::<Document>("artifact_bundles")
                .find_one(doc! {
                    "_id": binary(&bundle_id.as_bytes()),
                    "s": 1_i32,
                    "c": binary(&claim),
                    "v": generation_filter(generation),
                    "e": { "$gt": datetime(minimum_lease_until) },
                })
                .await
                .map(|document| document.is_some())
                .map_err(unavailable)
        })
    }
    fn finish_gc(
        &self,
        bundle_id: ArtifactBundleId,
        generation: u32,
        claim: [u8; 16],
        tombstone_until: Timestamp,
    ) -> PortFuture<'_, Result<bool, ArtifactStoreError>> {
        Box::pin(self.finish(bundle_id, generation, claim, tombstone_until))
    }
}

pub(crate) fn artifact_bundle_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object", "required": ["_id", "o", "h"], "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" }, "o": { "bsonType": "long", "minimum": 1 },
            "b": { "bsonType": "array", "minItems": 1, "maxItems": i32::try_from(MAX_ARTIFACT_BINDINGS).unwrap_or(512) },
            "g": { "bsonType": "binData" }, "k": { "bsonType": "array", "maxItems": i32::try_from(MAX_ARTIFACT_DEBUG_IDS).unwrap_or(20_000) },
            "x": { "bsonType": "binData" }, "h": { "bsonType": "binData" }, "z": { "bsonType": "long", "minimum": 0 },
            "u": { "bsonType": "date" }, "v": { "bsonType": "int", "minimum": 1 }, "s": { "enum": [1, 2, 3] },
            "e": { "bsonType": "date" }, "c": { "bsonType": "binData" }, "j": { "bsonType": "binData" },
        },
        "oneOf": [
            {
                "required": ["b", "g", "k", "x", "z", "u"],
                "not": { "anyOf": [{ "required": ["s"] }, { "required": ["e"] }, { "required": ["c"] }] }
            },
            {
                "required": ["g", "k", "x", "z", "u", "e"],
                "not": { "anyOf": [{ "required": ["b"] }, { "required": ["s"] }, { "required": ["c"] }] }
            },
            {
                "required": ["g", "k", "x", "z", "u", "s", "e", "c"],
                "properties": { "s": { "enum": [1] } },
                "not": { "required": ["b"] }
            },
            {
                "required": ["s", "e"],
                "properties": { "s": { "enum": [2, 3] } },
                "not": { "anyOf": [{ "required": ["b"] }, { "required": ["g"] }, { "required": ["k"] }, { "required": ["x"] }, { "required": ["z"] }, { "required": ["u"] }, { "required": ["c"] }] }
            }
        ]
    }}
}

pub(crate) fn artifact_upload_validator() -> Document {
    doc! { "$jsonSchema": {
        "bsonType": "object", "required": ["_id", "o", "h", "c", "b", "t", "u"], "additionalProperties": false,
        "properties": {
            "_id": { "bsonType": "binData" }, "o": { "bsonType": "long", "minimum": 1 }, "h": { "bsonType": "binData" },
            "c": { "bsonType": "binData" }, "b": { "bsonType": "array", "minItems": 1, "maxItems": i32::try_from(MAX_ARTIFACT_BINDINGS).unwrap_or(512) },
            "s": { "enum": [1, 2, 3] }, "a": { "bsonType": "int", "minimum": 0 }, "r": { "bsonType": "date" },
            "t": { "bsonType": "date" }, "u": { "bsonType": "date" }, "f": { "bsonType": "binData" },
            "e": { "bsonType": "date" }, "q": { "bsonType": "int", "minimum": 1 },
        }
    }}
}

pub(crate) async fn create_artifact_indexes(
    database: &Database,
) -> Result<(), mongodb::error::Error> {
    let bundles = database.collection::<Document>("artifact_bundles");
    for model in [
        index(doc! { "k": 1 }, "artifact_debug_tokens", false, None),
        index(
            doc! { "b.p": 1, "b.r": 1, "b.d": 1, "u": -1, "_id": -1 },
            "artifact_legacy_binding",
            false,
            None,
        ),
        index(
            doc! { "o": 1, "h": 1 },
            "artifact_org_sha1_unique",
            true,
            None,
        ),
        index(
            doc! { "b.p": 1, "u": -1, "_id": -1 },
            "artifact_project_list",
            false,
            None,
        ),
        index(doc! { "e": 1, "_id": 1 }, "artifact_gc_due", false, None),
        index(
            doc! { "s": 1, "e": 1, "_id": 1 },
            "artifact_gc_claims",
            false,
            Some(doc! { "s": 1_i32 }),
        ),
        index(
            doc! { "j": 1, "_id": 1 },
            "artifact_gc_project_delete",
            false,
            Some(doc! { "j": { "$exists": true } }),
        ),
    ] {
        bundles.create_index(model).await?;
    }
    bundles
        .create_index(
            IndexModel::builder()
                .keys(doc! { "e": 1 })
                .options(
                    IndexOptions::builder()
                        .name("artifact_gc_tombstone_expiry".to_owned())
                        .expire_after(std::time::Duration::ZERO)
                        .partial_filter_expression(doc! { "s": 2_i32 })
                        .build(),
                )
                .build(),
        )
        .await?;
    let uploads = database.collection::<Document>("artifact_uploads");
    uploads
        .create_index(index(
            doc! { "s": 1, "r": 1, "_id": 1 },
            "artifact_upload_recovery",
            false,
            None,
        ))
        .await?;
    uploads
        .create_index(
            IndexModel::builder()
                .keys(doc! { "e": 1 })
                .options(
                    IndexOptions::builder()
                        .name("artifact_upload_expiry".to_owned())
                        .expire_after(std::time::Duration::ZERO)
                        .build(),
                )
                .build(),
        )
        .await?;
    Ok(())
}

pub(crate) async fn validate_artifact_indexes(
    database: &Database,
) -> Result<bool, mongodb::error::Error> {
    for (collection, expected) in [
        (
            "artifact_bundles",
            BTreeSet::from(
                [
                    "_id_",
                    "artifact_debug_tokens",
                    "artifact_legacy_binding",
                    "artifact_org_sha1_unique",
                    "artifact_project_list",
                    "artifact_gc_due",
                    "artifact_gc_claims",
                    "artifact_gc_project_delete",
                    "artifact_gc_tombstone_expiry",
                ]
                .map(str::to_owned),
            ),
        ),
        (
            "artifact_uploads",
            BTreeSet::from(
                ["_id_", "artifact_upload_recovery", "artifact_upload_expiry"].map(str::to_owned),
            ),
        ),
    ] {
        let mut cursor = database
            .collection::<Document>(collection)
            .list_indexes()
            .await?;
        let mut actual = BTreeSet::new();
        while let Some(model) = cursor.try_next().await? {
            if let Some(name) = model.options.and_then(|options| options.name) {
                actual.insert(name);
            }
        }
        if actual != expected {
            return Ok(false);
        }
    }
    Ok(true)
}

fn index(keys: Document, name: &str, unique: bool, partial: Option<Document>) -> IndexModel {
    IndexModel::builder()
        .keys(keys)
        .options(
            IndexOptions::builder()
                .name(name.to_owned())
                .unique(unique)
                .partial_filter_expression(partial)
                .build(),
        )
        .build()
}

fn encode_bundle(bundle: &ArtifactBundle) -> Result<Document, ArtifactStoreError> {
    if bundle.bindings.is_empty() || bundle.debug_id_tokens.len() > MAX_ARTIFACT_DEBUG_IDS {
        return Err(ArtifactStoreError::InvalidData);
    }
    let mut document = doc! {
        "_id": binary(&bundle.id.as_bytes()), "o": i64::try_from(bundle.organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?,
        "b": encode_bindings(&bundle.bindings)?, "g": binary(&bundle.bundle_debug_id.encode()),
        "k": bundle.debug_id_tokens.iter().map(|token| Bson::Int64(token.stored())).collect::<Vec<_>>(),
        "x": binary(&bundle.checksum), "h": binary(&bundle.sha1), "z": i64::try_from(bundle.size).map_err(|_| ArtifactStoreError::InvalidData)?, "u": datetime(bundle.uploaded_at),
    };
    if bundle.generation != 0 {
        document.insert(
            "v",
            i32::try_from(bundle.generation).map_err(|_| ArtifactStoreError::InvalidData)?,
        );
    }
    Ok(document)
}

fn decode_bundle(document: &Document) -> Result<ArtifactBundle, ArtifactStoreError> {
    if matches!(document.get_i32("s"), Ok(2)) {
        return Err(ArtifactStoreError::Busy);
    }
    let mut tokens = document
        .get_array("k")
        .map_err(|_| ArtifactStoreError::InvalidData)?
        .iter()
        .map(|value| {
            value
                .as_i64()
                .map(ArtifactDebugIdToken::from_stored)
                .ok_or(ArtifactStoreError::InvalidData)
        })
        .collect::<Result<Vec<_>, _>>()?;
    tokens.sort();
    tokens.dedup();
    Ok(ArtifactBundle {
        id: ArtifactBundleId::from_bytes(fixed(document, "_id")?),
        organization_id: organization_id(document)?,
        bindings: decode_bindings(document)?,
        bundle_debug_id: DebugId::decode(binary_slice(document, "g")?)
            .map_err(|_| ArtifactStoreError::InvalidData)?,
        debug_id_tokens: tokens,
        checksum: fixed(document, "x")?,
        sha1: fixed(document, "h")?,
        size: u64::try_from(
            document
                .get_i64("z")
                .map_err(|_| ArtifactStoreError::InvalidData)?,
        )
        .map_err(|_| ArtifactStoreError::InvalidData)?,
        uploaded_at: timestamp(document, "u")?,
        generation: stored_generation(document)?,
    })
}

fn encode_upload(upload: &ArtifactUpload) -> Result<Document, ArtifactStoreError> {
    if upload.chunks.is_empty() || upload.chunks.len() > MAX_ARTIFACT_CHUNKS {
        return Err(ArtifactStoreError::InvalidData);
    }
    let mut chunks = Vec::with_capacity(upload.chunks.len() * 20);
    for chunk in &upload.chunks {
        chunks.extend_from_slice(chunk);
    }
    Ok(
        doc! { "_id": binary(&upload.id), "o": i64::try_from(upload.organization_id.get()).map_err(|_| ArtifactStoreError::InvalidData)?, "h": binary(&upload.sha1), "c": binary(&chunks), "b": encode_bindings(&upload.bindings)?, "a": 0_i32, "t": datetime(upload.created_at), "u": datetime(upload.updated_at) },
    )
}

fn decode_upload(document: &Document) -> Result<ArtifactUploadRecord, ArtifactStoreError> {
    let packed = binary_slice(document, "c")?;
    if packed.is_empty() || packed.len() % 20 != 0 || packed.len() / 20 > MAX_ARTIFACT_CHUNKS {
        return Err(ArtifactStoreError::InvalidData);
    }
    let state = match document.get_i32("s") {
        Err(_) if !document.contains_key("s") => ArtifactUploadState::Pending,
        Ok(1) => ArtifactUploadState::Assembling,
        Ok(2) => ArtifactUploadState::Complete,
        Ok(3) => ArtifactUploadState::Failed,
        _ => return Err(ArtifactStoreError::InvalidData),
    };
    Ok(ArtifactUploadRecord {
        upload: ArtifactUpload {
            id: fixed(document, "_id")?,
            organization_id: organization_id(document)?,
            sha1: fixed(document, "h")?,
            chunks: packed
                .chunks_exact(20)
                .map(|chunk| chunk.try_into().expect("SHA-1 chunk"))
                .collect(),
            bindings: decode_bindings(document)?,
            created_at: timestamp(document, "t")?,
            updated_at: timestamp(document, "u")?,
        },
        state,
        attempts: document.get_i32("a").ok().map_or(Ok(0), |value| {
            u32::try_from(value).map_err(|_| ArtifactStoreError::InvalidData)
        })?,
        final_id: optional_fixed(document, "f")?.map(ArtifactBundleId::from_bytes),
        error_code: document
            .get_i32("q")
            .ok()
            .map(|value| u16::try_from(value).map_err(|_| ArtifactStoreError::InvalidData))
            .transpose()?,
    })
}

fn encode_bindings(bindings: &[ArtifactBinding]) -> Result<Vec<Bson>, ArtifactStoreError> {
    canonical_bindings(bindings.iter().cloned())?
        .iter()
        .map(|binding| {
            let mut value = doc! { "p": binding.project_id.get() };
            if let Some(release) = binding.release_id {
                value.insert("r", binary(&release.as_bytes()));
            }
            if let Some(dist) = &binding.dist {
                value.insert("d", dist.as_ref());
            }
            Ok(Bson::Document(value))
        })
        .collect()
}
fn decode_bindings(document: &Document) -> Result<Vec<ArtifactBinding>, ArtifactStoreError> {
    if !document.contains_key("b") {
        return Ok(Vec::new());
    }
    canonical_bindings(
        document
            .get_array("b")
            .map_err(|_| ArtifactStoreError::InvalidData)?
            .iter()
            .map(|value| {
                let value = value.as_document().ok_or(ArtifactStoreError::InvalidData)?;
                ArtifactBinding::new(
                    ProjectId::new(
                        value
                            .get_i32("p")
                            .map_err(|_| ArtifactStoreError::InvalidData)?,
                    )
                    .map_err(|_| ArtifactStoreError::InvalidData)?,
                    optional_fixed(value, "r")?.map(ReleaseId::from_bytes),
                    value.get_str("d").ok().map(Into::into),
                )
                .map_err(|_| ArtifactStoreError::InvalidData)
            })
            .collect::<Result<Vec<_>, _>>()?,
    )
}
fn canonical_bindings(
    bindings: impl IntoIterator<Item = ArtifactBinding>,
) -> Result<Vec<ArtifactBinding>, ArtifactStoreError> {
    let mut values = bindings.into_iter().collect::<Vec<_>>();
    values.sort();
    values.dedup();
    if values.is_empty() || values.len() > MAX_ARTIFACT_BINDINGS {
        return Err(ArtifactStoreError::InvalidData);
    }
    Ok(values)
}
fn affected_projects(before: &[ArtifactBinding], after: &[ArtifactBinding]) -> BTreeSet<ProjectId> {
    after
        .iter()
        .filter(|binding| !before.contains(binding))
        .map(|binding| binding.project_id)
        .collect()
}
fn validate_content_identity(
    document: &Document,
    bundle: &ArtifactBundle,
) -> Result<(), ArtifactStoreError> {
    if fixed::<16>(document, "_id")? != bundle.id.as_bytes()
        || organization_id(document)? != bundle.organization_id
        || fixed::<20>(document, "h")? != bundle.sha1
        || (document.contains_key("x") && fixed::<32>(document, "x")? != bundle.checksum)
        || stored_generation(document)? != bundle.generation
    {
        return Err(ArtifactStoreError::Conflict);
    }
    Ok(())
}
fn organization_id(document: &Document) -> Result<OrganizationId, ArtifactStoreError> {
    let value = document
        .get_i64(if document.contains_key("o") {
            "o"
        } else {
            "organization_id"
        })
        .map_err(|_| ArtifactStoreError::InvalidData)?;
    OrganizationId::new(u64::try_from(value).map_err(|_| ArtifactStoreError::InvalidData)?)
        .map_err(|_| ArtifactStoreError::InvalidData)
}
fn artifact_revision(document: &Document) -> Result<u64, ArtifactStoreError> {
    match document.get_i64("ar") {
        Ok(value) => u64::try_from(value).map_err(|_| ArtifactStoreError::InvalidData),
        Err(_) if !document.contains_key("ar") => Ok(0),
        Err(_) => Err(ArtifactStoreError::InvalidData),
    }
}
fn stored_generation(document: &Document) -> Result<u32, ArtifactStoreError> {
    match document.get_i32("v") {
        Ok(value) => u32::try_from(value).map_err(|_| ArtifactStoreError::InvalidData),
        Err(_) if !document.contains_key("v") => Ok(0),
        Err(_) => Err(ArtifactStoreError::InvalidData),
    }
}
fn generation_filter(generation: u32) -> Bson {
    if generation == 0 {
        Bson::Document(doc! { "$exists": false })
    } else {
        Bson::Int32(i32::try_from(generation).unwrap_or(i32::MAX))
    }
}
fn upload_state_code(state: ArtifactUploadState) -> Option<i32> {
    match state {
        ArtifactUploadState::Pending => None,
        ArtifactUploadState::Assembling => Some(1),
        ArtifactUploadState::Complete => Some(2),
        ArtifactUploadState::Failed => Some(3),
    }
}
fn binary(bytes: &[u8]) -> Binary {
    Binary {
        subtype: BinarySubtype::Generic,
        bytes: bytes.to_vec(),
    }
}
fn binary_slice<'a>(document: &'a Document, field: &str) -> Result<&'a [u8], ArtifactStoreError> {
    document
        .get_binary_generic(field)
        .map(Vec::as_slice)
        .map_err(|_| ArtifactStoreError::InvalidData)
}
fn fixed<const N: usize>(document: &Document, field: &str) -> Result<[u8; N], ArtifactStoreError> {
    binary_slice(document, field)?
        .try_into()
        .map_err(|_| ArtifactStoreError::InvalidData)
}
fn optional_fixed<const N: usize>(
    document: &Document,
    field: &str,
) -> Result<Option<[u8; N]>, ArtifactStoreError> {
    if document.contains_key(field) {
        fixed(document, field).map(Some)
    } else {
        Ok(None)
    }
}
fn datetime(value: Timestamp) -> DateTime {
    DateTime::from_millis(value.unix_millis())
}
fn timestamp(document: &Document, field: &str) -> Result<Timestamp, ArtifactStoreError> {
    Timestamp::from_unix_millis(
        document
            .get_datetime(field)
            .map_err(|_| ArtifactStoreError::InvalidData)?
            .timestamp_millis(),
    )
    .map_err(|_| ArtifactStoreError::InvalidData)
}
fn add_duration(
    value: Timestamp,
    duration: std::time::Duration,
) -> Result<Timestamp, ArtifactStoreError> {
    Timestamp::from_unix_millis(
        value
            .unix_millis()
            .checked_add(
                i64::try_from(duration.as_millis()).map_err(|_| ArtifactStoreError::InvalidData)?,
            )
            .ok_or(ArtifactStoreError::InvalidData)?,
    )
    .map_err(|_| ArtifactStoreError::InvalidData)
}
fn unavailable(_: mongodb::error::Error) -> ArtifactStoreError {
    ArtifactStoreError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_and_upload_codecs_round_trip_compactly() {
        let organization = OrganizationId::new(7).unwrap();
        let project = ProjectId::new(9).unwrap();
        let debug_id = DebugId::parse("67e9247c-814e-392b-a027-dbde6748fcbf").unwrap();
        let binding = ArtifactBinding::new(project, None, None).unwrap();
        let bundle = ArtifactBundle {
            id: ArtifactBundleId::derive(organization, [3; 32]),
            organization_id: organization,
            bindings: vec![binding.clone()],
            bundle_debug_id: debug_id.clone(),
            debug_id_tokens: vec![ArtifactDebugIdToken::derive(organization, &debug_id)],
            checksum: [3; 32],
            sha1: [4; 20],
            size: 123,
            uploaded_at: Timestamp::from_unix_millis(1_800_000_000_000).unwrap(),
            generation: 0,
        };
        let bundle_document = encode_bundle(&bundle).unwrap();
        assert_eq!(decode_bundle(&bundle_document).unwrap(), bundle);
        let upload = ArtifactUpload {
            id: [5; 16],
            organization_id: organization,
            sha1: [4; 20],
            chunks: vec![[4; 20]],
            bindings: vec![binding],
            created_at: bundle.uploaded_at,
            updated_at: bundle.uploaded_at,
        };
        let upload_document = encode_upload(&upload).unwrap();
        assert_eq!(decode_upload(&upload_document).unwrap().upload, upload);
        assert_eq!(mongodb::bson::to_vec(&bundle_document).unwrap().len(), 198);
        assert_eq!(mongodb::bson::to_vec(&upload_document).unwrap().len(), 150);
    }

    #[test]
    fn malformed_packed_chunks_and_cross_element_bindings_fail() {
        let mut upload = doc! { "_id": binary(&[1; 16]), "o": 7_i64, "h": binary(&[2; 20]), "c": binary(&[3; 21]), "b": [doc! { "p": 1_i32 }], "t": DateTime::now(), "u": DateTime::now() };
        assert!(decode_upload(&upload).is_err());
        upload.insert("c", binary(&[3; 20]));
        upload.insert("b", vec![Bson::Document(doc! { "p": 1_i32, "d": "web" })]);
        assert!(decode_upload(&upload).is_err());
    }
}
