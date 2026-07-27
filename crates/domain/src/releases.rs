use crate::{
    EventId, OrganizationId, ProjectId, Timestamp,
    finalization::{ReleaseId, derive_release_id},
    issue::IssueRelease,
};
use std::fmt;

pub const MAX_RELEASE_PROJECTS: usize = 256;
pub const MAX_RELEASE_REPOSITORIES: usize = 16;
pub const MAX_RELEASE_VERSION_BYTES: usize = 200;
pub const MAX_RELEASE_REFERENCE_BYTES: usize = 200;
pub const MAX_RELEASE_URL_BYTES: usize = 2_048;
pub const MAX_DEPLOY_ENVIRONMENT_BYTES: usize = 64;
pub const MAX_DEPLOY_NAME_BYTES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeployId([u8; 16]);

impl DeployId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Display for DeployId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&hex::encode(self.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryReference {
    pub repository: Box<str>,
    pub commit_from: Option<Box<str>>,
    pub commit_to: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRelease {
    pub organization_id: OrganizationId,
    pub project_ids: Vec<ProjectId>,
    pub version: Box<str>,
    pub url: Option<Box<str>>,
    pub reference: Option<Box<str>>,
    pub repositories: Vec<RepositoryReference>,
    pub created_at: Timestamp,
}

impl CreateRelease {
    pub fn validate(&self) -> Result<ReleaseId, ReleaseValueError> {
        validate_version(&self.version)?;
        validate_projects(&self.project_ids)?;
        validate_optional(&self.url, MAX_RELEASE_URL_BYTES)?;
        validate_optional(&self.reference, MAX_RELEASE_REFERENCE_BYTES)?;
        if self.repositories.len() > MAX_RELEASE_REPOSITORIES {
            return Err(ReleaseValueError);
        }
        for repository in &self.repositories {
            validate_required(&repository.repository, MAX_RELEASE_REFERENCE_BYTES)?;
            validate_optional(&repository.commit_from, MAX_RELEASE_REFERENCE_BYTES)?;
            validate_optional(&repository.commit_to, MAX_RELEASE_REFERENCE_BYTES)?;
        }
        Ok(derive_release_id(self.organization_id, &self.version))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FinalizeRelease {
    pub organization_id: OrganizationId,
    pub release_id: ReleaseId,
    pub released_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateDeploy {
    pub deploy_id: DeployId,
    pub organization_id: OrganizationId,
    pub release_id: ReleaseId,
    pub project_ids: Vec<ProjectId>,
    pub environment: Box<str>,
    pub name: Option<Box<str>>,
    pub url: Option<Box<str>>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

impl CreateDeploy {
    pub fn validate(&self) -> Result<(), ReleaseValueError> {
        validate_projects(&self.project_ids)?;
        validate_required(&self.environment, MAX_DEPLOY_ENVIRONMENT_BYTES)?;
        validate_optional(&self.name, MAX_DEPLOY_NAME_BYTES)?;
        validate_optional(&self.url, MAX_RELEASE_URL_BYTES)?;
        if self
            .finished_at
            .is_some_and(|finished| finished < self.started_at)
        {
            return Err(ReleaseValueError);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRecord {
    pub id: ReleaseId,
    pub organization_id: OrganizationId,
    pub version: Box<str>,
    pub project_ids: Vec<ProjectId>,
    pub created_at: Timestamp,
    pub activity_at: Timestamp,
    pub released_at: Option<Timestamp>,
    pub first_seen: Option<Timestamp>,
    pub last_seen: Option<Timestamp>,
    pub first_event_id: Option<EventId>,
    pub latest_event_id: Option<EventId>,
    pub url: Option<Box<str>>,
    pub reference: Option<Box<str>>,
    pub repositories: Vec<RepositoryReference>,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployRecord {
    pub id: DeployId,
    pub release_id: ReleaseId,
    pub project_ids: Vec<ProjectId>,
    pub environment: Box<str>,
    pub name: Option<Box<str>>,
    pub url: Option<Box<str>>,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIssueSummary {
    pub issue_id: crate::grouping::IssueId,
    pub title: crate::issue::IssueTitle,
    pub first_seen: Timestamp,
    pub last_seen: Timestamp,
    pub first_release: Option<IssueRelease>,
    pub last_release: Option<IssueRelease>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseValueError;

pub fn derive_deploy_id(
    organization_id: OrganizationId,
    release_id: ReleaseId,
    operation_id: [u8; 16],
) -> DeployId {
    let mut derivation = blake3::Hasher::new();
    derivation.update(b"metric/deploy-id/v1");
    derivation.update(&organization_id.get().to_be_bytes());
    derivation.update(&release_id.as_bytes());
    derivation.update(&operation_id);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&derivation.finalize().as_bytes()[..16]);
    DeployId(bytes)
}

pub fn validate_version(value: &str) -> Result<(), ReleaseValueError> {
    validate_required(value, MAX_RELEASE_VERSION_BYTES)
}

fn validate_projects(projects: &[ProjectId]) -> Result<(), ReleaseValueError> {
    if projects.is_empty() || projects.len() > MAX_RELEASE_PROJECTS {
        return Err(ReleaseValueError);
    }
    let mut sorted = projects.to_vec();
    sorted.sort_unstable_by_key(|project| project.get());
    sorted.dedup();
    if sorted.len() != projects.len() {
        return Err(ReleaseValueError);
    }
    Ok(())
}

fn validate_required(value: &str, maximum: usize) -> Result<(), ReleaseValueError> {
    if value.is_empty() || value.len() > maximum {
        Err(ReleaseValueError)
    } else {
        Ok(())
    }
}

fn validate_optional(value: &Option<Box<str>>, maximum: usize) -> Result<(), ReleaseValueError> {
    match value {
        Some(value) => validate_required(value, maximum),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{hint::black_box, time::Instant};

    #[test]
    fn release_and_deploy_identities_are_deterministic_and_scoped() {
        let organization = OrganizationId::new(7).unwrap();
        let release = derive_release_id(organization, "backend@1");
        assert_eq!(
            derive_deploy_id(organization, release, [9; 16]),
            derive_deploy_id(organization, release, [9; 16])
        );
        assert_ne!(
            derive_deploy_id(organization, release, [9; 16]),
            derive_deploy_id(organization, release, [8; 16])
        );
    }

    #[test]
    fn release_metadata_is_bounded() {
        let command = CreateRelease {
            organization_id: OrganizationId::new(7).unwrap(),
            project_ids: vec![ProjectId::new(42).unwrap()],
            version: "backend@1".into(),
            url: None,
            reference: None,
            repositories: Vec::new(),
            created_at: Timestamp::from_unix_millis(1).unwrap(),
        };
        assert!(command.validate().is_ok());
        let mut invalid = command;
        invalid.version = "x".repeat(MAX_RELEASE_VERSION_BYTES + 1).into();
        assert!(invalid.validate().is_err());
    }

    #[test]
    #[ignore = "retained release-mode Phase 29 RPS baseline"]
    fn performance_release_identity_rps() {
        const OPERATIONS: usize = 100_000;
        let organization = OrganizationId::new(7).unwrap();
        let project = ProjectId::new(42).unwrap();
        let command = CreateRelease {
            organization_id: organization,
            project_ids: vec![project],
            version: "backend@2.4.0".into(),
            url: Some("https://ci.example/releases/backend-2.4.0".into()),
            reference: Some("0123456789abcdef".into()),
            repositories: vec![RepositoryReference {
                repository: "acme/backend".into(),
                commit_from: Some("1111111111111111111111111111111111111111".into()),
                commit_to: Some("2222222222222222222222222222222222222222".into()),
            }],
            created_at: Timestamp::from_unix_millis(1_700_000_000_000).unwrap(),
        };
        let started = Instant::now();
        for index in 0..OPERATIONS {
            let release = black_box(command.validate().unwrap());
            black_box(derive_deploy_id(
                organization,
                release,
                u128::try_from(index).unwrap().to_be_bytes(),
            ));
        }
        let elapsed = started.elapsed();
        let rps = OPERATIONS as f64 / elapsed.as_secs_f64();
        eprintln!(
            "Phase 29 Release identity: rps={rps:.0},operations={OPERATIONS},elapsed_ms={}",
            elapsed.as_millis()
        );
        assert!(rps >= 100_000.0, "{rps:.0} RPS below local gate");
    }
}
