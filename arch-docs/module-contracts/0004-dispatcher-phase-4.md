# Phase 4 contract: bounded Dispatcher and durable-backlog refill

- Status: accepted for implementation
- Date: 2026-07-22
- Owners: `application::dispatcher` (queue, dedup and lifecycle), `mongo`
  (pending Event query and body decode), `server` (configuration/composition)

## Responsibilities and exclusions

Dispatcher owns the bounded in-process acceleration queue between durable Event
acceptance and a `WorkHandler`. It accepts fresh owned payloads without copying,
deduplicates queued/running Event keys, discovers due pending Events on startup,
idle and low watermark, and refills toward a configured target. MongoDB remains the
only durable backlog.

Dispatcher does not normalize, symbolize, group, finalize, mutate retry metadata,
claim distributed leases, enforce backlog admission, implement Scheduler, or add a
disk spool/broker. Phase 4 uses controlled fake WorkHandlers; Phase 5 and later
replace only that port.

## Ports and completion invariant

`EventBacklog` returns complete decoded `AcceptedEvent` values ordered by `q.n`,
`r`, `_id`, excluding a bounded caller-provided set of queued/running keys. It also
returns a bounded backlog observation containing pending count and oldest received
time. Backend queries and BSON remain inside the adapter.

`WorkHandler` receives one owned Event. Returning from `handle` means the handler
has durably changed that Event's eligibility: successful processing removed pending
state, temporary failure moved `q.n` into the future, or permanent failure made it
ineligible. Dispatcher never invents that state transition. Test fakes must enforce
the same invariant in their backing source.

The existing `AcceptedEventHandoff` is nonblocking. A fresh payload is moved into
the queue only when capacity and a unique key are available. Full/closed admission
returns ownership to MongoWriter, which may release it because the MongoDB document
is authoritative. A duplicate schedule is consumed and dropped without a second
payload allocation or WorkHandler call.

## Bounds and ordering

Queue capacity, worker concurrency, low watermark, refill target, per-query batch,
poll interval, metrics interval, source timeout and shutdown drain are finite. The
dedup set contains only queued/running keys and is bounded by queue capacity plus
running concurrency. `low_watermark < refill_target <= queue_capacity`.

MongoDB refill uses the ADR-0008 due order. The adapter scans a bounded multiple of
the requested batch while filtering pending-delete/purging/deleted projects. A very
large deletion-fenced prefix can delay eligible work until Phase 15 purges it; it
must never schedule fenced work or create an unbounded scan.

## Cancellation, restart and failure

Startup performs one bounded refill before readiness. Temporary source failures
after startup leave existing queued/running work intact and retry on the next poll.
A crash loses only RAM state; restart begins with an empty dedup set and reloads due
pending Events. WorkHandler completion before its durable eligibility update is a
handler contract violation, not compensated by local acknowledgement.

Shutdown fences fresh offers, closes refill, drains buffered/running work until the
configured deadline and then cancels remaining local tasks. Dropped local work stays
pending in MongoDB and is recoverable after restart.

## Operability and verification

Fixed-label metrics cover queue depth, queued/running dedup size, fresh/refill
admission, duplicate/full drops, refill duration/outcome/count, pending estimate,
oldest-pending age, handler completions and shutdown deadline exhaustion. Logs and
metrics never include project/Event IDs, payloads, database names or backend text.

Required verification is deterministic refill/dedup simulation, full queue with
continued durable acceptance, ordering/retry/project fence, shutdown and restart,
large-backlog bounded soak/fault tests, real MongoDB query/index integration,
recorded refill throughput above the later Processor target, and cumulative
SDK-to-MongoDB-to-Dispatcher E2E with a controlled WorkHandler.
