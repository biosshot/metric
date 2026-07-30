# Capacity and profiles

Metric has four starting profiles. They change memory limits, MongoDB cache,
request concurrency, queue sizes, file limits and retention together.

## Recommended starting point

| Profile | vCPU | RAM | SSD/NVMe | Symbolicator | BlobStore capacity |
| --- | ---: | ---: | ---: | --- | ---: |
| **Min** | 1 | 1 GiB | 15 GiB | No | 5 GiB |
| **Low** | 2 | 2 GiB | 30 GiB | No | 10 GiB |
| **Medium** | 4 | 8 GiB | 100 GiB | Yes | 33 GiB |
| **High** | 8 | 16 GiB | 250 GiB | Yes | 83 GiB |

Medium is the default installer profile. Min is deliberately designed for a
small VPS but can still batch a continuous stream of compact logs. Its practical
long-term limit is usually disk space and retention rather than Rust request
handling. Give a 1 GiB Min server 1–2 GiB of swap as an emergency buffer.

The profile names describe resource limits, not guaranteed traffic. Event size,
MongoDB indexes, attachments, Replay, source maps and traffic bursts can change
capacity substantially.

## Foreground ingest

The supplied profiles use aggressive but bounded admission. A larger admission
window lets the Log and Span writers fill MongoDB batches instead of writing many
small batches.

| Profile | Active ingest requests | Parsing tasks | Storage queue | Documents per batch |
| --- | ---: | ---: | ---: | ---: |
| **Min** | 64 | 2 | 128 | 128 |
| **Low** | 256 | 4 | 512 | 250 |
| **Medium** | 1024 | 8 | 2048 | 500 |
| **High** | 4096 | 16 | 8192 | 500 |

Queue limits are ceilings, not preallocated memory. Small SDK envelopes use only
a small part of them. Large attachments and Replay segments remain controlled by
separate byte limits.

## Feature choices

| Feature | Min | Low | Medium | High |
| --- | --- | --- | --- | --- |
| Errors, issues and web interface | Yes | Yes | Yes | Yes |
| Logs, transactions, spans and metrics | Yes | Yes | Yes | Yes |
| Attachments and feedback screenshots | No | Yes | Yes | Yes |
| Symbolicator and source-map processing | No | No | Yes | Yes |
| Session Replay | Keep disabled | Keep disabled | Optional per project | Optional per project |
| Minidumps | Disabled | Disabled | Disabled | Disabled |
| Cold archive | Disabled | Disabled | Disabled | Disabled |

Minidumps remain disabled because they may contain process memory. Cold archive
remains disabled because archived data cannot yet be searched or restored through
Metric. These choices should not change merely because a server is larger.

Min and Low still store raw JavaScript and native frames. They only skip the
separate Symbolicator processing step.

## Retention

High-volume raw data is kept for less time than compact hourly statistics.
This preserves useful trends without filling a small disk with individual rows.
Cold archive is disabled in the supplied profiles, so every period below is for
searchable hot data.

| Data | Min | Low | Medium | High |
| --- | ---: | ---: | ---: | ---: |
| Error events | 30 days | 60 days | 90 days | 180 days |
| Logs | 7 days | 14 days | 30 days | 90 days |
| Spans | 7 days | 14 days | 30 days | 90 days |
| Hourly span statistics | 180 days | 1 year | 2 years | 3 years |
| Feedback | 90 days | 180 days | 1 year | 2 years |
| Hourly issue statistics | 1 year | 2 years | 3 years | 5 years |
| Individual release sessions | 14 days | 30 days | 60 days | 90 days |
| Hourly release statistics | 1 year | 2 years | 3 years | 5 years |
| Monitor runs | 90 days | 180 days | 1 year | 2 years |
| Application metrics | 60 days | 120 days | 180 days | 1 year |
| Session Replay, when enabled | 7 days | 14 days | 30 days | 90 days |
| Delivered notification history | 30 days | 60 days | 90 days | 180 days |
| Failed notification history | 90 days | 180 days | 1 year | 2 years |

MongoDB TTL cleanup is asynchronous. Data can remain for a short time after its
retention period, so never size a disk with zero free space.

## Why Min can fit in 1 GiB

The Min Compose profile:

- does not start Symbolicator or its cleanup process;
- gives MongoDB a 256 MiB WiredTiger cache and a 512 MiB container limit;
- limits Metric to 320 MiB;
- batches bursts while keeping active requests and background workers bounded;
- disables attachments and limits Replay buffering to 4 MiB;
- rotates container logs after two 5 MiB files per service.

The limits leave part of the recommended RAM to Linux, Docker and short memory
spikes:

| Profile | MongoDB ceiling | Metric ceiling | Symbolicator ceiling | Left for host and spikes |
| --- | ---: | ---: | ---: | ---: |
| Min | 512 MiB | 320 MiB | — | 192 MiB |
| Low | 768 MiB | 768 MiB | — | 512 MiB |
| Medium | 3 GiB | 1.5 GiB | 2 GiB + 128 MiB cleanup | 1.375 GiB |
| High | 6 GiB | 3 GiB | 4 GiB + 256 MiB cleanup | 2.75 GiB |

These are container ceilings, not expected idle use. Docker may use less.

[MongoDB documents](https://www.mongodb.com/docs/manual/core/wiredtiger/#memory-use)
a minimum WiredTiger cache of 256 MiB. Metric uses that lower bound explicitly
rather than letting MongoDB compete for the entire host.

One GiB is still a tight machine. Use a minimal 64-bit Linux installation, keep
swap available, avoid unrelated services and watch for container restarts. The
profile already uses an aggressive ingest window. If Min frequently returns
`429`/`503` or uses swap continuously, move to Low instead of removing its final
safety bounds.

## Plan disk space

Metric stores data in:

- `mongo-data`: events, issues, users, indexes and settings;
- `blob-data`: attachments, replays, debug files and exports;
- `symbolicator-cache`: rebuildable data used only by Medium and High.

The SSD recommendation includes the operating system, container images, swap and
working space. Backups must be stored on another disk or machine and are not
included in the table.

Each supplied profile gives BlobStore about one third of the recommended disk.
Its internal reserve keeps 256 MiB, 512 MiB, 2 GiB or 4 GiB of that allocation
unwritable so one large upload cannot consume the last available bytes. MongoDB,
the operating system, images and swap share the rest of the disk. This is a
ceiling, not preallocated space.

One event per second is 2,592,000 events over 30 days. Even a 10 KiB stored event
would be about 25 GiB before indexes and other collections. Sampling SDK traces
and logs matters more than choosing a larger queue.

## Reference result

A committed short reference test reached 4,983 durable error events per second
with no acknowledged loss on:

- AMD Ryzen 5 5600H, 6 cores and 12 threads;
- 16 GiB RAM;
- MongoDB 8 on the same machine;
- a 5,000 events-per-second target for 15 seconds.

This is a regression reference, not a long-running capacity claim and not
evidence for Min or Low. Raw results are in
[`performance/baselines`](https://github.com/biosshot/metric/tree/main/performance/baselines).

## Check your installation

Watch:

- `docker stats`;
- HTTP `429` and `503` responses;
- `/ready` failures and container restarts;
- event processing delay;
- MongoDB disk use;
- free space in all active volumes.

Start with the smallest profile that contains the features you need. Move up when
normal traffic, not a one-time spike, repeatedly reaches its limits.

The DSN load script prints `200`, `429`, `503`, other HTTP statuses and TCP errors
separately:

```bash
k6 run -e FAULTKEEP_DSN=http://KEY@SERVER:4001/PROJECT \
  -e FAULTKEEP_LOG_RPS=500 -e FAULTKEEP_DURATION=30s \
  performance/k6/structured-logs-dsn.js
```
