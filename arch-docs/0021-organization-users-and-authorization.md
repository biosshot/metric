# ADR-0021: Organizations, users, sessions, API tokens, and authorization

- Status: Accepted
- Date: 2026-07-21

## Context

Sentry SDK ingestion, interactive Web access, HTTP API automation, and a future MCP
adapter have different trust models. A public DSN key must remain limited to event
ingestion, while users and automation require revocable read and command permissions.

The first version is self-hosted and must work without SMTP, an external identity
provider, Redis, or another authorization service. Authorization must remain simple
enough for the single-process runtime while preserving organization isolation and a
clean path to future teams, service accounts, and SSO.

## Decision

### Identity and tenancy model

Users are global identities and may belong to multiple organizations. Projects belong
to exactly one organization. Membership is represented separately rather than copied
into user or organization documents.

The control plane uses these MongoDB collections:

```text
organizations
users
organization_memberships
web_sessions
api_tokens
password_setup_tokens
audit_log
```

Organization and user identifiers use random positive `u63` values. Unlike the
per-Event project reference optimized to `i31` by ADR-0022, these control-plane IDs do
not justify changing width. They are not authentication secrets. MongoDB uniqueness
is the final collision check and generation retries on conflict.

`users.canonical_email` is globally unique and is used for login lookup. The original
display form is stored separately. Password hashes and authentication tokens are
never embedded in organization or membership documents.

Organizations and projects also have the stable Sentry-compatible slugs defined by
ADR-0025. Organization slugs are globally unique; project slugs are unique within an
organization. They are control-plane route keys and are not copied into Event or
Issue documents.

Each membership contains at least:

```javascript
{
  organization_id,
  user_id,
  role,
  created_at,
  created_by
}
```

A unique compound index on `(organization_id, user_id)` prevents duplicate
memberships. Every project query and command derives organization scope from its
authorized project, not from an untrusted organization value in the request body.

### Organization roles

Version one defines four roles:

```text
owner   full control, including organization deletion and owner management
admin   membership, project, key, integration, and policy administration
member  event/issue access and ordinary issue commands
viewer  read-only access
```

All members of an organization can access all of its projects according to their
role. Teams, per-project membership, custom roles, and field-level policy are not
implemented initially.

An organization must always have at least one owner. Creating another owner is
allowed, but removal, disabling, or demotion of the final owner is rejected
atomically. An admin cannot delete the organization or grant and remove owner status.

Authorization is deny-by-default. Internally, roles expand into a typed `Permission`
set. Application code checks permissions rather than comparing role names at each
endpoint.

### Password authentication

Passwords are stored with Argon2id. The encoded hash contains its algorithm, unique
random salt, and cost parameters so a successful login can upgrade an older hash.
Initial minimum parameters are:

```text
memory      = 19 MiB
iterations  = 2
parallelism = 1
```

Operators may configure stronger values. The implementation enforces safe minima and
bounded maxima to prevent accidental denial of service. Passwords are never logged,
stored reversibly, or included in audit metadata. A global password pepper is not
required in version one because its backup and rotation lifecycle would add another
mandatory secret-management dependency.

Login uses a generic failure response and bounded rate limits keyed by both account
and client network identity. Disabled users cannot create sessions or use existing
sessions and personal tokens.

### Browser sessions

Web authentication uses revocable, server-side opaque sessions rather than JWTs. On
login, the server creates 32 cryptographically random bytes, sends the plaintext only
in a cookie, and persists only a cryptographic digest as the session key.

The cookie uses:

```text
HttpOnly
Secure in HTTPS deployments
SameSite=Lax
Path=/
```

Production configuration refuses an insecure cookie mode unless explicitly marked as
a local-development deployment. State-changing browser requests also require a CSRF
token and use non-GET methods.

Each session records the user, creation time, last meaningful activity, idle expiry,
absolute expiry, and revocation state. Defaults are configurable:

```toml
[auth.session]
idle_timeout = "7d"
absolute_timeout = "30d"
```

The application checks expiry on every authenticated request. A MongoDB TTL index is
only asynchronous cleanup and is not an authorization mechanism. `last_seen_at`
writes are coalesced so ordinary page requests do not continuously update MongoDB.

Login, password change, privilege-sensitive account changes, and explicit logout
rotate or revoke applicable sessions. Session identifiers are not stored in browser
local storage, URLs, logs, or audit documents.

Version one performs authoritative session and membership reads without a positive
authorization cache. This favors immediate revocation and simple correctness; a
bounded cache may be added only with an explicit invalidation design.

### Personal API tokens

CLI and HTTP API automation authenticate with personal API tokens. A future MCP
adapter will reuse those tokens. A token contains at least 32 cryptographically
random bytes, is displayed only once, and is stored only as a cryptographic digest
plus non-secret metadata.

A token is bound to one user and one organization and contains:

```javascript
{
  token_id,
  token_digest,
  user_id,
  organization_id,
  name,
  scopes,
  created_at,
  expires_at,
  last_used_at,
  revoked_at
}
```

`last_used_at` updates are coalesced. Expiry is required by policy but its configured
maximum may allow long-lived self-hosted automation tokens. Revocation, user disable,
membership removal, or token expiry prevents further use immediately.

Token scopes use stable capability names such as:

```text
event:read
issue:read
issue:write
project:read
project:admin
debug_file:read
debug_file:write
debug_file:delete
artifact:read
artifact:write
artifact:delete
organization:admin
```

The effective permissions are the intersection of the token scopes and the user's
current membership permissions. A token therefore cannot retain a permission after
its owner is demoted, and a user cannot mint a token broader than their role.

Service accounts, organization-owned tokens, OAuth, and SSO/OIDC are deferred.

### Shared authorization boundary

Successful authentication creates an application-owned context conceptually shaped
as:

```rust
struct AuthContext {
    actor: Actor,
    user_id: UserId,
    organization_id: OrganizationId,
    role: OrganizationRole,
    permissions: PermissionSet,
    credential_id: CredentialId,
}
```

Web, HTTP API, and CLI call the same query and command services with this context. A
future MCP adapter must call those identical services. They resolve project ownership
and enforce the required typed permission before calling Storage. Storage operations
used by these services accept an organization/project scope rather than an arbitrary
raw identifier whenever that prevents an unscoped query.

The future MCP adapter uses ordinary API tokens. It never authenticates with a DSN,
receives MongoDB credentials, issues arbitrary collection queries, or bypasses
application services. Each future MCP tool declares its required permission.
Destructive operations additionally use the same explicit command validation and
idempotency behavior as the HTTP API.

MCP transport, tool schemas, discovery, and server runtime are not implemented in the
initial milestone. Initial work only keeps application queries and commands,
`AuthContext`, stable permissions, audit events, and destructive confirmations
transport-neutral. MCP can therefore be added later as a thin adapter rather than a
privileged parallel backend.

The project-bound Symbolicator callback credential from ADR-0026 is a separate
internal service credential. It is accepted only by the private debug-file read
route, is derived from a deployment secret rather than stored in `api_tokens`, and
grants no Web, API, MCP, Issue, project-command, or ingestion permission.

DSN project keys remain governed by ADR-0019 and authorize ingestion only.

### Bootstrap and password setup without email

When no user exists, the server enables a one-time bootstrap operation and emits one
cryptographically random setup token at startup. Only its digest is persisted. The
token creates the first user, organization, and owner membership in one guarded
operation. Once a user exists or the token is consumed, bootstrap cannot be used
again; concurrent attempts have a single winner.

Administrators create later users with a one-time password-setup link that can be
copied manually. Password setup and administrator-initiated reset tokens are stored
only as digests, are single-use, and expire after 24 hours by default. SMTP is
therefore optional rather than required for account administration.

An administrator may reissue an invitation while the invited account has no
password. The new password-setup token replaces every older unconsumed setup token
for that user. Once the account has a password, the invitation operation cannot
silently become a password reset; the explicit administrator reset flow is required.

### Audit log

Security and administrative mutations append an `audit_log` record containing the
organization, actor, action, target identifiers, timestamp, request correlation ID,
and bounded safe metadata. Audited actions include:

- successful login, password setup, and administrative reset;
- membership creation, removal, and role changes;
- API-token creation and revocation;
- project-key, retention, PII, integration, and webhook policy changes;
- project and organization deletion commands.

Raw passwords, session tokens, API tokens, DSN secret material, event payloads, and
unbounded request data are never audited. Issue activity remains a separate product
history because it has different readers and retention semantics.

## Consequences

- A public SDK credential cannot become a read or administration credential.
- Web sessions and automation tokens can be revoked without a JWT denylist.
- One user can work in several organizations without duplicating their identity.
- The first permission model remains small because every membership covers all
  projects in its organization.
- A future MCP adapter has a prepared authorization boundary, but no MCP runtime is
  required for the initial implementation.
- Initial and recovery account setup work without an email server.
- Authoritative auth reads add MongoDB work to interactive/API traffic, but not to the
  high-volume DSN ingest path.

## Deferred questions

- Teams, per-project access, custom roles, and guest users.
- Service accounts and organization-owned automation tokens.
- SSO/OIDC, SCIM, MFA, passkeys, and recovery codes.
- SMTP delivery of invitations and password-reset links.
- Distributed authorization-cache invalidation after runtime roles are split.
- Audit-log retention, export, and optional tamper-evident chaining.
