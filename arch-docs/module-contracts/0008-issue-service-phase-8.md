# Phase 8 contract: IssueService and compact Issue persistence

- Status: accepted for implementation
- Date: 2026-07-22
- Owners: `application::issues`, `domain::issue`, `ports::IssueStore`, `mongo::issue`

## Responsibilities and exclusions

IssueService consumes one normalized Event and its already-computed GroupingResult. It
constructs a bounded stable title and optional culprit, then submits one project-scoped
Issue occurrence to IssueStore. The store atomically creates or updates the deterministic
Issue identity, complete GroupingKey, first/latest occurrence pairs, release pairs,
approximate count, and receipt-time regression state. IssueService also exposes bounded
resolve, ignore, reopen, and assignment commands through the same capability-specific port.

Phase 8 does not finalize Event documents, write hourly buckets, materialize Release or
Environment catalogs, emit notification intents, expose HTTP/API routes, add migrations,
or implement team assignment, merge/split, snooze, NATS, sharding, or disk spooling.

## Domain and port contract

Domain Issue values contain no BSON or MongoDB types. IssueId and the complete GroupingKey
must agree with the project-scoped ADR-0014 derivation before storage. Titles are non-empty
UTF-8 bounded to 512 bytes; culprits are optional and bounded to 256 bytes. ActorRef is a
17-byte canonical value: an append-only one-byte user/API/system kind followed by a
16-byte identifier. Teams are not representable.

`IssueStore::apply_occurrence` is the only hot Issue mutation. Its input includes the
Event's occurrence time, server receipt time, exact optional release, stable creation
summary, grouping explanation, and a positive batch increment. Retrying may positively
drift the approximate count, as accepted by ADR-0005, but cannot create a second Issue,
change its stable creation title, duplicate an activity, or increment regression count
more than once for the same resolved-to-open transition.

Workflow commands carry a caller-provided 16-byte idempotency key. Invalid transitions
are no-ops against the current source-of-truth Issue. Activity insertion follows the Issue
mutation, uses a deterministic ID, and is best effort; failure never rolls back current
Issue state. Title search is project-scoped, term/phrase based, and bounded to 100 compact
projections.

## Atomic persistence rules

Issue creation/upsert filters on `_id`, project, and the complete 34-byte GroupingKey. A
duplicate `_id` with a different complete key is reported as an identity collision. One
MongoDB update pipeline compares occurrence timestamp and Event ID together, updates the
first/latest Event and release pairs deterministically, increments `c`, and reopens only a
resolved Issue whose stored resolution time is older than the Event's server receipt time.
Ignored Issues never regress.

The compact release state follows amended ADR-0024: `fr` is the optional first release,
`lr` is an optional differing latest release, and `m: true` distinguishes a missing latest
release when `fr` exists. `lr` and `m` are mutually exclusive. Ordinary occurrences never
rewrite `t`, `q`, or grouping body `b`.

## Operability and verification

MongoDB owns strict validators and named indexes for Issue timeline, status timeline,
notification-ready scanning, bounded title text search, and Issue activity history. Safe
metrics contain only operation and outcome; titles, grouping material, releases, actors,
and identifiers are forbidden labels.

The exit gate requires compact codec round trips, malformed/collision/property and exact
byte-size fixtures; real MongoDB concurrent create/update, lifecycle, ignored and
receipt-time regression tests; retry/crash-window count-drift tests; same-Issue and
distributed-Issue contention; query explain evidence for initial timeline and title
search; full formatting/lint/test gates; and one retained RPS baseline. Every performance
run must terminate its test process and be followed by an explicit lingering-process check.
