# ADR-0034: Module contracts, dependency direction, and quality gates

- Status: Accepted
- Date: 2026-07-21

## Context

The system will be implemented sequentially, module by module, beginning with HTTP
ingestion. Logical modules are already intentionally self-contained, but source-code
boundaries, allowed dependencies, and a repeatable completion gate must prevent the
single-process runtime from becoming a monolith.

Testing every module in exactly the same way would waste effort: a parser or compact
codec benefits from property and fuzz tests, while a MongoDB batch writer needs real
integration, failure, and load tests. Conversely, relying only on whole-system E2E
tests would localize deterministic data-corruption bugs poorly.

## Decision

### Contract before implementation

Before implementation begins, every application module records:

1. owned responsibility and explicit non-responsibilities;
2. typed inputs, outputs, and stable domain errors;
3. required ports and emitted commands/events;
4. idempotency and retry behavior;
5. MongoDB, BlobStore, network, and audit side effects;
6. memory, item-size, queue, concurrency, and time bounds;
7. cancellation and shutdown behavior;
8. health signals, metrics, and safe logging fields;
9. tests and performance acceptance criteria.

Contracts describe present invariants and extension points. They do not require a
distributed implementation for roles, queues, or leases that version one does not
run.

### Workspace boundaries

The repository is one Cargo workspace and one deployable application binary, with a
small number of crates enforcing dependency direction:

```text
domain          IDs, bounded values, canonical models, domain errors
ports           Storage/BlobStore/Symbolication/clock/randomness traits
sentry-protocol wire parsing, DSN/envelope compatibility, protocol DTOs
application     Ingest orchestration, writer, dispatcher, processor and services
mongo           MongoDB adapter, BSON codecs, indexes and query implementations
blob            local filesystem and later S3-compatible adapters
symbolication   external Symbolicator adapter
server          HTTP/Web adapters, configuration and composition root
testkit         fixtures, fakes and reusable conformance suites
```

Product modules such as Normalizer, Grouper, IssueService, Finalizer, Scheduler, and
upload services remain cohesive modules inside `application`; one crate per small
service is not required.

Allowed dependency direction is:

```text
domain <- ports
domain <- sentry-protocol
domain + ports <- application
domain + ports <- mongo/blob/symbolication
all selected crates <- server composition root
testkit -> public test contracts only
```

`domain` has no HTTP, async runtime, MongoDB, BSON, BlobStore, or Symbolicator types.
`application` cannot import concrete adapters. Adapters cannot call another adapter
directly. `server` wires implementations but contains no business decisions. Cyclic
crate dependencies are forbidden, and CI checks the declared graph.

Wire DTOs, BSON documents, and backend response types are translated at their owning
adapter boundary and never become the internal Event, Issue, or error model.

### Ports and dispatch

Hot replaceable boundaries use enum dispatch when the implementation set is known,
as accepted for Storage and Symbolication. Narrow cold-path test seams may use trait
objects when their measured cost is irrelevant. Domain services depend on traits or
generic ports, not global singletons.

Ports are capability-specific rather than one unrestricted database interface. For
example, Ingest receives a `ProjectResolver` and `EventSink`; it never receives a raw
MongoDB client. IssueService receives issue operations scoped to an authorized
project, not an arbitrary collection handle.

### Module quality gate

A module is complete only after this sequence succeeds:

```text
accepted contract and resource bounds
-> implementation
-> deterministic/property/contract tests where applicable
-> real-adapter integration tests where applicable
-> fault, load, and soak tests for hot or durable paths
-> addition to the cumulative E2E path
-> recorded benchmark and known-limit baseline
```

Tests are designed with the contract rather than added after the implementation as a
coverage exercise. The next dependent module does not begin while a correctness or
capacity gate for the current module is failing.

### Test selection by risk

There is no global line-coverage percentage. Required test styles are selected by
behavior:

```text
Envelope/HTTP protocol   golden fixtures, compatibility, fuzz, load
DSN/project resolution  security vectors, cache and integration tests
MongoWriter              real MongoDB, ambiguous failure, partial batch, load
Dispatcher               deterministic simulation, restart recovery, soak
Normalizer/PII           golden, unit, property and adversarial corpus
BSON/body codecs         round trip, malformed input, byte-size golden tests
Grouping/IDs/cursors     deterministic golden and property tests
Symbolication adapter    backend contract and failure classification
Issue/Finalizer          real MongoDB and crash-window integration tests
Web/API                  DTO contract, authorization and cumulative E2E
```

Pure routing glue does not need artificial unit tests when an adapter contract or E2E
test covers it. Deterministic transforms, parsers, security policy, state machines,
and stable encodings require focused tests even if E2E also covers common cases.

Fuzzing is bounded and reproducible in CI through retained regression inputs. Long
fuzz, soak, and maximum-load runs execute in scheduled CI or a dedicated benchmark
environment rather than making every local edit slow.

### Cumulative E2E development

The first functional module is Ingest, initially connected to fake ports. Its first
black-box path is:

```text
HTTP -> authentication/parser/admission -> fake durable EventSink -> response
```

As modules are completed, the same suite grows without discarding earlier isolation:

```text
HTTP -> Ingest -> MongoWriter -> MongoDB
HTTP -> ... -> Dispatcher -> Processor -> finalized Event
HTTP -> ... -> IssueService -> Web/API query
```

A full E2E test is required only once all dependencies for that slice exist. Before
then, module contract tests with fakes are the correct gate rather than a mock version
of the entire future system.

### Ingest-specific first gate

Before work moves from Ingest to its first durable adapter, Ingest must pass real
Sentry Envelope fixtures, authentication variants, compressed/decompressed limits,
mixed Item handling, malformed and fuzz corpora, slow/cancelled request cases,
bounded-memory assertions, and HTTP load against a controlled fake EventSink.

The fake reports the same durable outcomes as the real port but cannot expose an API
that production Storage lacks. Conformance tests are reused against the real writer
adapter later.

### Review rule

A change that crosses a module boundary must identify which contract changes. New
dependencies, unbounded collections, persistent fields, public errors, and background
work require an architecture update. Internal refactoring that preserves the
contract does not require another ADR.

## Consequences

- Architecture is completed at stable boundaries without pre-implementing a future
  cluster.
- Compile-time dependency direction prevents adapter and transport types from leaking
  into domain logic.
- Each module can be implemented and benchmarked with controlled ports.
- Unit tests are used where they find local deterministic bugs, not as a vanity
  percentage.
- Cumulative E2E confidence grows with the real implementation order.

ADR-0039 applies this gate to the accepted sequential module implementation order.

## Deferred questions

- Splitting an application module into another crate only after compilation time,
  ownership, or independent reuse justifies it.
- Distributed conformance suites when application roles become separate processes.
