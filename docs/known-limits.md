# Known limits

Metric 0.1.0 is an early release. Read these limits before using it for important
production data.

## Updates

This version requires MongoDB schema generation **19 exactly**. It can prepare an
empty database, but it cannot migrate data from an older generation. Read
[Update Metric](upgrading.md) before changing versions.

## Deployment

- Metric currently runs as one application container.
- The supplied MongoDB deployment is a single server, not a high-availability
  cluster.
- Metric uses the MongoDB administrator account created inside the supplied
  container. The MongoDB port is not published; do not use that database for
  other applications.
- The supplied Symbolicator 26.6.0 image is a third-party component under
  FSL-1.1-MIT, not Metric's MIT License. Read the
  [third-party notice](https://github.com/biosshot/metric/blob/main/THIRD_PARTY_NOTICES.md).
- Sharding and multiple Metric processing nodes are not supported.
- Capacity depends on your hardware, event size and enabled features. Measure
  your own workload before relying on a particular event rate.

## Backup and archive

- Metric does not yet provide its own backup and restore command.
- MongoDB and file storage must be retained together.
- Cold archives cannot be searched or restored through Metric.

## Features

- Profiling is not supported.
- Session Replay must be enabled for each project.
- Advanced ProGuard, IL2CPP, BCSymbolMap and Hermes processing is not included.
- Single sign-on, SCIM, MFA and passkeys are not included.

## Delivery behavior

- Webhooks may be delivered more than once. Receivers should ignore a repeated
  delivery ID.
- Retrying an application metric after an unclear response can count it twice.
- Replay privacy depends on masking configured in the browser SDK. Metric does not
  add server-side DOM masking.

Only SDK versions listed in [SDK compatibility](compatibility.md) are tested
release claims.
