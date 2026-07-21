# ADR-0037: Capacity and verification model for 100 million Error Events per day

- Status: Accepted
- Date: 2026-07-21

## Context

The target of up to 100 million Events per day is plausible for a Rust ingest process,
but it is not a useful guarantee without Event sizes, burst shape, retention,
MongoDB hardware, indexes, replication, and Processor work. At that volume storage
bytes and MongoDB write/retention behavior can dominate HTTP parsing.

The architecture needs a reproducible capacity envelope and module/E2E gates rather
than an unqualified events-per-day statement.

## Decision

### Rate translation and design envelope

The target translates to:

```text
100,000,000 / 86,400 = approximately 1,158 accepted Events/second average
```

The initial design and benchmark envelope is:

```text
daily average target       1,158 accepted Events/s
steady headroom test       5,000 accepted Events/s for 60 minutes
burst test                20,000 accepted Events/s for 5 minutes
recovery processing rate  at least 1.5x the measured accepted steady rate
```

These are workload targets, not a promise for unspecified hardware. Every published
result records application commit, Rust/toolchain, MongoDB version/topology, storage
backend, CPU, RAM, disks, network, configuration, dataset and generator capacity.

### Standard Event corpus

The base throughput corpus contains scrubbed Error Events without attachments or
transactions and targets encoded MongoDB document sizes:

```text
60%   approximately   1 KiB
30%   approximately   4 KiB
 9%   approximately  16 KiB
 1%   approximately 128 KiB
```

The weighted body/document mix is intentionally much larger than a tiny synthetic
message-only Event. Fixtures cover multiple SDKs, exception chains, stack depths,
breadcrumbs, tags, request/user/context, releases, fingerprints, native module data,
and unknown compatible fields.

Traffic contains deterministic duplicate retries and bounded malformed/disabled Item
requests in separate reported categories. New-Event throughput is never inflated by
counting cheap duplicates as full durable inserts.

Attachments, standalone minidumps, debug uploads, Artifact Bundles, and notification
delivery have separate byte/concurrency workloads. Symbolication benchmarks report a
base no-symbolication path and explicit cache-hit/cache-miss mixtures rather than
hiding external service cost inside the generic Event number.

### Storage capacity is explicit

Operators calculate primary logical hot storage as:

```text
events_per_day *
  (average encoded Event bytes + average maintained index bytes + engine overhead) *
  retention_days
```

Replication, journal, oplog, temporary index builds, free-space reserve, and
filesystem/object-store copies are added separately.

For scale intuition only, 100 million Events at an average 2 KiB document plus 0.5
KiB indexes is roughly 250 GB/day before replication. At 5 KiB plus 1 KiB indexes it
is roughly 600 GB/day. Thirty hot days are roughly 7.5--18 TB on the primary and
22.5--54 TB across three full replicas, before operational headroom. Benchmarks must
measure actual BSON and index sizes from the accepted corpus; these examples are not
sizing guarantees.

The server exposes a capacity report using observed average encoded bytes, accepted
rate, retention, current indexes, compression and replication inputs so operators can
see projected daily and retained storage.

### Hot-path batch assumptions

The accepted MongoWriter defaults remain:

```toml
max_wait_ms = 20
max_documents = 250
max_bytes = "8MiB"
```

All remain configurable within ADR-0002's bounds. At the daily average, full 250-item
batches would require fewer than five insert commands per second; at 20,000 Events/s,
approximately 80 full batches/s. Real occupancy, partial batches, BSON bytes and
write latency are measured rather than inferred from this arithmetic.

Unordered batch inserts, deterministic IDs, bounded request permits, durable backlog,
and the RAM queue fallback are required in every load profile. The generator uses
enough independent connections and hosts/processes that its CPU or network is not the
reported server limit.

### Acceptance correctness and latency SLOs

For a passing steady test on declared hardware:

```text
acknowledged Event loss                 0
duplicate durable Event creation        0
unexpected 5xx under target             < 0.1%
Ingest durable-ack latency p95           < 100 ms
Ingest durable-ack latency p99           < 250 ms
steady oldest-pending age                < 60 s
bounded memory/queue growth              required
```

Planned overload may return the accepted `429`/`503` responses and is reported
separately from unexpected failure. An ambiguous client timeout can produce a retry,
but deterministic identity must still yield one Event.

Latency SLOs exclude deliberately configured remote Symbolicator work because Error
acceptance precedes Processor completion. Processor completion latency, Issue
availability, and symbolication are reported as separate distributions.

### Required performance suites

Module gates include:

```text
Ingest parser        requests/s, bytes/s, allocations, malformed/fuzz corpus
MongoWriter          documents/s, batch occupancy, bytes, partial/ambiguous failure
Dispatcher           refill rate, dedup set, restart with large pending backlog
Processor stages     CPU/event and allocations by Event fixture class
Finalizer            issue/bucket update throughput and contention
Search/Web queries   p95/p99 plus keys/docs examined on production-shaped data
Retention/TTL        delete/archive lag and impact on foreground writes
Blob paths           streamed bytes/s, concurrency, failure and disk reserve
```

The cumulative E2E suites are:

1. average 24-hour-equivalent rate extrapolation after a long steady run;
2. 5,000/s steady headroom;
3. 20,000/s bounded burst and recovery;
4. MongoDB latency injection and temporary outage;
5. Processor slower than Ingest until backlog guard, then recovery;
6. process restart with a populated durable pending queue;
7. duplicate storm and one-project noisy-neighbor rate limit;
8. retention/GC work concurrent with foreground ingestion;
9. mixed small/large Event corpus and decompression limits;
10. graceful shutdown during active batches.

Each run verifies accepted responses against durable Event IDs and terminal counts;
throughput alone cannot pass a test that lost or duplicated data.

### Profiling and regression policy

Benchmark artifacts retain latency histograms, throughput, encoded bytes, batch
occupancy, CPU profiles, allocation profiles, MongoDB server metrics, index sizes,
query explains, queue depth, oldest pending age, disk/IOPS and network use.

CI compares short deterministic microbenchmarks and module integration baselines.
Long load/soak tests run on controlled hardware. A statistically significant
regression beyond an agreed module budget blocks merging or requires an explicit
baseline update with explanation.

### Scaling decision thresholds

Version one does not enable application role splitting or MongoDB sharding merely to
claim scale. Those decisions are revisited when production-shaped tests show a
sustained bottleneck such as:

- application CPU or memory saturation after profiling and bounded optimization;
- MongoDB primary CPU, disk latency, IOPS, or replication lag near safe capacity;
- write p99 or TTL/retention lag violating the accepted SLO;
- working-set/index size no longer fitting the chosen topology;
- Processor recovery rate unable to exceed arrival rate;
- one process unable to isolate HTTP, processing, and maintenance concurrency.

The recorded bottleneck determines whether the next step is vertical hardware,
index/schema reduction, retention change, partitioning/sharding, or split roles. NATS
or another broker is not selected before that evidence exists.

## Consequences

- "100 million/day" becomes a measurable average plus burst, size and correctness
  contract.
- Storage requirements are visible and may dominate application throughput.
- A Rust-only microbenchmark cannot be presented as an end-to-end capacity result.
- Module performance baselines align with the sequential implementation strategy.
- Future scaling work responds to an observed bottleneck rather than speculation.

## Deferred questions

- Published reference-hardware profiles after the implementation exists.
- Native shard key/partition plan after production-shaped MongoDB results.
- Separate capacity models for enabled transactions, replays, profiles, and metrics.
