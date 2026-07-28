# Phase 38 module contract: Session Replay

## Scope and compatibility

Phase 38 accepts only a paired `replay_event` and `replay_recording` from the pinned
`@sentry/browser` 10.66.0 path. The player pins `rrweb-player` 2.1.1. Replay is
disabled for a project by default and does not enable Profiling or another SDK
family.

## Data flow and ownership

```text
bounded Envelope parser
  -> paired Replay metadata + recording
  -> pinned SDK and metadata validation
  -> bounded raw/zlib rrweb structural validation
  -> dedicated ReplaySink / ReplayWriter byte-bounded channel
  -> immutable BlobStore recording segment
  -> compact MongoDB replays manifest
  -> durable HTTP acknowledgement
```

The Blob object is committed before its manifest reference. A duplicate segment with
the same identity, size and checksum is accepted as a duplicate; conflicting bytes
fail closed. A crash between the Blob and MongoDB commits leaves an orphan that the
Replay writer removes after the configured grace period.

Replay never consumes Error, Log, Span or Metric channel permits. Its channel
capacity, queued-byte budget, segment size and operation deadline are independent.

## Bounds and privacy

- compressed segment bytes are bounded by the Envelope and writer limits;
- decompression streams into a bounded buffer and rejects bombs before unbounded
  allocation;
- the rrweb array is visited one event at a time and has an explicit event-count
  bound;
- segment identity is at most 100 per Replay, sorted by `segment_id`;
- Replay duration and correlation lists are bounded;
- the Web player requires an explicit load and caps segments, decompressed bytes and
  events before mounting.

Searchable metadata uses the ordinary scrubber. Recording bytes are opaque untrusted
content after structural, compression, size and integrity validation. Metric does
not implement DOM-aware server-side masking and never indexes or logs recording
contents. Operators must use the pinned SDK masking configuration; the Web UI states
this limitation.

## Storage, retention and deletion

`replays` stores one compact project-scoped manifest with ordered Blob references,
bounded Error/Trace correlations and optional environment/release/URL metadata.
Recording segments use the project/replay/segment Blob key and checksum contract.

`retention.replays_days` controls the metadata TTL and is independent from other
signals. When `retention.replay_archive = true`, the manifest receives an archive
hold/due marker instead of a TTL; the immutable recording is already in BlobStore.
Project deletion owns both the `replays` dataset and Replay-recording Blob namespace.
Schema generation 19 is an intentional breaking empty-database schema with no
migration from generation 18.

## API and product surface

Project-read authorization protects Replay list, detail and raw segment access.
Every raw segment read emits a durable `replay.accessed` audit record. The Vue routes
are `/replays` and `/replays/:replayId`; the player dependency is lazy-loaded and
does not enter the primary Web bundle.

Replay metadata links exact Error IDs and Trace IDs. Feedback can be filtered by
`replay_id`, and the detail UI exposes linked Errors, Feedback, Logs and Traces
without searching recording contents.
