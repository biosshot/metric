# ADR-0011: Mandatory pre-storage PII scrubbing

- Status: Accepted
- Date: 2026-07-20

## Context

SDK error payloads may contain authentication headers, cookies, request bodies,
credentials, personal identifiers, local paths, and arbitrary unknown structures.
Scrubbing after MongoDB insertion would leave an unsanitized durable copy and would
also expose the data to the Processor backlog, archives, logs, and future disk spool.

Sentry's current Relay project contains a capable Rust PII implementation, but its
license history and suitability for use in a competing product require a separate
legal review. The initial architecture must not depend on that implementation.

## Decision

### Scrubbing boundary

PII scrubbing runs after bounded Envelope parsing, project authentication, and
acceptance-critical structural validation, but before any durable write:

```text
HTTP -> bounded decode -> parse -> authenticate -> scrub -> MongoDB / BlobStore
```

MongoDB, BlobStore, cold archives, the Processor queue, Web, MCP, and the future disk
spool receive only the scrubbed representation. No fallback or quarantine collection
stores the original unsanitized payload.

### Engine dispatch

Scrubbing uses enum dispatch:

```rust
pub enum ScrubEngine {
    Native(NativeScrubber),
}
```

The first implementation is written for this project. The current `relay-pii` crate
is not added without an explicit review of the exact dependency version and license.
There is no stable scrubber plugin ABI in the first version.

### Mandatory security floor

A non-disableable baseline replaces or removes common credential material, including:

- authorization and proxy-authorization values;
- cookies and set-cookie values;
- password, secret, access-token, refresh-token, API-key, and private-key fields;
- PEM private keys and bearer tokens;
- credentials embedded in URLs.

Project rules may strengthen this policy but cannot disable the mandatory floor.
Unknown event structures that are preserved for SDK compatibility are traversed by
the same bounded recursive scrubbing process.

### Project rules and actions

Project policies may match typed event paths, wildcard paths, field names, and bounded
patterns. Policies are validated and compiled when project configuration changes,
then cached as immutable shared state. Invalid policy updates are rejected while the
previous valid revision remains active.

The initial actions are:

- `replace`: substitute a marker such as `[Filtered]`;
- `remove`: remove the selected value;
- `hmac`: replace the value with a keyed stable pseudonymous identifier.

HMAC uses a versioned installation secret rather than an unkeyed digest. Secret
material is supplied through the deployment's secret-management mechanism and is not
included in event documents or logs.

### IP addresses

The default IP policy is `hmac`. Configurable alternatives are `keep`, `remove`, and
`truncate`. HMAC preserves stable correlation without retaining the original address,
but it is not represented as making the value legally anonymous.

### Audit metadata

Every accepted event records the applied policy revision:

```javascript
scrubbing: {
  policy_revision
}
```

The revision explains differences between events and supports scoped re-scrubbing
after a stricter policy is introduced. It cannot restore data removed by an earlier
policy.

### Failure behavior

Scrubbing is fail-closed. If the active policy cannot be loaded or safely applied,
the unsanitized event is not persisted and is not enqueued. Structural input errors
produce a client error; temporary policy availability and internal scrubber failures
produce an appropriate temporary or server error without logging the payload.

Application logs must not contain raw request bodies, Envelopes, event BSON,
attachment contents, or unrestricted request headers. Allowed diagnostic context is
limited to identifiers, item type, byte counts, policy revision, and machine-readable
error codes.

### Attachments

Attachment handling has three policy modes:

```toml
[attachments]
mode = "disabled" # disabled | scrubbable_only | allow
```

The default is `disabled` for arbitrary binary attachment storage.

`scrubbable_only` accepts only explicitly supported formats whose content can be
processed by a bounded format-specific scrubber, such as selected text, JSON, or
minidump formats. `allow` permits arbitrary binary content with an explicit privacy
warning and normal BlobStore access controls.

Attachment metadata is scrubbed in every enabled mode. Unsupported binary content is
not falsely reported as scrubbed.

## Consequences

- An accepted event has no intentionally retained unsanitized durable predecessor.
- Processor retries, archives, Incident Capsules, and MCP cannot bypass the accepted
  privacy policy.
- Stable HMAC identifiers preserve correlation while reducing exposure of original
  IP and selected identity values.
- A scrubber error loses the incoming event rather than leaking its raw secrets.
- Arbitrary SDK attachments require an explicit operator decision.
- Updating to stricter rules can re-scrub stored data, but loosening rules cannot
  recover removed values.
- Direct reuse of Sentry Relay PII code remains blocked on license review rather than
  becoming an accidental foundational dependency.

## Deferred questions

- Exact native selector grammar and built-in sensitive-name list.
- HMAC key provisioning, versioning, and rotation procedure.
- Format-specific attachment and minidump scrubbers.
- Attachment access authorization; parent-event commit is defined by ADR-0012.
- UI and API for testing a policy against locally supplied sample data without
  persisting that sample.
