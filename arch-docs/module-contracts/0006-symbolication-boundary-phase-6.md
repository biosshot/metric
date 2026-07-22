# Phase 6 contract: Symbolication application boundary and baseline behavior

- Status: accepted for implementation
- Date: 2026-07-22
- Owners: `application::symbolication` (classification, limits and policy hooks),
  `domain::symbolication` (backend-independent request/result), `ports`
  (`SymbolicationBackend` capability)

## Responsibilities and exclusions

The application stage determines whether a normalized Error Event requires no
symbolication, native symbolication, or JavaScript/Node source mapping. It sends only
project scope, bounded raw traces, bounded module metadata, and exact release/dist to
a replaceable backend port. Raw frames remain in every stage result and derived
frames are stored separately.

Phase 6 does not implement the external Symbolicator HTTP adapter, private callbacks,
debug-file or artifact upload, source-map parsing, caches, BlobStore, MongoDB,
grouping, finalization, or Processor retry persistence. Backend wire types and status
codes cannot enter domain/application crates.

## Classification and results

An Event is `not_required` when it has no native address work and is not a
JavaScript/Node Event with stack frames. Address-bearing traces are native work;
JavaScript/Node traces are JavaScript work. Classification is deterministic and
depends only on the normalized Event.

`SymbolicationResult` contains the classification, stable status, policy disposition,
all raw traces, optional derived traces, bounded missing debug identifiers, and
bounded machine diagnostics. Statuses cover not-required, complete, partial,
missing, malformed, timeout, unavailable, and cancelled. Dispositions are continue,
retryable, or finalize-raw; Processor later owns attempt/backoff persistence.

Complete and partial backend results continue. Missing or malformed results finalize
with raw frames. Runtime timeout/unavailable/cancellation is retryable. The disabled
production baseline is deliberately different from a temporary backend outage: it
returns unavailable plus `finalize_raw` immediately for required work, so ordinary
installations cannot leave Events pending forever.

## Bounds, cancellation, and operability

Configuration bounds backend concurrency, total timeout, request traces/frames,
modules, missing identifiers, diagnostics, and derived frames. A request above a
backend bound is not partially sent: it returns malformed/finalize-raw while retaining
all already-bounded Phase 5 raw frames. The timeout covers semaphore admission and
backend execution. Dropping or cancelling the stage drops the backend future; no
background task is detached.

Safe metrics use only classification, status, disposition and stable diagnostic code.
Project/Event IDs, filenames, addresses, debug IDs, release/dist, backend text and
payload values are forbidden labels and logs.

## Verification

Required verification covers not-required, complete, partial, missing, malformed,
timeout, unavailable and cancellation vectors; raw-frame equality for every outcome;
bounded concurrency and oversized request behavior; a reusable scripted fake backend;
compile-time dependency direction; and one recorded classification/baseline CPU RPS
result. No real network or storage integration belongs to this phase.
