# Capacity and profiles

Metric has four starting profiles. They change memory limits, MongoDB cache,
request concurrency, queue sizes, file limits and retention together.

## Recommended starting point

| Profile | vCPU | RAM | SSD/NVMe | Symbolicator | Local file limit |
| --- | ---: | ---: | ---: | --- | ---: |
| **Min** | 1 | 1 GiB | 15 GiB | No | 256 MiB |
| **Low** | 2 | 2 GiB | 30 GiB | No | 2 GiB |
| **Medium** | 4 | 8 GiB | 100 GiB | Yes | 10 GiB |
| **High** | 8 | 16 GiB | 250 GiB | Yes | 50 GiB |

Medium is the default installer profile. Min is deliberately designed for a
small VPS: one or a few projects, a low average error rate and no continuous log
or trace stream. Give a 1 GiB Min server 1–2 GiB of swap as an emergency buffer.

The profile names describe resource limits, not guaranteed traffic. Event size,
MongoDB indexes, attachments, Replay, source maps and traffic bursts can change
capacity substantially.

## Feature choices

| Feature | Min | Low | Medium | High |
| --- | --- | --- | --- | --- |
| Errors, issues and web interface | Yes | Yes | Yes | Yes |
| Logs, transactions, spans and metrics | Yes, use sparingly | Yes | Yes | Yes |
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

| Data | Min | Low | Medium | High |
| --- | ---: | ---: | ---: | ---: |
| Error events | 7 days | 14 days | 30 days | 90 days |
| Logs | 3 days | 7 days | 14 days | 30 days |
| Spans | 3 days | 7 days | 14 days | 30 days |
| Hourly span statistics | 30 days | 60 days | 90 days | 180 days |
| Feedback | 30 days | 60 days | 90 days | 180 days |
| Hourly issue statistics | 90 days | 180 days | 400 days | 730 days |
| Individual release sessions | 3 days | 7 days | 7 days | 14 days |
| Hourly release statistics | 90 days | 180 days | 400 days | 730 days |
| Monitor runs | 30 days | 60 days | 90 days | 180 days |
| Application metrics | 30 days | 60 days | 90 days | 180 days |
| Session Replay, when enabled | 1 day | 3 days | 7 days | 30 days |
| Delivered notification history | 7 days | 14 days | 30 days | 90 days |
| Failed notification history | 30 days | 60 days | 90 days | 180 days |

MongoDB TTL cleanup is asynchronous. Data can remain for a short time after its
retention period, so never size a disk with zero free space.

## Why Min can fit in 1 GiB

The Min Compose profile:

- does not start Symbolicator or its cleanup process;
- gives MongoDB a 256 MiB WiredTiger cache and a 512 MiB container limit;
- limits Metric to 320 MiB;
- reduces active requests, background workers and queues;
- disables attachments and limits Replay buffering to 1 MiB;
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
swap available, avoid unrelated services and watch for container restarts. If
Min frequently returns `429`/`503` or uses swap continuously, move to Low rather
than raising one limit in isolation.

## Plan disk space

Metric stores data in:

- `mongo-data`: events, issues, users, indexes and settings;
- `blob-data`: attachments, replays, debug files and exports;
- `symbolicator-cache`: rebuildable data used only by Medium and High.

The SSD recommendation includes the operating system, container images, swap and
working space. Backups must be stored on another disk or machine and are not
included in the table.

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
