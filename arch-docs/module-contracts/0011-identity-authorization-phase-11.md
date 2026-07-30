# Phase 11 contract: identity, credentials, authorization, and audit

> Historical phase contract, not an upgrade runbook. Do not recreate a data-bearing
> installation based on this document. The current binary requires generation 19;
> see [the current upgrade runbook](../../docs/upgrading.md).

- Status: accepted for implementation
- Date: 2026-07-23
- Owners: `domain::auth` (bounded identity and permission model),
  `application::auth` (authentication, authorization, rate limits and command
  policy), `ports::AuthStore` (capability-specific persistence), `mongo::auth`
  (authoritative identity/credential storage), `server` (validated configuration
  and composition)

## Responsibilities and exclusions

Phase 11 owns first-owner bootstrap, global users, organization memberships,
role-to-permission expansion, Argon2id password verification, opaque Web sessions,
CSRF validation, personal API tokens, authoritative authorization, final-owner
invariants and bounded administrative audit records.

It does not add Web screens, the Phase 12 `/api/v1` query/command DTO surface, MCP,
SMTP, teams, project-specific membership, service accounts, OAuth, SSO/OIDC, MFA,
passkeys, Redis, distributed authorization caches, migrations, NATS, sharding or
disk spool. DSN credentials remain ingest-only and never inhabit `AuthContext`.

## Domain model and authorization

User and organization identifiers are random positive `u63` values. Email display
form is bounded separately from its ASCII-lowercase canonical login key. Roles are
the closed `owner`, `admin`, `member`, and `viewer` set. They expand into a typed
append-only `Permission` registry; application services check permissions and never
compare role strings.

`AuthContext` contains the actor kind, user, organization, current role, effective
permission set and non-secret credential identifier. Session permissions equal the
current membership role. API-token permissions are the intersection of current
membership permissions and the token's stored scopes. Disabled users, removed
memberships, expired/revoked credentials and organization mismatches fail closed on
every request; there is no positive authorization cache.

Project authorization resolves the project's authoritative organization through a
separate scoped store operation before applying the permission check. Organization
or project values supplied by a caller cannot widen scope.

## Passwords, sessions, tokens, and rate limits

Argon2id hashes use a unique random salt and PHC encoding. Configuration enforces at
least 19 MiB memory, two iterations and parallelism one, with finite maxima. Password
work runs on a bounded blocking semaphore. Login has one generic authentication
failure regardless of unknown email, invalid password, disabled user or absent
membership; unknown users verify against a fixed dummy hash to avoid a lookup oracle.

The in-process login limiter uses both canonical-account and client-network digests,
has bounded entries, a fixed retry window and deterministic expiry. It does not emit
identity or network values to metrics or logs. Restart clearing this defensive
limiter is accepted for the single-process version.

Web session and personal-token secrets contain 32 random bytes, are displayed once,
and only SHA-256 digests are persisted. Session authentication checks idle and
absolute expiry, revocation and current user/membership state. Meaningful
`last_seen_at`/`last_used_at` writes are coalesced by a configured interval. Browser
mutations require exact constant-time CSRF verification against a second random
secret digest. Login, password changes and explicit logout revoke or rotate affected
sessions.

API tokens are organization-bound, always expire within the configured maximum and
cannot be created with a scope outside the actor's current permission set. Revocation
is authoritative immediately.

## Bootstrap, persistence, and audit

When no user exists, startup may create one bootstrap setup-token digest. Only the
plaintext token returned by that creation is operator-visible. Consumption is a
single-winner durable guarded operation. Because the supported development MongoDB
is standalone, the adapter records an operation identifier and idempotently upserts
the first user, organization and owner membership after atomically consuming the
digest. A retry resumes the same operation; another token or concurrent consumer
cannot create a second first owner. Once any user exists, bootstrap creation and
consumption fail closed.

Final-owner removal, demotion and disable are conditional MongoDB commands guarded
by an authoritative owner count and a short-lived organization mutation lock owned
by the adapter. The lock has a bounded expiry and operation identifier, so process
failure cannot permanently block administration. No application read-then-write
sequence claims atomic final-owner safety.

Collections are `users`, `organization_memberships`, `web_sessions`, `api_tokens`,
`password_setup_tokens`, and `audit_log`, in addition to existing `organizations`.
All have strict validators and exact required indexes. An empty database bootstraps
the new schema generation; existing pre-Phase-11 development data must be recreated,
as required by the no-migrations rule.

Administrative application commands append an idempotent audit record keyed by the
bounded request-correlation identifier. `AuditAction` and metadata keys are closed
allowlists. Values are bounded non-secret identifiers or static outcomes. Passwords,
password hashes, setup/session/API tokens and digests, DSN secrets, Event payloads
and arbitrary request bodies cannot be represented as audit metadata.

## Bounds, cancellation, and operability

Configuration bounds Argon2 cost and blocking concurrency, session idle/absolute
lifetimes, token/setup maximum lifetimes, coalesced activity intervals, login
attempts/window/entry capacity, audit metadata count/value size and identity
collision retries. Zero, inverted or unsafe values fail startup validation.

Each store call has an application deadline. Cancellation before a credential or
mutation reaches storage leaves no successful result. Credential touch failure does
not extend authorization; an otherwise valid request may proceed because stored
expiry remains authoritative. No detached worker or background queue is introduced.

Metrics use only credential kind, fixed operation, bounded outcome and stable error
code. User, organization, project, email, token/session identifier, IP/network,
audit target and request identifier are forbidden labels. Debug and error output
redacts every credential and password-derived value.

## Verification and performance gate

Verification covers role expansion and scope intersection, final-owner demotion/
disable/removal, disabled-user and token revocation, session idle/absolute expiry,
rotation/logout, CSRF, generic login failure, cross-organization/project matrices,
secret `Debug` redaction, audit allowlists and bounded rate-limiter state.

Real MongoDB integration covers bootstrap single-winner behavior, unique identity and
membership constraints, authoritative session/token revocation, final-owner guards,
strict validators/indexes and project-to-organization scope. One retained release
benchmark reports explicit RPS for the bounded login-rate-limit rejection path on
declared hardware. It is a security control regression sentinel rather than ingest
capacity and is compared only with identical configuration and fixture metadata.
