# ADR-0036: Sentry compatibility contract and minimal Web/API

- Status: Accepted
- Date: 2026-07-21

## Context

"Sentry compatible" can mean compatible Error Event ingestion, every Envelope item,
the entire Sentry REST API, or the complete Sentry product. Claiming all of those
would be untestable and conflict with the accepted decision to leave transactions,
replays, profiles, sessions, metrics, and other categories disabled initially.

The initial product still needs official SDKs to send errors without a custom client,
and users need a small stable API and Web surface for project setup and investigation.
Compatibility must be a versioned executable contract rather than an informal claim.

## Decision

### Compatibility claim

Version one claims Sentry SDK compatibility for the Error Event path:

```text
official SDK DSN/auth
-> Sentry store/envelope transport
-> Error Event acceptance and idempotency
-> normalized Event, Issue grouping and investigation API
```

It does not claim full Sentry product or REST API compatibility. Disabled Envelope
categories remain parsed and handled through ADR-0018's capability/partial-acceptance
contract so an SDK using a shared Envelope transport does not require a custom fork.

Feature-specific compatibility is advertised only after its module's contract suite
passes. Native minidumps, debug-file upload, and Artifact Bundles have their own
accepted endpoint decisions; transactions, spans, sessions, profiles, replays,
check-ins, StatsD/metrics, logs, and feedback remain disabled until explicitly added.

### Machine-readable compatibility matrix

The repository owns a versioned manifest conceptually shaped as:

```toml
[[sdk]]
name = "javascript"
version = "pinned-tested-version"
runtime = "node-or-browser-harness"
transport = "envelope"
error_event = "pass"
attachments = "disabled"
transactions = "disabled"
fixture_set = "javascript-v1"
```

Each row records exact SDK version, runtime, transport, auth form, compression,
enabled feature expectations, test fixture revision, and last passing server build.
"Untested" is distinct from "disabled" and "failed".

The initial matrix covers representative current stable releases from JavaScript
browser/Node, Python, Java/Kotlin/Android, .NET, Go, Rust, PHP, Ruby, Cocoa, React
Native, Flutter/Dart, and native C/C++ families for Error Events. A family is not
listed as supported merely because another SDK happens to serialize similar JSON.

Minimum/maximum supported SDK versions are published from passing rows, not guessed
from protocol age. New SDK releases enter scheduled compatibility CI before the
documented range changes.

### Compatibility test layers

The conformance corpus includes:

- real SDK processes sending to the server;
- captured golden store/envelope requests with safe synthetic data;
- DSN header/query/envelope authentication variants;
- gzip and every explicitly supported content encoding;
- exception chains, stack frames, messages, fingerprints, tags, user/request/context,
  breadcrumbs, release/dist/environment, platform and timestamps;
- duplicate Event IDs and conflicting retries;
- mixed supported/disabled items and client reports;
- size, malformed input and partial-acceptance behavior;
- response status, headers and retry semantics across SDK generations.

Fixtures are immutable after publication; a corrected expectation creates a new
fixture revision with an explanation. Fuzz-generated regressions are minimized and
added to the same corpus.

### Separate compatible and native APIs

Sentry-compatible routes live under their required `/api/0` and ingest paths. Native
product API routes use `/api/v1`. A native DTO is not forced to mimic a Sentry REST
response, and Sentry compatibility handlers do not expose internal MongoDB schemas.

Both adapters call shared application services. Authorization, validation,
idempotency, auditing, pagination, and destructive confirmation are not duplicated in
route handlers.

### Minimal native API

The first complete investigation surface includes:

```text
authentication
  session login/logout/current user
  personal token create/list/revoke

organizations/projects
  create/list/get project
  create/list/disable DSN key
  read/update accepted project policy
  project deletion status/cancel

issues
  list/search with keyset cursor
  get detail and hourly statistics
  resolve/reopen/ignore and activity history

events
  list by project or Issue with keyset cursor
  exact Event detail
  bounded Search v1 query

metadata
  Release and Environment lists needed by the investigation UI

system
  public live/ready probes
  authenticated capability and component status
```

Debug-file, artifact, attachment, archive, notification, and Incident Capsule routes
are mounted when their owning modules are enabled. Their absence is represented by
capabilities, not placeholder endpoints returning success.

### DTO and error rules

Native API JSON uses descriptive stable names, explicit optional values, UTC RFC3339
timestamps, opaque cursors, and string-rendered identifiers where JavaScript numeric
precision could be lost. Compact BSON field names and packed enum words never cross
the API boundary.

Native errors have a bounded envelope:

```json
{
  "error": {
    "code": "stable_machine_code",
    "message": "safe human message",
    "request_id": "opaque-correlation-id",
    "details": {}
  }
}
```

`details` is error-specific and bounded. Stack traces, database errors, secrets, raw
payload values, and backend responses are not returned. Validation errors can include
safe field paths and stable reasons.

Collection APIs use keyset cursors from ADR-0008 and bounded page sizes. No initial
endpoint exposes arbitrary MongoDB filters, projections, aggregation, regex, or deep
offset pagination.

### Minimal Web

Web is a thin client of `/api/v1`; it does not receive a private database or
application-service bypass. Its first required screens are:

```text
login/bootstrap
organization/project selector
project DSN/setup instructions
Issue list with status/search/time controls
Issue detail with statistics and activity
Event detail with raw and symbolicated stack/context
project retention/PII/key settings
system capability/degraded status
```

ADR-0041 now defines the accepted monochrome minimal visual system. The architectural
constraint remains that UI state derives from stable DTOs/capabilities and all
mutations use the same commands, permissions, audit, CSRF, and idempotency rules as
other clients.

### Future MCP

MCP remains unimplemented. Its later tools call application services and may reuse
native DTO concepts, but the API is not distorted around a hypothetical MCP schema.
No current endpoint or permission grants MCP a database-level shortcut.

## Consequences

- Official SDK Error Events have a precise compatibility promise without claiming
  unimplemented Sentry products.
- Exact SDK versions and fixture revisions make regressions reproducible.
- `/api/0` compatibility and `/api/v1` product evolution do not contaminate each
  other's DTOs.
- Minimal Web remains replaceable because it consumes the same stable API.
- Capabilities accurately expose incremental module availability.

## Deferred questions

- Enabling and adding conformance rows for each currently disabled Envelope category.
- Broader Sentry REST compatibility driven by real tooling rather than endpoint-count
  parity.
- Localization; the visual system and its accessibility gate are resolved by
  ADR-0041.
- MCP transport and tool schemas.
