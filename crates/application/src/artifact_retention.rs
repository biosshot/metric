//! Artifact Bundle retention (ADR-0032): unbinds bundles that projects no longer need.
//!
//! A project keeps a bundle while it is one of the project's newest `keep_latest`
//! uploads, was uploaded within `max_age`, or is bound to a release that saw an Event
//! within `max_age`. Removing a bundle's last binding leaves it to the generation-fenced
//! garbage collection of ADR-0031 once `orphan_grace` has passed.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    sync::Arc,
    time::Duration,
};

use metric_domain::{
    OrganizationId, ProjectId, Timestamp,
    artifacts::{ArtifactBundle, ArtifactRetentionMode},
    finalization::ReleaseId,
};
use metric_ports::{ArtifactStore, Clock, ReleaseStore, ReleaseStoreError};

use crate::{
    artifacts::{ArtifactError, add_duration, map_store},
    shutdown::ShutdownSignal,
};

const PAGE_SIZE: usize = 100;
const MAX_KEEP_LATEST: usize = 10_000;
const DAY_MILLIS: i64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, Copy)]
pub struct ArtifactRetentionConfig {
    pub mode: ArtifactRetentionMode,
    pub max_age: Duration,
    pub keep_latest: usize,
    pub interval: Duration,
    pub orphan_grace: Duration,
}

/// Totals of one pass. A bundle counts once for every project that binds it, so bytes
/// past retention are an upper bound of what garbage collection frees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactRetentionReport {
    pub bundles: usize,
    pub bytes: u64,
    pub stale_bundles: usize,
    pub stale_bytes: u64,
    pub stale_bindings: usize,
    pub removed_bindings: usize,
}

pub struct ArtifactRetention {
    artifacts: Arc<dyn ArtifactStore>,
    releases: Arc<dyn ReleaseStore>,
    clock: Arc<dyn Clock>,
    config: ArtifactRetentionConfig,
}

pub struct ArtifactRetentionTask {
    handle: tokio::task::JoinHandle<()>,
}

impl ArtifactRetentionTask {
    pub async fn wait(self) {
        let _ = self.handle.await;
    }
}

/// Runs a pass right after the start and then every `interval`. Passes run apart from
/// garbage collection, which must not wait for a pass over every bundle.
pub fn start_artifact_retention(
    retention: Arc<ArtifactRetention>,
    shutdown: ShutdownSignal,
) -> ArtifactRetentionTask {
    let interval = retention.config.interval;
    ArtifactRetentionTask {
        handle: tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    _ = ticker.tick() => {
                        // Shutdown drops a pass between storage calls, which leaves what a
                        // crash at that point would.
                        tokio::select! {
                            biased;
                            () = shutdown.cancelled() => break,
                            result = retention.run_once() => {
                                if let Err(error) = result {
                                    tracing::warn!(
                                        operation = "artifacts.retention",
                                        %error,
                                        "artifact retention pass failed"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }),
    }
}

struct Pass {
    now: Timestamp,
    cutoff: Timestamp,
    report: ArtifactRetentionReport,
}

/// What retention keeps of a release.
struct ReleaseSeen {
    last_seen: Option<Timestamp>,
    version: Box<str>,
}

/// The releases of one page, each loaded once; `None` marks one that does not exist.
/// The page bounds the cache, and a release's bundles are adjacent in upload order.
type PageReleases = BTreeMap<ReleaseId, Option<ReleaseSeen>>;

impl ArtifactRetention {
    pub fn new(
        artifacts: Arc<dyn ArtifactStore>,
        releases: Arc<dyn ReleaseStore>,
        clock: Arc<dyn Clock>,
        config: ArtifactRetentionConfig,
    ) -> Result<Self, ArtifactError> {
        if config.max_age.is_zero()
            || config.keep_latest > MAX_KEEP_LATEST
            || config.interval.is_zero()
            || config.orphan_grace.is_zero()
        {
            return Err(ArtifactError::InvalidRequest);
        }
        Ok(Self {
            artifacts,
            releases,
            clock,
            config,
        })
    }

    /// Applies retention to every project once. A storage error ends the pass, and the
    /// next pass starts over.
    pub async fn run_once(&self) -> Result<ArtifactRetentionReport, ArtifactError> {
        if self.config.mode == ArtifactRetentionMode::Off {
            return Ok(ArtifactRetentionReport::default());
        }
        let now = self.clock.now();
        let max_age = i64::try_from(self.config.max_age.as_millis())
            .map_err(|_| ArtifactError::InvalidRequest)?;
        let mut pass = Pass {
            now,
            cutoff: Timestamp::from_unix_millis(now.unix_millis().saturating_sub(max_age))
                .map_err(|_| ArtifactError::InvalidRequest)?,
            report: ArtifactRetentionReport::default(),
        };
        for (organization_id, project_id) in
            self.artifacts.bound_projects().await.map_err(map_store)?
        {
            match self
                .retain_project(organization_id, project_id, &mut pass)
                .await
            {
                // The project entered deletion during the pass, and deletion takes its
                // bindings.
                Ok(()) | Err(ArtifactError::NotFound) => {}
                Err(error) => return Err(error),
            }
        }
        let report = pass.report;
        tracing::info!(
            operation = "artifacts.retention",
            mode = self.config.mode.as_str(),
            bundles = report.bundles,
            bytes = report.bytes,
            stale_bundles = report.stale_bundles,
            stale_bytes = report.stale_bytes,
            stale_bindings = report.stale_bindings,
            removed_bindings = report.removed_bindings,
            "artifact retention pass finished"
        );
        Ok(report)
    }

    async fn retain_project(
        &self,
        organization_id: OrganizationId,
        project_id: ProjectId,
        pass: &mut Pass,
    ) -> Result<(), ArtifactError> {
        let mut rank = 0_usize;
        let mut before = None;
        loop {
            let page = self
                .artifacts
                .project_bundles(organization_id, project_id, before, PAGE_SIZE)
                .await
                .map_err(map_store)?;
            let mut releases = PageReleases::new();
            for bundle in &page {
                rank += 1;
                pass.report.bundles += 1;
                pass.report.bytes = pass.report.bytes.saturating_add(bundle.size);
                if rank > self.config.keep_latest && bundle.uploaded_at < pass.cutoff {
                    self.retain_bundle(organization_id, project_id, bundle, pass, &mut releases)
                        .await?;
                }
            }
            match page.last() {
                Some(last) if page.len() == PAGE_SIZE => before = Some((last.uploaded_at, last.id)),
                _ => return Ok(()),
            }
        }
    }

    /// Handles the project's bindings of a bundle that is neither recent nor among the
    /// newest: only a release with a recent Event still keeps such a binding.
    async fn retain_bundle(
        &self,
        organization_id: OrganizationId,
        project_id: ProjectId,
        bundle: &ArtifactBundle,
        pass: &mut Pass,
        releases: &mut PageReleases,
    ) -> Result<(), ArtifactError> {
        let mut stale = Vec::new();
        for binding in bundle
            .bindings
            .iter()
            .filter(|binding| binding.project_id == project_id)
        {
            let release = match binding.release_id {
                Some(release_id) => self.release(organization_id, release_id, releases).await?,
                None => None,
            };
            if release
                .and_then(|release| release.last_seen)
                .is_some_and(|seen| seen >= pass.cutoff)
            {
                continue;
            }
            tracing::info!(
                operation = "artifacts.retention",
                mode = self.config.mode.as_str(),
                project_id = project_id.get(),
                bundle_id = %bundle.id,
                age_days = (pass.now.unix_millis() - bundle.uploaded_at.unix_millis()) / DAY_MILLIS,
                size = bundle.size,
                release = release.map(|release| release.version.as_ref()),
                "artifact bundle binding is past retention"
            );
            stale.push(binding);
        }
        if stale.is_empty() {
            return Ok(());
        }
        pass.report.stale_bundles += 1;
        pass.report.stale_bytes = pass.report.stale_bytes.saturating_add(bundle.size);
        pass.report.stale_bindings += stale.len();
        if self.config.mode != ArtifactRetentionMode::Enforce {
            return Ok(());
        }
        for binding in stale {
            // The grace starts at the removal: a long pass must not hand GC a deadline
            // that has already passed.
            let orphan_at = add_duration(self.clock.now(), self.config.orphan_grace)?;
            // Only while the bundle keeps the upload time this page showed: a newer
            // publication of the same content keeps its binding.
            if self
                .artifacts
                .remove_binding(
                    organization_id,
                    bundle.id,
                    binding.clone(),
                    orphan_at,
                    Some(bundle.uploaded_at),
                )
                .await
                .map_err(map_store)?
                .is_some()
            {
                pass.report.removed_bindings += 1;
            }
        }
        Ok(())
    }

    async fn release<'a>(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
        releases: &'a mut PageReleases,
    ) -> Result<Option<&'a ReleaseSeen>, ArtifactError> {
        if let Entry::Vacant(entry) = releases.entry(release_id) {
            entry.insert(
                match self
                    .releases
                    .load_release(organization_id, release_id)
                    .await
                {
                    Ok(release) => Some(ReleaseSeen {
                        last_seen: release.last_seen,
                        version: release.version,
                    }),
                    Err(ReleaseStoreError::NotFound) => None,
                    Err(_) => return Err(ArtifactError::Unavailable),
                },
            );
        }
        Ok(releases.get(&release_id).and_then(Option::as_ref))
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    use metric_domain::{
        api::ProjectView,
        artifacts::{
            ArtifactBinding, ArtifactBundleId, ArtifactCandidate, ArtifactGcClaim, ArtifactLookup,
            ArtifactUpload, ArtifactUploadRecord, ArtifactUploadState,
        },
        debug_files::DebugId,
        finalization::derive_release_id,
        grouping::IssueId,
        releases::{
            CreateDeploy, CreateRelease, DeployId, DeployRecord, FinalizeRelease,
            ReleaseIssueSummary, ReleaseRecord,
        },
    };
    use metric_ports::{ArtifactStoreError, PortFuture, ReleaseIssueKind};

    use super::*;
    use crate::shutdown::ShutdownRoot;

    const NOW: i64 = 100;
    const GRACE: Duration = Duration::from_secs(24 * 60 * 60);

    fn day(value: i64) -> Timestamp {
        Timestamp::from_unix_millis(1_800_000_000_000 + value * DAY_MILLIS).unwrap()
    }

    fn organization() -> OrganizationId {
        OrganizationId::new(1).unwrap()
    }

    fn project(value: i32) -> ProjectId {
        ProjectId::new(value).unwrap()
    }

    fn binding(project_id: i32, release: Option<&str>) -> ArtifactBinding {
        ArtifactBinding::new(
            project(project_id),
            release.map(|version| derive_release_id(organization(), version)),
            None,
        )
        .unwrap()
    }

    fn bundle(seed: u16, uploaded_day: i64, bindings: Vec<ArtifactBinding>) -> ArtifactBundle {
        let mut id = [0_u8; 16];
        id[..2].copy_from_slice(&seed.to_be_bytes());
        ArtifactBundle {
            id: ArtifactBundleId::from_bytes(id),
            organization_id: organization(),
            bindings,
            bundle_debug_id: DebugId::Uuid {
                uuid: id,
                appendix: 0,
            },
            debug_id_tokens: Vec::new(),
            checksum: [0; 32],
            sha1: [0; 20],
            size: 1_000,
            uploaded_at: day(uploaded_day),
            generation: 0,
        }
    }

    fn release(version: &str, last_seen: i64) -> ReleaseRecord {
        ReleaseRecord {
            id: derive_release_id(organization(), version),
            organization_id: organization(),
            version: version.into(),
            project_ids: vec![project(1)],
            created_at: day(0),
            activity_at: day(last_seen),
            released_at: None,
            first_seen: Some(day(0)),
            last_seen: Some(day(last_seen)),
            first_event_id: None,
            latest_event_id: None,
            url: None,
            reference: None,
            repositories: Vec::new(),
            explicit: false,
        }
    }

    fn retention(
        mode: ArtifactRetentionMode,
        artifacts: &Arc<Artifacts>,
        releases: Vec<ReleaseRecord>,
    ) -> ArtifactRetention {
        ArtifactRetention::new(
            Arc::clone(artifacts) as Arc<dyn ArtifactStore>,
            Arc::new(Releases(
                releases
                    .into_iter()
                    .map(|release| (release.id, release))
                    .collect(),
            )),
            Arc::new(Ticking(Mutex::new(day(NOW)))),
            ArtifactRetentionConfig {
                mode,
                max_age: Duration::from_secs(30 * 24 * 60 * 60),
                keep_latest: 2,
                interval: Duration::from_secs(60 * 60),
                orphan_grace: GRACE,
            },
        )
        .unwrap()
    }

    /// The cutoff is day 70. Project 1 binds bundles 1-7, project 3 shares bundle 7, and
    /// project 2 enters deletion during the pass, so removing its binding fails.
    fn fixture() -> (Arc<Artifacts>, Vec<ReleaseRecord>) {
        let artifacts = Arc::new(Artifacts::new(vec![
            bundle(1, 99, vec![binding(1, None)]),
            bundle(2, 50, vec![binding(1, None)]),
            bundle(3, 40, vec![binding(1, None)]),
            bundle(4, 30, vec![binding(1, Some("active"))]),
            bundle(5, 20, vec![binding(1, Some("idle"))]),
            bundle(6, 10, vec![binding(1, Some("missing"))]),
            bundle(7, 5, vec![binding(1, Some("idle")), binding(3, None)]),
            bundle(8, 3, vec![binding(2, None)]),
            bundle(9, 2, vec![binding(2, None)]),
            bundle(10, 1, vec![binding(2, None)]),
            bundle(11, 4, vec![binding(3, None)]),
            bundle(12, 0, vec![binding(3, None)]),
        ]));
        (artifacts, vec![release("active", 95), release("idle", 60)])
    }

    #[tokio::test]
    async fn report_counts_stale_bindings_and_changes_nothing() {
        let (artifacts, releases) = fixture();
        let before = artifacts.bindings();
        let report = retention(ArtifactRetentionMode::Report, &artifacts, releases)
            .run_once()
            .await
            .unwrap();
        assert_eq!(
            report,
            ArtifactRetentionReport {
                bundles: 13,
                bytes: 13_000,
                stale_bundles: 6,
                stale_bytes: 6_000,
                stale_bindings: 6,
                removed_bindings: 0,
            }
        );
        assert_eq!(artifacts.bindings(), before);
    }

    #[tokio::test]
    async fn enforce_keeps_newest_recent_and_active_release_bundles() {
        let (artifacts, releases) = fixture();
        let retention = retention(ArtifactRetentionMode::Enforce, &artifacts, releases);
        let report = retention.run_once().await.unwrap();
        assert_eq!(report.stale_bindings, 6);
        // Project 2's binding failed, and project 3 was still handled after it.
        assert_eq!(report.removed_bindings, 5);
        assert_eq!(
            artifacts.bindings(),
            BTreeMap::from([
                (1, vec![binding(1, None)]),
                (2, vec![binding(1, None)]),
                (3, Vec::new()),
                (4, vec![binding(1, Some("active"))]),
                (5, Vec::new()),
                (6, Vec::new()),
                (7, vec![binding(3, None)]),
                (8, vec![binding(2, None)]),
                (9, vec![binding(2, None)]),
                (10, vec![binding(2, None)]),
                (11, vec![binding(3, None)]),
                (12, Vec::new()),
            ])
        );
        let again = retention.run_once().await.unwrap();
        assert_eq!((again.stale_bindings, again.removed_bindings), (1, 0));
    }

    #[tokio::test]
    async fn orphan_grace_starts_when_each_binding_is_removed() {
        let (artifacts, releases) = fixture();
        retention(ArtifactRetentionMode::Enforce, &artifacts, releases)
            .run_once()
            .await
            .unwrap();
        let deadlines = artifacts.deadlines.lock().unwrap().clone();
        // The clock moves an hour per read and the pass read it first.
        assert!(deadlines[0] > add_duration(day(NOW), GRACE).unwrap());
        assert!(deadlines.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[tokio::test]
    async fn off_leaves_every_binding() {
        let (artifacts, releases) = fixture();
        let before = artifacts.bindings();
        let report = retention(ArtifactRetentionMode::Off, &artifacts, releases)
            .run_once()
            .await
            .unwrap();
        assert_eq!(report, ArtifactRetentionReport::default());
        assert_eq!(artifacts.bindings(), before);
    }

    #[tokio::test]
    async fn pages_through_bundles_uploaded_at_the_same_time() {
        let count = 2 * u16::try_from(PAGE_SIZE).unwrap() + 5;
        let artifacts = Arc::new(Artifacts::new(
            (1..=count)
                .map(|seed| bundle(seed, 10, vec![binding(1, None)]))
                .collect(),
        ));
        let report = retention(ArtifactRetentionMode::Enforce, &artifacts, Vec::new())
            .run_once()
            .await
            .unwrap();
        assert_eq!(report.removed_bindings, usize::from(count) - 2);
        let kept = artifacts
            .bindings()
            .into_iter()
            .filter(|(_, bindings)| !bindings.is_empty())
            .map(|(seed, _)| seed)
            .collect::<Vec<_>>();
        assert_eq!(kept, vec![count - 1, count]);
    }

    #[tokio::test]
    async fn shutdown_stops_a_pass_in_progress() {
        let (artifacts, releases) = fixture();
        artifacts.hang.store(true, Ordering::SeqCst);
        let root = ShutdownRoot::new();
        let task = start_artifact_retention(
            Arc::new(retention(
                ArtifactRetentionMode::Enforce,
                &artifacts,
                releases,
            )),
            root.signal(),
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !artifacts.listing.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        root.begin();
        tokio::time::timeout(Duration::from_secs(5), task.wait())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_newer_publication_keeps_its_binding() {
        let (artifacts, releases) = fixture();
        artifacts.republish.store(true, Ordering::SeqCst);
        let before = artifacts.bindings();
        let retention = retention(ArtifactRetentionMode::Enforce, &artifacts, releases);
        let report = retention.run_once().await.unwrap();
        // The pages showed stale bundles, but each was published again before removal.
        assert_eq!(report.stale_bindings, 6);
        assert_eq!(report.removed_bindings, 0);
        assert_eq!(artifacts.bindings(), before);
        let again = retention.run_once().await.unwrap();
        assert_eq!(again.stale_bindings, 0);
    }

    #[test]
    fn rejects_a_policy_that_would_unbind_everything_at_once() {
        let (artifacts, _) = fixture();
        let config = ArtifactRetentionConfig {
            mode: ArtifactRetentionMode::Enforce,
            max_age: Duration::ZERO,
            keep_latest: 2,
            interval: Duration::from_secs(60),
            orphan_grace: Duration::from_secs(60),
        };
        assert!(
            ArtifactRetention::new(
                artifacts,
                Arc::new(Releases(BTreeMap::new())),
                Arc::new(Ticking(Mutex::new(day(NOW)))),
                config,
            )
            .is_err()
        );
    }

    /// Moves an hour forward on every read.
    struct Ticking(Mutex<Timestamp>);

    impl Clock for Ticking {
        fn now(&self) -> Timestamp {
            let mut now = self.0.lock().unwrap();
            let current = *now;
            *now = add_duration(current, Duration::from_secs(60 * 60)).unwrap();
            current
        }
    }

    struct Artifacts {
        bundles: Mutex<Vec<ArtifactBundle>>,
        /// `orphan_at` of every removal, in order.
        deadlines: Mutex<Vec<Timestamp>>,
        /// Makes the project listing never finish.
        hang: AtomicBool,
        listing: AtomicBool,
        /// Republishes every bundle of a page right after it was read.
        republish: AtomicBool,
    }

    impl Artifacts {
        fn new(bundles: Vec<ArtifactBundle>) -> Self {
            Self {
                bundles: Mutex::new(bundles),
                deadlines: Mutex::new(Vec::new()),
                hang: AtomicBool::new(false),
                listing: AtomicBool::new(false),
                republish: AtomicBool::new(false),
            }
        }

        /// Bindings by the seed `bundle` was built from.
        fn bindings(&self) -> BTreeMap<u16, Vec<ArtifactBinding>> {
            self.bundles
                .lock()
                .unwrap()
                .iter()
                .map(|bundle| {
                    let id = bundle.id.as_bytes();
                    (u16::from_be_bytes([id[0], id[1]]), bundle.bindings.clone())
                })
                .collect()
        }
    }

    impl ArtifactStore for Artifacts {
        fn resolve_projects(
            &self,
            _: Box<str>,
            _: Vec<Box<str>>,
        ) -> PortFuture<'_, Result<(OrganizationId, Vec<ProjectView>), ArtifactStoreError>>
        {
            unimplemented!("retention resolves no slugs")
        }

        fn project_organization(
            &self,
            _: ProjectId,
        ) -> PortFuture<'_, Result<OrganizationId, ArtifactStoreError>> {
            unimplemented!("projects come with their organization")
        }

        fn load_by_sha1(
            &self,
            _: OrganizationId,
            _: [u8; 20],
        ) -> PortFuture<'_, Result<Option<ArtifactBundle>, ArtifactStoreError>> {
            unimplemented!("retention loads no content")
        }

        fn upsert_upload(
            &self,
            _: ArtifactUpload,
        ) -> PortFuture<'_, Result<ArtifactUploadRecord, ArtifactStoreError>> {
            unimplemented!("retention uploads nothing")
        }

        fn set_upload_state(
            &self,
            _: [u8; 16],
            _: ArtifactUploadState,
            _: Timestamp,
            _: Option<ArtifactBundleId>,
            _: Option<u16>,
        ) -> PortFuture<'_, Result<(), ArtifactStoreError>> {
            unimplemented!("retention uploads nothing")
        }

        fn publication_generation(
            &self,
            _: OrganizationId,
            _: [u8; 20],
            _: [u8; 16],
            _: Timestamp,
        ) -> PortFuture<'_, Result<u32, ArtifactStoreError>> {
            unimplemented!("retention publishes nothing")
        }

        fn publish_bundle(
            &self,
            _: [u8; 16],
            _: ArtifactBundle,
        ) -> PortFuture<'_, Result<Vec<(ProjectId, u64)>, ArtifactStoreError>> {
            unimplemented!("retention publishes nothing")
        }

        fn lookup(
            &self,
            _: ArtifactLookup,
        ) -> PortFuture<'_, Result<Vec<ArtifactCandidate>, ArtifactStoreError>> {
            unimplemented!("retention looks nothing up")
        }

        fn load_for_project(
            &self,
            _: ProjectId,
            _: ArtifactBundleId,
        ) -> PortFuture<'_, Result<ArtifactBundle, ArtifactStoreError>> {
            unimplemented!("retention reads bundles by page")
        }

        fn bound_projects(
            &self,
        ) -> PortFuture<'_, Result<Vec<(OrganizationId, ProjectId)>, ArtifactStoreError>> {
            if self.hang.load(Ordering::SeqCst) {
                self.listing.store(true, Ordering::SeqCst);
                return Box::pin(std::future::pending());
            }
            let projects = self
                .bundles
                .lock()
                .unwrap()
                .iter()
                .flat_map(|bundle| bundle.bindings.iter().map(|binding| binding.project_id))
                .collect::<BTreeSet<_>>();
            Box::pin(async move {
                Ok(projects
                    .into_iter()
                    .map(|project_id| (organization(), project_id))
                    .collect())
            })
        }

        fn project_bundles(
            &self,
            _: OrganizationId,
            project_id: ProjectId,
            before: Option<(Timestamp, ArtifactBundleId)>,
            limit: usize,
        ) -> PortFuture<'_, Result<Vec<ArtifactBundle>, ArtifactStoreError>> {
            let mut stored = self.bundles.lock().unwrap();
            let mut bundles = stored
                .iter()
                .filter(|bundle| {
                    bundle
                        .bindings
                        .iter()
                        .any(|binding| binding.project_id == project_id)
                        && before.is_none_or(|position| (bundle.uploaded_at, bundle.id) < position)
                })
                .cloned()
                .collect::<Vec<_>>();
            bundles.sort_by(|left, right| {
                (right.uploaded_at, right.id).cmp(&(left.uploaded_at, left.id))
            });
            bundles.truncate(limit);
            if self.republish.load(Ordering::SeqCst) {
                for bundle in stored.iter_mut() {
                    if bundles.iter().any(|listed| listed.id == bundle.id) {
                        bundle.uploaded_at = day(NOW);
                    }
                }
            }
            Box::pin(async move { Ok(bundles) })
        }

        fn remove_binding(
            &self,
            _: OrganizationId,
            bundle_id: ArtifactBundleId,
            binding: ArtifactBinding,
            orphan_at: Timestamp,
            uploaded_at: Option<Timestamp>,
        ) -> PortFuture<'_, Result<Option<u64>, ArtifactStoreError>> {
            // Project 2 enters deletion during the pass.
            if binding.project_id == project(2) {
                return Box::pin(async { Err(ArtifactStoreError::NotFound) });
            }
            self.deadlines.lock().unwrap().push(orphan_at);
            let removed = self
                .bundles
                .lock()
                .unwrap()
                .iter_mut()
                .find(|bundle| {
                    bundle.id == bundle_id
                        && uploaded_at.is_none_or(|expected| bundle.uploaded_at == expected)
                })
                .is_some_and(|bundle| {
                    let before = bundle.bindings.len();
                    bundle.bindings.retain(|candidate| *candidate != binding);
                    bundle.bindings.len() < before
                });
            Box::pin(async move { Ok(removed.then_some(1)) })
        }

        fn recoverable_uploads(
            &self,
            _: usize,
        ) -> PortFuture<'_, Result<Vec<ArtifactUploadRecord>, ArtifactStoreError>> {
            unimplemented!("retention recovers no uploads")
        }

        fn claim_gc(
            &self,
            _: Timestamp,
            _: Timestamp,
            _: [u8; 16],
            _: usize,
        ) -> PortFuture<'_, Result<Vec<ArtifactGcClaim>, ArtifactStoreError>> {
            unimplemented!("garbage collection is separate")
        }

        fn validate_gc_claim(
            &self,
            _: ArtifactBundleId,
            _: u32,
            _: [u8; 16],
            _: Timestamp,
        ) -> PortFuture<'_, Result<bool, ArtifactStoreError>> {
            unimplemented!("garbage collection is separate")
        }

        fn finish_gc(
            &self,
            _: ArtifactBundleId,
            _: u32,
            _: [u8; 16],
            _: Timestamp,
        ) -> PortFuture<'_, Result<bool, ArtifactStoreError>> {
            unimplemented!("garbage collection is separate")
        }
    }

    struct Releases(BTreeMap<ReleaseId, ReleaseRecord>);

    impl ReleaseStore for Releases {
        fn resolve_projects(
            &self,
            _: Box<str>,
            _: Vec<Box<str>>,
        ) -> PortFuture<'_, Result<(OrganizationId, Vec<ProjectId>), ReleaseStoreError>> {
            unimplemented!("retention resolves no slugs")
        }

        fn create_release(
            &self,
            _: CreateRelease,
        ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
            unimplemented!("retention only reads releases")
        }

        fn finalize_release(
            &self,
            _: FinalizeRelease,
        ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
            unimplemented!("retention only reads releases")
        }

        fn load_release(
            &self,
            _: OrganizationId,
            release_id: ReleaseId,
        ) -> PortFuture<'_, Result<ReleaseRecord, ReleaseStoreError>> {
            let release = self
                .0
                .get(&release_id)
                .cloned()
                .ok_or(ReleaseStoreError::NotFound);
            Box::pin(async move { release })
        }

        fn create_deploy(
            &self,
            _: CreateDeploy,
        ) -> PortFuture<'_, Result<DeployRecord, ReleaseStoreError>> {
            unimplemented!("retention reads no deploys")
        }

        fn finish_deploy(
            &self,
            _: OrganizationId,
            _: DeployId,
            _: Timestamp,
        ) -> PortFuture<'_, Result<DeployRecord, ReleaseStoreError>> {
            unimplemented!("retention reads no deploys")
        }

        fn list_deploys(
            &self,
            _: OrganizationId,
            _: ProjectId,
            _: ReleaseId,
            _: Option<(Timestamp, DeployId)>,
            _: usize,
        ) -> PortFuture<'_, Result<Vec<DeployRecord>, ReleaseStoreError>> {
            unimplemented!("retention reads no deploys")
        }

        fn list_release_issues(
            &self,
            _: ProjectId,
            _: Box<str>,
            _: ReleaseIssueKind,
            _: Option<(Timestamp, IssueId)>,
            _: usize,
        ) -> PortFuture<'_, Result<Vec<ReleaseIssueSummary>, ReleaseStoreError>> {
            unimplemented!("retention reads no issues")
        }
    }
}
