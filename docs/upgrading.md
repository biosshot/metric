# Schema compatibility and upgrades

## Current compatibility boundary

The current Metric binary requires MongoDB schema generation **19 exactly**. The
runtime constant
[`SCHEMA_GENERATION`](https://github.com/biosshot/metric/blob/main/crates/mongo/src/lib.rs)
is the source of truth; this page and
[`arch-docs/README.md`](https://github.com/biosshot/metric/blob/main/arch-docs/README.md)
are its operator-facing
summaries.

Startup behavior is fail-closed:

| MongoDB state | Current binary behavior | Operator action |
| --- | --- | --- |
| Empty database | Bootstraps generation 19 idempotently | Allow startup to complete, then verify readiness |
| Complete generation 19 | Validates collections, modules and indexes, then starts | No schema action |
| Generation 18 or older | Refuses startup as incompatible | Stop the upgrade and preserve the database |
| Newer or otherwise different schema | Refuses startup as incompatible | Use the binary that owns that schema |
| Non-empty database without Metric metadata | Refuses startup | Check the configured database name; do not adopt or erase it |

Empty-database bootstrap is not a migration mechanism. Metric currently has no
online migration, automatic migration, mixed-generation rolling upgrade or
supported data-preserving conversion from generation 18 to 19.

> **Data-loss warning:** never drop or recreate a data-bearing MongoDB database to
> satisfy the current binary. Never edit the `schema_meta` generation manually.
> Changing the marker does not create or transform the collections, validators,
> indexes or BlobStore references required by generation 19.

If an installation still contains generation 18 or older production data, keep the
matching old binary and configuration available, stop the upgrade, and take a
backend-native backup before doing anything else. A data-preserving upgrade remains
blocked until a tested offline migration/export procedure for the exact source and
target generations is published.

## Release upgrade checklist

Before replacing a running binary:

1. read the target release notes and confirm its required schema generation;
2. query and record the source installation's schema generation with a read-only
   command against the configured database:

   ```javascript
   db.schema_meta.findOne(
     { _id: "metric.schema" },
     { _id: 1, generation: 1, state: 1, modules: 1 }
   )
   ```

   A missing marker or any generation other than 19 blocks startup of the current
   binary;
3. stop new admission and shut down Metric gracefully;
4. back up MongoDB and the configured BlobStore as one operational unit;
5. retain the previous binary, configuration and secrets needed for rollback;
6. proceed only when the source and target generations are identical, or when an
   explicit tested migration procedure covers that exact transition;
7. start in an isolated environment first and verify `/ready`, representative
   reads, Replay segment retrieval where enabled, and a test ingest.

An independently timed MongoDB snapshot and BlobStore copy are not guaranteed to be
application-consistent. Session Replay stores its compact manifest in MongoDB and
immutable recording segments in BlobStore, so both sides must be retained and
restored together.

## Historical architecture documents

Phase reports and module contracts record the schema generation that their phase
actually tested. Their generation numbers are historical evidence, not current
upgrade targets or runbooks. Use
[`arch-docs/README.md`](https://github.com/biosshot/metric/blob/main/arch-docs/README.md)
to interpret them and use this page
for current operator decisions.
