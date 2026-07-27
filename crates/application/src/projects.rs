//! Project identity commands and the bounded DSN authorization cache.

use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use metric_domain::{
    DisplayName, DsnKey, IpScrubPolicy, ItemCapabilities, OrganizationId, OrganizationIdentity,
    ProjectAcceptanceState, ProjectId, ProjectIdentity, ProjectIngestLimits, ProjectKeyIdentity,
    ProjectKeyLabel, ProjectKeyState, ProjectSnapshot, Slug,
    api::{ProjectKeyView, ProjectPolicyUpdate, ProjectView},
};
use metric_ports::{
    Clock, PortFuture, ProjectResolveError, ProjectResolver, ProjectStore, ProjectStoreError,
    RandomSource,
};
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::observability::{Metrics, ProjectCacheResult};

#[derive(Debug, Clone, Copy)]
pub struct ProjectCacheConfig {
    pub capacity: usize,
    pub max_inflight: usize,
    pub positive_ttl: Duration,
    pub negative_ttl: Duration,
}

#[derive(Debug, Clone)]
pub struct CreateOrganization {
    pub slug: Slug,
    pub display_name: DisplayName,
}

#[derive(Debug, Clone)]
pub struct CreateProject {
    pub organization_id: OrganizationId,
    pub slug: Slug,
    pub display_name: DisplayName,
    pub ip_policy: IpScrubPolicy,
    pub items: ItemCapabilities,
    pub limits: ProjectIngestLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreatedProject {
    pub project_id: ProjectId,
    pub dsn_key: DsnKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProjectServiceError {
    #[error("project service configuration is invalid")]
    InvalidConfiguration,
    #[error("organization or project already exists")]
    AlreadyExists,
    #[error("organization or project does not exist")]
    NotFound,
    #[error("random identity collision retry limit was exhausted")]
    CollisionExhausted,
    #[error("cryptographic randomness is unavailable")]
    RandomUnavailable,
    #[error("project identity storage is temporarily unavailable")]
    Unavailable,
    #[error("project state transition is not owned by this phase")]
    InvalidStateTransition,
}

impl ProjectServiceError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "invalid_project_configuration",
            Self::AlreadyExists => "project_identity_exists",
            Self::NotFound => "project_identity_not_found",
            Self::CollisionExhausted => "identity_collision_exhausted",
            Self::RandomUnavailable => "random_unavailable",
            Self::Unavailable => "project_storage_unavailable",
            Self::InvalidStateTransition => "invalid_project_state_transition",
        }
    }
}

type LookupResult = Result<ProjectSnapshot, ProjectResolveError>;
type SharedLookup = Arc<OnceCell<LookupResult>>;

pub struct ProjectService {
    store: Arc<dyn ProjectStore>,
    clock: Arc<dyn Clock>,
    random: Arc<dyn RandomSource>,
    collision_retries: usize,
    cache: Mutex<CacheState>,
    cache_generation: AtomicU64,
    cache_config: ProjectCacheConfig,
}

impl ProjectService {
    pub fn new(
        store: Arc<dyn ProjectStore>,
        clock: Arc<dyn Clock>,
        random: Arc<dyn RandomSource>,
        collision_retries: usize,
        cache_config: ProjectCacheConfig,
    ) -> Result<Self, ProjectServiceError> {
        let valid = collision_retries > 0
            && cache_config.capacity > 0
            && cache_config.max_inflight > 0
            && !cache_config.positive_ttl.is_zero()
            && !cache_config.negative_ttl.is_zero();
        if !valid {
            return Err(ProjectServiceError::InvalidConfiguration);
        }
        Ok(Self {
            store,
            clock,
            random,
            collision_retries,
            cache: Mutex::new(CacheState::new(cache_config.capacity)),
            cache_generation: AtomicU64::new(0),
            cache_config,
        })
    }

    pub async fn create_organization(
        &self,
        command: CreateOrganization,
    ) -> Result<OrganizationId, ProjectServiceError> {
        for _ in 0..self.collision_retries {
            let Some(id) = self.random_organization_id()? else {
                continue;
            };
            let organization = OrganizationIdentity {
                id,
                slug: command.slug.clone(),
                display_name: command.display_name.clone(),
                created_at: self.clock.now(),
            };
            match self.store.insert_organization(organization).await {
                Ok(()) => return Ok(id),
                Err(ProjectStoreError::IdentityCollision) => {}
                Err(ProjectStoreError::OrganizationSlugExists) => {
                    return Err(ProjectServiceError::AlreadyExists);
                }
                Err(error) => return Err(map_command_store_error(error)),
            }
        }
        Err(ProjectServiceError::CollisionExhausted)
    }

    pub async fn create_project(
        &self,
        command: CreateProject,
    ) -> Result<CreatedProject, ProjectServiceError> {
        let project_id = self.insert_generated_project(&command).await?;
        let label = ProjectKeyLabel::new("default").expect("default key label is valid");
        for _ in 0..self.collision_retries {
            let dsn_key = self.random_dsn_key()?;
            let key = ProjectKeyIdentity {
                key: dsn_key,
                project_id,
                state: ProjectKeyState::Active,
                label: label.clone(),
                created_at: self.clock.now(),
            };
            match self.store.insert_project_key(key).await {
                Ok(()) => {
                    self.invalidate_key(dsn_key);
                    return Ok(CreatedProject {
                        project_id,
                        dsn_key,
                    });
                }
                Err(ProjectStoreError::KeyCollision) => {}
                Err(error) => return Err(map_command_store_error(error)),
            }
        }
        Err(ProjectServiceError::CollisionExhausted)
    }

    pub async fn create_project_key(
        &self,
        project_id: ProjectId,
        label: ProjectKeyLabel,
    ) -> Result<DsnKey, ProjectServiceError> {
        for _ in 0..self.collision_retries {
            let dsn_key = self.random_dsn_key()?;
            let key = ProjectKeyIdentity {
                key: dsn_key,
                project_id,
                state: ProjectKeyState::Active,
                label: label.clone(),
                created_at: self.clock.now(),
            };
            match self.store.insert_project_key(key).await {
                Ok(()) => {
                    self.invalidate_key(dsn_key);
                    return Ok(dsn_key);
                }
                Err(ProjectStoreError::KeyCollision) => {}
                Err(error) => return Err(map_command_store_error(error)),
            }
        }
        Err(ProjectServiceError::CollisionExhausted)
    }

    pub async fn list_projects(
        &self,
        organization_id: OrganizationId,
        limit: usize,
    ) -> Result<Vec<ProjectView>, ProjectServiceError> {
        self.store
            .list_projects(organization_id, limit)
            .await
            .map_err(map_command_store_error)
    }

    pub async fn load_project_view(
        &self,
        project_id: ProjectId,
    ) -> Result<ProjectView, ProjectServiceError> {
        self.store
            .load_project_by_id(project_id)
            .await
            .map_err(map_command_store_error)
    }

    pub async fn list_project_keys(
        &self,
        project_id: ProjectId,
    ) -> Result<Vec<ProjectKeyView>, ProjectServiceError> {
        self.store
            .list_project_keys(project_id)
            .await
            .map_err(map_command_store_error)
    }

    pub async fn update_project_policy(
        &self,
        project_id: ProjectId,
        update: ProjectPolicyUpdate,
    ) -> Result<ProjectView, ProjectServiceError> {
        let (project, keys) = self
            .store
            .update_project_policy(project_id, update)
            .await
            .map_err(map_command_store_error)?;
        self.cache_generation.fetch_add(1, Ordering::AcqRel);
        let mut cache = self.cache.lock().expect("project cache lock poisoned");
        for key in keys {
            cache.invalidate(key);
        }
        Ok(project)
    }

    async fn insert_generated_project(
        &self,
        command: &CreateProject,
    ) -> Result<ProjectId, ProjectServiceError> {
        for _ in 0..self.collision_retries {
            let Some(id) = self.random_project_id()? else {
                continue;
            };
            let project = ProjectIdentity {
                id,
                organization_id: command.organization_id,
                slug: command.slug.clone(),
                display_name: command.display_name.clone(),
                state: ProjectAcceptanceState::Active,
                policy_revision: 1,
                ip_policy: command.ip_policy,
                items: command.items,
                limits: command.limits,
                grouping_revision: 1,
                created_at: self.clock.now(),
            };
            match self.store.insert_project(project).await {
                Ok(()) => return Ok(id),
                Err(ProjectStoreError::IdentityCollision) => {}
                Err(ProjectStoreError::ProjectSlugExists) => {
                    return Err(ProjectServiceError::AlreadyExists);
                }
                Err(error) => return Err(map_command_store_error(error)),
            }
        }
        Err(ProjectServiceError::CollisionExhausted)
    }

    pub async fn set_key_state(
        &self,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> Result<ProjectId, ProjectServiceError> {
        let project_id = self
            .store
            .set_key_state(key, state)
            .await
            .map_err(map_command_store_error)?;
        self.invalidate_key(key);
        Ok(project_id)
    }

    pub async fn set_project_key_state(
        &self,
        project_id: ProjectId,
        key: DsnKey,
        state: ProjectKeyState,
    ) -> Result<(), ProjectServiceError> {
        self.store
            .set_project_key_state(project_id, key, state)
            .await
            .map_err(map_command_store_error)?;
        self.invalidate_key(key);
        Ok(())
    }

    pub async fn set_project_acceptance(
        &self,
        project_id: ProjectId,
        state: ProjectAcceptanceState,
    ) -> Result<(), ProjectServiceError> {
        if matches!(
            state,
            ProjectAcceptanceState::Purging | ProjectAcceptanceState::Deleted
        ) {
            return Err(ProjectServiceError::InvalidStateTransition);
        }
        let keys = self
            .store
            .set_project_acceptance(project_id, state)
            .await
            .map_err(map_command_store_error)?;
        self.cache_generation.fetch_add(1, Ordering::AcqRel);
        let mut cache = self.cache.lock().expect("project cache lock poisoned");
        for key in keys {
            cache.invalidate(key);
        }
        Ok(())
    }

    pub fn invalidate_keys(&self, keys: &[DsnKey]) {
        self.cache_generation.fetch_add(1, Ordering::AcqRel);
        let mut cache = self.cache.lock().expect("project cache lock poisoned");
        for key in keys {
            cache.invalidate(*key);
        }
    }

    #[must_use]
    pub fn cached_entries(&self) -> usize {
        self.cache
            .lock()
            .expect("project cache lock poisoned")
            .entries
            .len()
    }

    fn random_organization_id(&self) -> Result<Option<OrganizationId>, ProjectServiceError> {
        let mut bytes = [0_u8; 8];
        self.random
            .fill_bytes(&mut bytes)
            .map_err(|_| ProjectServiceError::RandomUnavailable)?;
        let value = u64::from_be_bytes(bytes) & i64::MAX as u64;
        Ok(OrganizationId::new(value).ok())
    }

    fn random_project_id(&self) -> Result<Option<ProjectId>, ProjectServiceError> {
        let mut bytes = [0_u8; 4];
        self.random
            .fill_bytes(&mut bytes)
            .map_err(|_| ProjectServiceError::RandomUnavailable)?;
        let value = i32::from_be_bytes(bytes) & i32::MAX;
        Ok(ProjectId::new(value).ok())
    }

    fn random_dsn_key(&self) -> Result<DsnKey, ProjectServiceError> {
        let mut bytes = [0_u8; 16];
        self.random
            .fill_bytes(&mut bytes)
            .map_err(|_| ProjectServiceError::RandomUnavailable)?;
        Ok(DsnKey::from_bytes(bytes))
    }

    fn invalidate_key(&self, key: DsnKey) {
        self.cache_generation.fetch_add(1, Ordering::AcqRel);
        self.cache
            .lock()
            .expect("project cache lock poisoned")
            .invalidate(key);
    }

    async fn resolve_cached(&self, key: DsnKey) -> LookupResult {
        let now = self.clock.now().unix_millis();
        if let Some(result) = self
            .cache
            .lock()
            .expect("project cache lock poisoned")
            .get(key, now)
        {
            Metrics.project_cache(ProjectCacheResult::Hit);
            return result;
        }

        let generation = self.cache_generation.load(Ordering::Acquire);
        let (shared, cache_result) = {
            let mut cache = self.cache.lock().expect("project cache lock poisoned");
            if let Some(existing) = cache.inflight.get(&key) {
                (Arc::clone(existing), ProjectCacheResult::Coalesced)
            } else {
                if cache.inflight.len() >= self.cache_config.max_inflight {
                    Metrics.project_cache(ProjectCacheResult::CapacityRejected);
                    return Err(ProjectResolveError::Unavailable);
                }
                let cell = Arc::new(OnceCell::new());
                cache.inflight.insert(key, Arc::clone(&cell));
                (cell, ProjectCacheResult::Miss)
            }
        };
        Metrics.project_cache(cache_result);
        let result = shared
            .get_or_init(|| async {
                match self.store.load_project(key).await {
                    Ok(snapshot) if snapshot_is_active(&snapshot) => Ok(snapshot),
                    Ok(_) | Err(ProjectStoreError::NotFound) => {
                        Err(ProjectResolveError::Unauthorized)
                    }
                    Err(_) => Err(ProjectResolveError::Unavailable),
                }
            })
            .await
            .clone();

        let mut cache = self.cache.lock().expect("project cache lock poisoned");
        if self.cache_generation.load(Ordering::Acquire) == generation {
            let ttl = if result.is_ok() {
                self.cache_config.positive_ttl
            } else if result == Err(ProjectResolveError::Unauthorized) {
                self.cache_config.negative_ttl
            } else {
                Duration::ZERO
            };
            if !ttl.is_zero() {
                cache.insert(key, result.clone(), expires_at(now, ttl));
            }
        }
        if cache
            .inflight
            .get(&key)
            .is_some_and(|entry| Arc::ptr_eq(entry, &shared))
        {
            cache.inflight.remove(&key);
        }
        result
    }
}

impl ProjectResolver for ProjectService {
    fn resolve(&self, key: DsnKey) -> PortFuture<'_, LookupResult> {
        Box::pin(self.resolve_cached(key))
    }
}

fn snapshot_is_active(snapshot: &ProjectSnapshot) -> bool {
    snapshot.state == ProjectAcceptanceState::Active
        && snapshot.key_state == ProjectKeyState::Active
}

fn expires_at(now_millis: i64, ttl: Duration) -> i64 {
    let millis = i64::try_from(ttl.as_millis()).unwrap_or(i64::MAX);
    now_millis.saturating_add(millis)
}

fn map_command_store_error(error: ProjectStoreError) -> ProjectServiceError {
    match error {
        ProjectStoreError::OrganizationSlugExists | ProjectStoreError::ProjectSlugExists => {
            ProjectServiceError::AlreadyExists
        }
        ProjectStoreError::NotFound => ProjectServiceError::NotFound,
        ProjectStoreError::RevisionConflict => ProjectServiceError::AlreadyExists,
        ProjectStoreError::IdentityCollision | ProjectStoreError::KeyCollision => {
            ProjectServiceError::CollisionExhausted
        }
        ProjectStoreError::TooManyKeys
        | ProjectStoreError::InvalidData
        | ProjectStoreError::Unavailable => ProjectServiceError::Unavailable,
    }
}

#[derive(Clone)]
struct CacheEntry {
    value: LookupResult,
    expires_at: i64,
    generation: u64,
}

struct CacheState {
    entries: HashMap<DsnKey, CacheEntry>,
    order: VecDeque<(DsnKey, u64)>,
    inflight: HashMap<DsnKey, SharedLookup>,
    next_generation: u64,
    capacity: usize,
}

impl CacheState {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity.min(4096)),
            order: VecDeque::with_capacity(capacity.min(4096)),
            inflight: HashMap::new(),
            next_generation: 1,
            capacity,
        }
    }

    fn get(&mut self, key: DsnKey, now: i64) -> Option<LookupResult> {
        let entry = self.entries.get(&key)?.clone();
        if entry.expires_at <= now {
            self.entries.remove(&key);
            return None;
        }
        let generation = self.take_generation();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.generation = generation;
        }
        self.order.push_back((key, generation));
        self.trim_order();
        Some(entry.value)
    }

    fn insert(&mut self, key: DsnKey, value: LookupResult, expires_at: i64) {
        let generation = self.take_generation();
        self.entries.insert(
            key,
            CacheEntry {
                value,
                expires_at,
                generation,
            },
        );
        self.order.push_back((key, generation));
        while self.entries.len() > self.capacity {
            self.evict_oldest();
        }
        self.trim_order();
    }

    fn invalidate(&mut self, key: DsnKey) {
        self.entries.remove(&key);
    }

    fn trim_order(&mut self) {
        while self.order.len() > self.capacity.saturating_mul(2) {
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) {
        if let Some((key, generation)) = self.order.pop_front()
            && self
                .entries
                .get(&key)
                .is_some_and(|entry| entry.generation == generation)
        {
            self.entries.remove(&key);
        }
    }

    fn take_generation(&mut self) -> u64 {
        let generation = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metric_domain::{ScrubPolicy, SecretBytes, Timestamp};
    use metric_ports::{ProjectStoreError, RandomError};
    use std::sync::atomic::{AtomicI64, AtomicUsize};

    struct LookupStore {
        calls: AtomicUsize,
        organization_collisions: AtomicUsize,
        project_collisions: AtomicUsize,
        key_collisions: AtomicUsize,
        result: Mutex<Result<ProjectSnapshot, ProjectStoreError>>,
        delay: Duration,
    }

    impl LookupStore {
        fn active() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                organization_collisions: AtomicUsize::new(0),
                project_collisions: AtomicUsize::new(0),
                key_collisions: AtomicUsize::new(0),
                result: Mutex::new(Ok(snapshot(1))),
                delay: Duration::ZERO,
            }
        }
    }

    impl ProjectStore for LookupStore {
        fn insert_organization(
            &self,
            _organization: OrganizationIdentity,
        ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
            Box::pin(async move {
                if take_collision(&self.organization_collisions) {
                    Err(ProjectStoreError::IdentityCollision)
                } else {
                    Ok(())
                }
            })
        }

        fn insert_project(
            &self,
            _project: ProjectIdentity,
        ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
            Box::pin(async move {
                if take_collision(&self.project_collisions) {
                    Err(ProjectStoreError::IdentityCollision)
                } else {
                    Ok(())
                }
            })
        }

        fn insert_project_key(
            &self,
            _key: ProjectKeyIdentity,
        ) -> PortFuture<'_, Result<(), ProjectStoreError>> {
            Box::pin(async move {
                if take_collision(&self.key_collisions) {
                    Err(ProjectStoreError::KeyCollision)
                } else {
                    Ok(())
                }
            })
        }

        fn load_project(
            &self,
            _key: DsnKey,
        ) -> PortFuture<'_, Result<ProjectSnapshot, ProjectStoreError>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                if !self.delay.is_zero() {
                    tokio::time::sleep(self.delay).await;
                }
                self.result.lock().unwrap().clone()
            })
        }

        fn set_key_state(
            &self,
            _key: DsnKey,
            _state: ProjectKeyState,
        ) -> PortFuture<'_, Result<ProjectId, ProjectStoreError>> {
            Box::pin(async { Ok(ProjectId::new(42).unwrap()) })
        }

        fn set_project_acceptance(
            &self,
            _project_id: ProjectId,
            _state: ProjectAcceptanceState,
        ) -> PortFuture<'_, Result<Vec<DsnKey>, ProjectStoreError>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    struct TestClock(AtomicI64);

    fn take_collision(counter: &AtomicUsize) -> bool {
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_sub(1)
            })
            .is_ok()
    }

    impl Clock for TestClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_unix_millis(self.0.load(Ordering::Relaxed)).unwrap()
        }
    }

    struct TestRandom;

    impl RandomSource for TestRandom {
        fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
            output.fill(1);
            Ok(())
        }
    }

    struct ZeroThenOneRandom(AtomicUsize);

    impl RandomSource for ZeroThenOneRandom {
        fn fill_bytes(&self, output: &mut [u8]) -> Result<(), RandomError> {
            let byte = if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
                0
            } else {
                1
            };
            output.fill(byte);
            Ok(())
        }
    }

    fn snapshot(project: i32) -> ProjectSnapshot {
        ProjectSnapshot {
            project_id: ProjectId::new(project).unwrap(),
            organization_id: metric_domain::OrganizationId::new(1).unwrap(),
            state: ProjectAcceptanceState::Active,
            key_state: ProjectKeyState::Active,
            scrub_policy: ScrubPolicy {
                revision: 1,
                ip_policy: IpScrubPolicy::Hmac,
                hmac_key: SecretBytes::new([7; 32]),
            },
            items: ItemCapabilities {
                error: true,
                client_report: true,
                log: true,
                transaction: true,
                span: true,
                feedback: true,
            },
            limits: ProjectIngestLimits::default(),
            inbound_filters: Default::default(),
            grouping_revision: 1,
        }
    }

    fn service(store: Arc<LookupStore>, clock: Arc<TestClock>, capacity: usize) -> ProjectService {
        ProjectService::new(
            store,
            clock,
            Arc::new(TestRandom),
            4,
            ProjectCacheConfig {
                capacity,
                max_inflight: 8,
                positive_ttl: Duration::from_secs(60),
                negative_ttl: Duration::from_secs(5),
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn concurrent_misses_are_coalesced() {
        let store = Arc::new(LookupStore {
            delay: Duration::from_millis(10),
            ..LookupStore::active()
        });
        let service = Arc::new(service(
            Arc::clone(&store),
            Arc::new(TestClock(AtomicI64::new(0))),
            16,
        ));
        let key = DsnKey::from_bytes([1; 16]);
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let service = Arc::clone(&service);
            tasks.push(tokio::spawn(async move { service.resolve(key).await }));
        }
        for task in tasks {
            assert!(task.await.unwrap().is_ok());
        }
        assert_eq!(store.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn positive_and_negative_ttl_and_capacity_are_bounded() {
        let store = Arc::new(LookupStore::active());
        let clock = Arc::new(TestClock(AtomicI64::new(0)));
        let service = service(Arc::clone(&store), Arc::clone(&clock), 2);
        for byte in 1..=3 {
            assert!(
                service
                    .resolve(DsnKey::from_bytes([byte; 16]))
                    .await
                    .is_ok()
            );
        }
        assert_eq!(service.cached_entries(), 2);
        let calls = store.calls.load(Ordering::Relaxed);
        clock.0.store(61_000, Ordering::Relaxed);
        assert!(service.resolve(DsnKey::from_bytes([3; 16])).await.is_ok());
        assert_eq!(store.calls.load(Ordering::Relaxed), calls + 1);

        *store.result.lock().unwrap() = Err(ProjectStoreError::NotFound);
        let missing = DsnKey::from_bytes([9; 16]);
        assert_eq!(
            service.resolve(missing).await,
            Err(ProjectResolveError::Unauthorized)
        );
        assert_eq!(
            service.resolve(missing).await,
            Err(ProjectResolveError::Unauthorized)
        );
        assert_eq!(store.calls.load(Ordering::Relaxed), calls + 2);
        clock.0.store(67_000, Ordering::Relaxed);
        assert_eq!(
            service.resolve(missing).await,
            Err(ProjectResolveError::Unauthorized)
        );
        assert_eq!(store.calls.load(Ordering::Relaxed), calls + 3);
    }

    #[tokio::test]
    async fn distinct_inflight_misses_are_bounded() {
        let store = Arc::new(LookupStore {
            delay: Duration::from_millis(25),
            ..LookupStore::active()
        });
        let service = Arc::new(
            ProjectService::new(
                Arc::clone(&store) as Arc<dyn ProjectStore>,
                Arc::new(TestClock(AtomicI64::new(0))),
                Arc::new(TestRandom),
                4,
                ProjectCacheConfig {
                    capacity: 16,
                    max_inflight: 1,
                    positive_ttl: Duration::from_secs(60),
                    negative_ttl: Duration::from_secs(5),
                },
            )
            .unwrap(),
        );
        let first_service = Arc::clone(&service);
        let first =
            tokio::spawn(async move { first_service.resolve(DsnKey::from_bytes([1; 16])).await });
        while store.calls.load(Ordering::Relaxed) == 0 {
            tokio::task::yield_now().await;
        }
        assert_eq!(
            service.resolve(DsnKey::from_bytes([2; 16])).await,
            Err(ProjectResolveError::Unavailable)
        );
        assert!(first.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn invalidation_removes_an_authorized_snapshot() {
        let store = Arc::new(LookupStore::active());
        let service = service(
            Arc::clone(&store),
            Arc::new(TestClock(AtomicI64::new(0))),
            4,
        );
        let key = DsnKey::from_bytes([1; 16]);
        assert!(service.resolve(key).await.is_ok());
        *store.result.lock().unwrap() = Err(ProjectStoreError::NotFound);
        service
            .set_key_state(key, ProjectKeyState::Disabled)
            .await
            .unwrap();
        assert_eq!(
            service.resolve(key).await,
            Err(ProjectResolveError::Unauthorized)
        );
    }

    #[tokio::test]
    async fn generated_identity_and_key_collisions_are_retried() {
        let store = Arc::new(LookupStore::active());
        store.organization_collisions.store(1, Ordering::Relaxed);
        store.project_collisions.store(1, Ordering::Relaxed);
        store.key_collisions.store(1, Ordering::Relaxed);
        let service = service(
            Arc::clone(&store),
            Arc::new(TestClock(AtomicI64::new(0))),
            4,
        );
        let organization_id = service
            .create_organization(CreateOrganization {
                slug: Slug::new("acme").unwrap(),
                display_name: DisplayName::new("Acme").unwrap(),
            })
            .await
            .unwrap();
        let project = service
            .create_project(CreateProject {
                organization_id,
                slug: Slug::new("backend").unwrap(),
                display_name: DisplayName::new("Backend").unwrap(),
                ip_policy: IpScrubPolicy::Hmac,
                items: ItemCapabilities {
                    error: true,
                    client_report: true,
                    log: true,
                    transaction: true,
                    span: true,
                    feedback: true,
                },
                limits: ProjectIngestLimits::default(),
            })
            .await
            .unwrap();
        assert!(project.project_id.get() > 0);
        assert_eq!(project.dsn_key, DsnKey::from_bytes([1; 16]));
        assert_eq!(store.organization_collisions.load(Ordering::Relaxed), 0);
        assert_eq!(store.project_collisions.load(Ordering::Relaxed), 0);
        assert_eq!(store.key_collisions.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn generated_zero_identifier_is_retried() {
        let store = Arc::new(LookupStore::active());
        let service = ProjectService::new(
            Arc::clone(&store) as Arc<dyn ProjectStore>,
            Arc::new(TestClock(AtomicI64::new(0))),
            Arc::new(ZeroThenOneRandom(AtomicUsize::new(0))),
            2,
            ProjectCacheConfig {
                capacity: 4,
                max_inflight: 1,
                positive_ttl: Duration::from_secs(60),
                negative_ttl: Duration::from_secs(5),
            },
        )
        .unwrap();
        let id = service
            .create_organization(CreateOrganization {
                slug: Slug::new("retry-zero").unwrap(),
                display_name: DisplayName::new("Retry Zero").unwrap(),
            })
            .await
            .unwrap();
        assert!(id.get() > 0);
    }

    #[tokio::test]
    #[ignore = "performance baseline runs in release mode"]
    async fn performance_project_cache_hit_rps() {
        let store = Arc::new(LookupStore::active());
        let service = service(store, Arc::new(TestClock(AtomicI64::new(0))), 100);
        let key = DsnKey::from_bytes([1; 16]);
        service.resolve(key).await.unwrap();
        let iterations = 200_000_u64;
        let started = std::time::Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(service.resolve(key).await.unwrap());
        }
        let elapsed = started.elapsed();
        let rps = iterations as f64 / elapsed.as_secs_f64();
        let average_nanos = elapsed.as_nanos() as f64 / iterations as f64;
        eprintln!("project cache hit: {rps:.0} lookups/s, {average_nanos:.0} ns average");
        assert!(rps >= 20_000.0, "cache baseline {rps:.0} RPS is below gate");
    }
}
