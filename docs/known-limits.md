# Known limits

Metric 0.1.7 is an early release. Read these limits before using it for important
production data.

## Updates

The current binary requires MongoDB schema generation **22 exactly** before
starting application workers. Its automatic, forward-only, crash-resumable migration
runner supports generation 19 to 20 to 21 to 22. Earlier generations have no supported transition
chain, so those databases still fail closed.
There are no reverse migrations or mixed-version rolling upgrades. Read
[Update Metric](upgrading.md) before changing versions.

## Deployment

- Metric currently runs as one application container.
- The supplied MongoDB deployment is a single server, not a high-availability
  cluster.
- Metric uses the MongoDB administrator account created inside the supplied
  container. The MongoDB port is not published; do not use that database for
  other applications.
- Medium and High use the supplied Symbolicator 26.6.0 image. It is a
  third-party component under FSL-1.1-MIT, not Metric's MIT License. Read the
  [third-party notice](https://github.com/biosshot/metric/blob/main/THIRD_PARTY_NOTICES.md).
- Min and Low retain raw stack frames but do not process debug files or source
  maps because they omit Symbolicator.
- Profile hardware sizes are starting recommendations, not guaranteed event
  rates. Min and Low do not yet have retained long-duration soak evidence.
- Sharding and multiple Metric processing nodes are not supported.
- Capacity depends on your hardware, event size and enabled features. Measure
  your own workload before relying on a particular event rate.

## Backup and archive

- Metric does not provide its own backup and restore command; use external tooling.
- [Manual backup and restore](backup-restore.md) covers a database-scoped dump and
  local/S3 copies without a global MongoDB lock. TTL cleanup continues; this is not
  a point-in-time snapshot.
- MongoDB, file storage and installation secrets must be retained together.
- Cold archives cannot be searched or restored through Metric.
- With both `archive.enabled` and `retention.metric_archive` enabled, late
  measurements for an old metric bucket can be accepted after its archive snapshot
  has been created. These changes are not automatically re-archived and may be lost
  when the hot copy is removed. Setting `archive.hot_copy_delay = "0s"` does not
  eliminate the window: publication and source commitment are separate operations,
  and MongoDB TTL deletion runs asynchronously. This is an accepted current
  limitation to keep the implementation simple. Its frequency and volume under
  production workloads have not been measured. Metrics archival remains disabled
  in all supplied configurations.

## Features

- Profiling is not supported.
- Session Replay must be enabled for each project.
- Advanced ProGuard, IL2CPP, BCSymbolMap and Hermes processing is not included.
- Single sign-on, SCIM, MFA and passkeys are not included.

## Delivery behavior

- Query downloads are limited to 10,000 rows, 16 MiB and 15 seconds, with at most
  two concurrent exports per Metric process. The response is assembled in bounded
  application memory before it is sent; it is not HTTP chunk streaming.
- Webhooks may be delivered more than once. Receivers should ignore a repeated
  delivery ID.
- Retrying an application metric after an unclear response can count it twice.
- Replay privacy depends on masking configured in the browser SDK. Metric does not
  add server-side DOM masking.

Only SDK versions listed in [SDK compatibility](compatibility.md) are tested
release claims.
