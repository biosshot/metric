# Phase 36 Module Contract — Uptime Monitoring

## Scope

Phase 36 adds server-originated HTTP/HTTPS Uptime checks beside the Phase 35 Cron
monitor model. It does not add browser checks, response-body storage, private
network probing, screenshots, multi-step flows, or a second generic scheduler.

## Ownership

- `metric-domain::monitors` owns bounded Uptime configuration, sealed header
  values, stable identities and typed failure outcomes.
- `metric-mongo::monitors` owns tagged `monitors` (`k=0` Cron, `k=1` Uptime),
  durable due/lease state and compact TTL `monitor_runs`.
- `metric-application::uptime` owns polling, global/per-host concurrency,
  host-fair ordering and terminal run construction.
- `metric-server::uptime` owns DNS resolution/pinning, SSRF rejection, manual
  redirects, header stripping, response-byte and timeout bounds.
- Native HTTP owns plaintext admission and seals header values before domain
  configuration reaches MongoDB. Read APIs expose only header names and
  `has_value`.

## Safety invariants

Every hop accepts only HTTP/HTTPS without credentials or fragments. All DNS
answers are checked and the chosen address is pinned for that request. Loopback,
private, link-local, multicast, unspecified, CGNAT, benchmark, reserved and
metadata addresses are rejected for IPv4, IPv6 and IPv4-mapped IPv6.

Sensitive headers are sent only on the first hop. Non-sensitive headers survive
only same normalized-origin redirects. `Host`, framing, hop-by-hop, proxy and
forwarding headers are rejected. A check has at most 16 headers, 120 seconds,
three redirects and 64 KiB of discarded response bytes.

Uptime has a separate task and permits from ingest/read paths and is not part of
readiness. A terminal run advances the durable next due time and clears its
lease. The run ID derives from monitor ID and scheduled time, making lease
recovery idempotent after restart.

## Schema

Schema generation 17 is an intentional breaking empty-schema generation. The
existing collections are reused with tagged compact variants; no migration
framework is introduced.
