//! Shared real-adapter regression for preserving an active organization owner.
use metric_domain::{
    DisplayName, OrganizationId, OrganizationIdentity, Slug, Timestamp,
    auth::{
        BootstrapIdentity, CredentialId, EmailAddress, MembershipMutation, MembershipMutationKind,
        OrganizationMembership, OrganizationRole, PasswordHash, SecretDigest, SetupPurpose,
        SetupToken, UserAccount, UserDisplayName, UserId,
    },
};
use metric_ports::{AuthStore, AuthStoreError, BootstrapTokenInstall};

/// Run against an empty disposable database, with the same assertions on each backend.
pub async fn active_owner_guard(
    store: &dyn AuthStore,
    now: Timestamp,
) -> Result<(), AuthStoreError> {
    let first = user_id(1);
    let second = user_id(2);
    let org = organization_id(1);
    let user = |id: UserId| UserAccount {
        id,
        email: EmailAddress::parse(format!("owner{}@example.invalid", id.get())).unwrap(),
        display_name: UserDisplayName::new("Synthetic owner").unwrap(),
        password_hash: Some(PasswordHash::new("$argon2id$synthetic-fixture-hash").unwrap()),
        disabled_at: None,
        created_at: now,
    };
    let membership = |id, organization_id| OrganizationMembership {
        organization_id,
        user_id: id,
        role: OrganizationRole::Owner,
        created_at: now,
        created_by: first,
    };
    let setup = |id: u64, purpose, user_id| SetupToken {
        id: credential_id(id),
        digest: SecretDigest::new([id as u8; 32]),
        purpose,
        user_id,
        created_at: now,
        expires_at: Timestamp::from_unix_millis(now.unix_millis() + 3_600_000).unwrap(),
        consumed_at: None,
    };
    assert_eq!(
        store
            .install_bootstrap_token(setup(10, SetupPurpose::Bootstrap, None))
            .await?,
        BootstrapTokenInstall::Created
    );
    store
        .consume_bootstrap(BootstrapIdentity {
            operation_id: credential_id(11),
            token_digest: SecretDigest::new([10; 32]),
            organization_id: org,
            organization_slug: Slug::new("owner-guard-one").unwrap(),
            organization_name: DisplayName::new("Synthetic organization").unwrap(),
            user: user(first),
            membership: membership(first, org),
            timestamp: now,
        })
        .await?;
    store
        .create_invited_user(
            user(second),
            membership(second, org),
            setup(12, SetupPurpose::PasswordSetup, Some(second)),
        )
        .await?;

    // An enabled owner in a different organization must not satisfy this guard.
    let other_org = organization_id(2);
    store
        .create_owned_organization(
            OrganizationIdentity {
                id: other_org,
                slug: Slug::new("owner-guard-two").unwrap(),
                display_name: DisplayName::new("Other organization").unwrap(),
                created_at: now,
            },
            membership(first, other_org),
        )
        .await?;
    let third = user_id(3);
    store
        .create_invited_user(
            user(third),
            membership(third, other_org),
            setup(13, SetupPurpose::PasswordSetup, Some(third)),
        )
        .await?;

    let mutation = |id: UserId, kind, operation| MembershipMutation {
        organization_id: org,
        user_id: id,
        actor_user_id: second,
        operation_id: credential_id(operation),
        kind,
        timestamp: now,
    };
    store
        .set_user_disabled(first, Some(now), credential_id(20))
        .await?;
    store
        .set_user_disabled(first, Some(now), credential_id(21))
        .await?;
    assert_eq!(
        store
            .set_user_disabled(second, Some(now), credential_id(22))
            .await,
        Err(AuthStoreError::FinalOwner)
    );
    assert_eq!(
        store
            .mutate_membership(mutation(
                second,
                MembershipMutationKind::ChangeRole(OrganizationRole::Admin),
                23
            ))
            .await,
        Err(AuthStoreError::FinalOwner)
    );
    assert_eq!(
        store
            .mutate_membership(mutation(second, MembershipMutationKind::Remove, 24))
            .await,
        Err(AuthStoreError::FinalOwner)
    );
    assert_eq!(store.load_user(second).await?.disabled_at, None);

    // Removing/demoting an already-disabled owner does not remove an active owner.
    store
        .mutate_membership(mutation(
            first,
            MembershipMutationKind::ChangeRole(OrganizationRole::Member),
            25,
        ))
        .await?;
    store
        .mutate_membership(mutation(
            first,
            MembershipMutationKind::ChangeRole(OrganizationRole::Owner),
            26,
        ))
        .await?;
    store
        .mutate_membership(mutation(first, MembershipMutationKind::Remove, 27))
        .await?;
    store
        .mutate_membership(mutation(
            first,
            MembershipMutationKind::Create(OrganizationRole::Owner),
            28,
        ))
        .await?;
    store
        .set_user_disabled(first, None, credential_id(29))
        .await?;

    // Mongo may surface bounded lock contention; retry the losing operation after
    // its competitor completed, then require the authoritative FinalOwner rejection.
    let (one, two) = tokio::join!(
        store.set_user_disabled(first, Some(now), credential_id(30)),
        store.set_user_disabled(second, Some(now), credential_id(31)),
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    let (loser, operation, result) = if one.is_ok() {
        (second, 31, two)
    } else {
        (first, 30, one)
    };
    let result = if result == Err(AuthStoreError::Unavailable) {
        store
            .set_user_disabled(loser, Some(now), credential_id(operation))
            .await
    } else {
        result
    };
    assert_eq!(result, Err(AuthStoreError::FinalOwner));
    let active = usize::from(store.load_user(first).await?.disabled_at.is_none())
        + usize::from(store.load_user(second).await?.disabled_at.is_none());
    assert_eq!(active, 1);
    Ok(())
}

fn user_id(value: u64) -> UserId {
    UserId::new(value).unwrap()
}
fn organization_id(value: u64) -> OrganizationId {
    OrganizationId::new(value).unwrap()
}
fn credential_id(value: u64) -> CredentialId {
    CredentialId::new(value).unwrap()
}
