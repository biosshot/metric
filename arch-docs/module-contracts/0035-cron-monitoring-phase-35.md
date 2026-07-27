# Phase 35 module contract: Cron Monitoring

## Ownership

- `metric-domain::monitors` owns bounded monitor identity, Cron schedule, check-in
  lifecycle and deterministic SDK/synthetic run identities.
- `metric-sentry-protocol` recognizes `check_in` as a dedicated envelope item. It
  does not classify Cron as Error, Log, Transaction or Span.
- `metric-application::monitor_writer` owns the independent bounded admission queue,
  micro-batching, shutdown drain and durable acknowledgements.
- Scheduler owns bounded timeout and missed detection through typed monitor
  maintenance ports. Scheduler lag is observable but never participates in ingest
  readiness.
- `metric-mongo::monitors` owns compact BSON codecs, compare-and-set lifecycle
  writes, repairable monitor projections, retention and query indexes.
- Native API and Vue own ProjectRead history and ProjectAdmin definition/policy
  changes. Phase 34 owns Telegram and SMTP delivery after a monitor alert has been
  durably expanded into the existing notification outbox.

## Pipeline and isolation

```text
Sentry check_in envelope item
-> shared DSN/project resolution and scrub boundary
-> bounded CheckIn normalization
-> independent MonitorWriter queue
-> monitors + monitor_runs
-> Scheduler timeout/missed materialization
-> monitor alert evaluation
-> existing notification outbox
```

Cron has its own queue, batch, timeout and project admission budget. It cannot enter
or consume permits from Error, Log or Span writers. No provider request runs in
ingest, MonitorWriter or Scheduler.

## Storage and volume boundary

`monitors` contains one small mutable definition/current-state projection per
`(project, cron slug, environment)`. `monitor_runs` is the compact TTL history and
source of truth. A run stores only binary identifiers, tagged status/source,
timestamps, duration, optional Release identity and small processing markers.

Raw envelope JSON, stack data, arbitrary SDK attributes, attachments and BlobStore
objects are never stored for Cron. Mongo field names are compact and status/source
values are integer tags. Terminal run outcome fields are immutable; technical
projection/alert markers may advance idempotently.

SDK run identity is derived from `(monitor_id, check_in_id)`. A missed identity is
derived from `(monitor_id, scheduled_for)`. Timeout terminalizes an existing
in-progress run with compare-and-set. One overdue observation produces one missed
run and advances directly to the first future schedule, avoiding unbounded downtime
backfill.

## Time and ordering semantics

- incoming `ok` maps to domain `success`;
- server receipt time owns deadlines and ordering;
- client wall-clock timestamps are ignored; server receipt time prevents client
  clock skew from moving deadlines backward or forward;
- duplicate input is a no-op;
- terminal-before-in-progress creates the terminal run;
- in-progress after terminal is a no-op;
- the first terminal outcome wins, including a Scheduler timeout before a late SDK
  completion;
- missed is detected at `scheduled_for + checkin_margin`;
- timeout is detected at `in_progress_received_at + max_runtime`.

The configured `checkin_margin` is the only missed-run grace window. Once Scheduler
has observed a due definition, its deterministic `missed` run remains in history;
a later heartbeat is a distinct SDK run and advances the current projection without
rewriting that missed outcome. A Scheduler `timeout` and late completion share the
same SDK run identity, so the timeout wins and the completion is a duplicate.

Schedules support bounded interval values and an explicitly validated five-field
numeric UTC crontab subset with a one-minute minimum frequency. Unsupported syntax
or timezone fails explicitly.

## Storage lifecycle

Phase 35 creates `monitors` and `monitor_runs`, allocates stable project-deletion
dataset codes and advances the intentional empty-schema generation to 16. There is
no migration framework.

Monitor definitions have no TTL. Terminal run history has configurable TTL.
In-progress runs are not deleted before Scheduler terminalizes them. Project
deletion purges both collections in bounded batches.

## Explicit exclusions

Uptime HTTP execution, non-UTC timezone databases, legacy Cron REST endpoints,
archive-to-Blob, Metrics, Profiling, Replay, MCP, NATS, migrations, sharding and disk
spool remain deferred. Phase 36 is not implemented by this phase.
