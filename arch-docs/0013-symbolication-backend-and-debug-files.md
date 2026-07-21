# ADR-0013: Replaceable symbolication backend and debug-file storage

- Status: Accepted
- Date: 2026-07-21

## Context

Native events need function names, files, line numbers, demangling, and eventually
minidump stackwalking. Sentry Symbolicator already implements these features and is
operationally simpler than recreating the complete service immediately, but current
versions use FSL-1.1-MIT and prohibit competing use until the version-specific future
license becomes effective.

Replacing software after a license complaint would not erase earlier unauthorized
use. The integration must therefore be technically replaceable and gated on using a
legally permitted version or obtaining permission before distribution or service use.

## Decision

### Backend boundary

Processor calls an application-owned `SymbolicationService` with domain request and
response types. Sentry-specific HTTP schemas, status values, source descriptors, and
cache details remain inside one adapter.

Symbolication uses enum dispatch:

```rust
pub enum SymbolicationEngine {
    SentryService(SentrySymbolicatorClient),
}
```

The first implemented backend is an external Sentry Symbolicator service. A future
native implementation built on MIT `symbolic` crates can be added as another enum
variant without changing Processor, event schema, grouping input, Web, or MCP.

```toml
[symbolication]
backend = "sentry_symbolicator"
endpoint = "http://symbolicator:3021"
```

The application does not fork Symbolicator code, expose its API as the internal
domain model, or let ordinary modules call it directly.

### Development and distribution policy

The adapter can connect to any protocol-compatible Symbolicator endpoint selected by
the operator. Local development and conformance CI use one exact pinned image/build
for reproducibility, but the application has no runtime version gate and the
architecture is not blocked on choosing a permanent Symbolicator release.

The application initially does not vendor, fork, compile into its binary, or
redistribute a Symbolicator image. Deployment configuration supplies the endpoint;
the tested external version, source and checksum are recorded for diagnostics.

The current upstream repository publishes Symbolicator under FSL-1.1-MIT, whose
competing-use restriction applies until the future MIT date for each version. Making
this application open source does not itself change a third party's license. Public
release packaging therefore records the exact external component/license and does
not claim that an unreviewed bundled image is covered by this project's license.

This is a distribution/release check rather than an implementation blocker. If the
optional integration becomes unavailable, incompatible, or disputed, the adapter can
be removed and replaced with a backend built on suitably licensed `symbolic`-family
libraries without changing Processor or persisted domain contracts. This ADR records
technical boundaries and is not legal advice.

### Runtime topology

The application still exposes only `--role=all` in the first version. Symbolicator is
an external dependency, like MongoDB, and runs as a separate process or container.
The in-process `SymbolicationService` adapter has bounded request concurrency,
timeouts, cancellation, health reporting, and circuit-breaking behavior.

Processor sends only project scope, platform, modules, raw stack traces, and approved
source configuration. It does not send the complete event payload.

### Supported initial formats

The first symbolication scope is native:

- ELF with DWARF;
- Mach-O with dSYM/DWARF;
- PE with PDB;
- Portable PDB;
- Breakpad symbols;
- Rust, C++, Objective-C, and Swift demangling where supported.

ADR-0028 adds JavaScript and Node.js source maps through whole artifact bundles and
Symbolicator's `/symbolicate-js` endpoint. ProGuard/R8, full minidump stackwalking,
native source bundles, BCSymbolMaps, and IL2CPP remain later format-specific stages.

### Persistent debug-file storage

Customer-uploaded original debug files are persistent BlobStore objects. MongoDB
stores lookup and authorization metadata in `debug_files`:

```javascript
{
  _id,
  project_id,
  debug_id,
  code_id,
  object_format,
  file_kind,
  architecture,
  blob_key,
  filename,
  size,
  checksum,
  status,
  uploaded_at,
  last_used_at
}
```

This is the logical domain projection only. ADR-0027 replaces it physically with a
compact ready-only BSON document, reconstructs the BlobStore key, and omits status
and exact use tracking. Callers continue to use the descriptive domain fields.

One debug ID may have multiple candidate files. Private files are scoped by project;
only explicitly configured public sources use a global scope. Source URLs and request
headers are administrator configuration and can never be supplied by an event.

The adapter exposes approved BlobStore objects or symbol sources to the backend. The
backend's local raw, symcache, CFI, source-map, and negative caches are rebuildable and
are not the persistent source of truth.

ADR-0026 defines the first adapter path: each request supplies Symbolicator's native
`sentry` source type, backed by a project-authenticated internal index/download
endpoint. Symbolicator receives neither a user token nor direct BlobStore credentials.

### Security and resource control

External downloads block reserved/private network destinations by default, validate
redirect targets, limit response sizes and timeouts, and keep credentials in the
deployment secret mechanism. Debug-file upload and parsing use bounded sizes,
decompression limits, and concurrency.

Concurrent requests for the same missing or uncached object are coalesced by the
backend or adapter so a traffic burst cannot trigger identical downloads and cache
builds without bound.

### Event result

Raw frames and addresses are always retained. Symbolicated frames are stored as a
separate derived representation:

```javascript
symbolication: {
  status,
  completed_at,
  missing_debug_ids,
  errors,
  frames
}
```

Statuses distinguish complete, partial, missing, malformed, timeout, and not-required
results. Missing or malformed debug information does not keep an event pending
forever. After bounded retry classification, Processor can finalize the event using
raw module-relative addresses and expose the symbolication diagnostics.

### Reprocessing and retention

The first version does not automatically enqueue every historical event after a debug
file upload. A user may request bounded reprocessing for a project and time range.
Automatic missing-debug-ID indexing and reprocessing require later workload evidence.

ADR-0032 keeps original customer debug files without automatic age expiration by
default, adds optional explicit age and project quota policies, and rejects new
physical storage instead of silently evicting historical context. Files are also
removed explicitly or with the project. Backend-derived caches remain evictable and
rebuildable according to backend cache policy.

ADR-0025 defines the Sentry CLI-compatible chunk upload, one-job-per-file assembly,
temporary-chunk cleanup, authorization, and final BlobStore commit path. Temporary
chunks have their own bounded lifetime and are not customer debug-file records.

ADR-0033 defines exact-ID explicit deletion for the initial single-process runtime,
keeps ready metadata free of deletion state, and uses keyed in-process exclusion plus
orphan reconciliation instead of a premature distributed fencing protocol.

## Consequences

- Initial native symbolication can rely on a mature service without coupling the
  application domain to its protocol.
- Symbolicator adds another external runtime process even though application roles
  remain `--role=all` only.
- A compatible backend can replace it without rewriting Processor or persisted event
  semantics.
- Development can use a pinned compatible external endpoint without coupling the
  application binary or domain to one Symbolicator release.
- Public packaging does not silently redistribute the external service under this
  project's license, and the adapter remains replaceable if use becomes disputed.
- Events remain usable and groupable when symbols are absent.
- Original debug files survive cache cleanup and application restarts.

## Deferred questions

- Exact optional Symbolicator packaging choice for a public release, if it is ever
  bundled rather than operator-supplied.
- ProGuard/R8, minidump, source-bundle, and IL2CPP stages.
- Automatic affected-event discovery after symbol upload.
