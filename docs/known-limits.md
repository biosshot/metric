# Known limits

Metric 0.1.0 is an early release. Read these limits before using it for important
production data.

## Updates

This version requires MongoDB schema generation **19 exactly**. It can prepare an
empty database, but it cannot migrate data from an older generation. Read
[Upgrading](upgrading.md) before changing versions.

## Deployment

- Metric currently runs as one application container.
- The supplied MongoDB deployment is a single server, not a high-availability
  cluster.
- Sharding and multiple Metric processing nodes are not supported.
- The supplied Compose file is not a promise of a particular event rate.

## Backup and archive

- Metric does not yet provide its own backup and restore command.
- MongoDB and file storage must be retained together.
- Cold archives cannot be searched or restored through Metric.

## Features

- Profiling is not supported.
- Session Replay must be enabled for each project.
- An external Symbolicator is optional and operated separately.
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
