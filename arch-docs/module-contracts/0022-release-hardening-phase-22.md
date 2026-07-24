# Phase 22 module contract: full-system verification and packaging

Status: accepted implementation contract
Owner: release verification and deployment boundary
Owning decisions: ADR-0034, ADR-0035, ADR-0036, ADR-0037, ADR-0039
Sequential gate: ADR-0039 Phase 22

## Boundary

Phase 22 does not add product behavior or relax any Phase 1–21 contract. It owns
reproducible evidence, packaging, deployment defaults and published capability
claims. Runtime crates remain responsible for their existing limits, recovery,
health, metrics and safe errors.

The release boundary may invoke public binaries and conformance tests, inspect
aggregate MongoDB/storage measurements, and assemble immutable artifacts. It may not
reach into MongoDB to repair product state, invent test-only success paths, enable an
untested SDK family, or introduce MCP, NATS, sharding, split roles, disk spool or
online migrations.

## Inputs and outputs

Inputs are a clean source revision, pinned Rust/Node/SDK/CLI toolchains, explicit
MongoDB and BlobStore configuration, controlled hardware identity and isolated test
databases. Secret values are supplied through the accepted secret-reference/env-file
boundary and are never copied into artifacts.

Outputs are:

- a machine-readable SDK/CLI compatibility matrix with `pass`, `untested`,
  `disabled`, and `failed` kept distinct;
- contract, security, compatibility, load, recovery, soak and shutdown evidence;
- capacity artifacts containing aggregate BSON/index/storage measurements;
- a non-root container image and all-in-one MongoDB/local-BlobStore deployment;
- operations, compatibility, capacity, capability and known-limit documentation;
- a committed release report that marks every ADR-0039 release-gate row honestly.

Phase 22 defines no new public application error code.

## Resource and process contract

- Every test database and object prefix is unique to a run.
- Every runner has a finite timeout and at most the explicitly configured
  concurrency, duration and RPS.
- Local performance work executes no more than two profiles per implementation
  pass. Long steady/soak evidence remains a separately invoked controlled-hardware
  gate.
- A runner records TCP failures and HTTP `200`, `429`, `503`, and other responses;
  overload is never hidden in a single success rate.
- Processes started by a runner are tracked by PID and stopped in `finally`, including
  failure and threshold-exit paths. User-owned MongoDB is never stopped.
- Generated databases are removed only when their validated names use the dedicated
  `faultkeep_phase22_` prefix.

## Compatibility claim

Only a row with exact version/runtime, immutable fixture revision, executable
evidence and last passing server build may use `status = "pass"`. Similar wire JSON
is not evidence for another SDK family. The release gate fails while any family in
the compatibility manifest's versioned `release_required_families` set is not
passing. Non-required inventory rows remain honest and visible without blocking the
selected version-one scope.

Disabled transactions, spans, profiles, sessions, replays, check-ins, metrics/logs
and feedback remain advertised as disabled.

## Packaging contract

The image contains one `--role all` Faultkeep binary and built Vue assets, runs as an
unprivileged user, uses an explicit local BlobStore volume, exposes only the
application listener, and receives MongoDB/HMAC secrets at runtime. MongoDB remains
a separate service. External Symbolicator is optional configuration and no
Symbolicator image is bundled.

The compose example is a simple initial deployment, not a high-availability,
sharded, migration-capable or guaranteed-backup design.

## Release gate

Phase 22 is complete only when all ADR-0039 release rows pass on recorded evidence:
zero acknowledged loss/duplicate identity, bounded soak, security/tenant isolation,
complete claimed compatibility, visible overload metrics, complete
deletion/retention registration and no unresolved critical/high enabled defect.

The ADR-0039 closure amendment accepts the retained bounded-resource/restart corpus
and short Windows RPS artifacts for the selected development release. It deliberately
does not turn them into a controlled-hardware production-capacity or long-soak claim.

Until then, commits may advance the release harness but the Phase report must remain
`in progress` and must name the blocking rows.
