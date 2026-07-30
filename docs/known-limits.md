# Version-one known limits

- Only SDK rows marked `pass` in the compatibility matrix are supported claims.
- Session Replay is implemented for the pinned browser contract and is disabled per
  project by default. Profiling remains deliberately deferred. Legacy StatsD metric
  items are not accepted.
- The runtime is one `--role all` process. Split roles, NATS, distributed claims,
  sharding and disk spool are not implemented.
- The current binary requires MongoDB schema generation 19 exactly. It bootstraps
  only an empty database. There is no supported migration from generation 18,
  online migration, rolling mixed-version upgrade or downgrade rewrite; see
  [Schema compatibility and upgrades](upgrading.md).
- Archive objects cannot be searched, restored or rehydrated through Metric.
- External Symbolicator is optional and separately operated; ProGuard, IL2CPP,
  BCSymbolMap and Hermes-specific extended pipelines are outside version one.
- Webhook delivery is at least once. Receivers must deduplicate the stable delivery
  identifier.
- Application Metric increments are at least once. Retrying a container after an
  ambiguous response can apply its counter/sum/count values again.
- There is no application-consistent backup/restore protocol or universal
  MongoDB/BlobStore reconciliation scanner.
- The supplied compose file is a simple single-MongoDB/local-BlobStore deployment,
  not high availability or a 100-million-Event/day capacity guarantee.
- MCP, teams, SSO/SCIM/MFA/passkeys and advanced permission models are not included.
- Replay privacy will rely on the configured pinned Sentry browser SDK/rrweb masking
  policy. No server-side DOM-aware privacy masking layer is planned.
