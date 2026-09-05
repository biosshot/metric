# Manual backup/restore verification — 2026-09-05

Related request: [issue #5](https://github.com/biosshot/metric/issues/5).

## Scope

Document external, database-scoped backup and restore without changing application
code, schema generation 20, CLI commands, runtime dependencies or deployment
behavior. PostgreSQL and a built-in backup subsystem remain outside this work.

The procedure pauses only Metric. It does not acquire a MongoDB `fsync` lock,
disable TTL, use a full-server dump or require `--oplog`. Other databases can keep
accepting writes. The TTL/cross-collection consistency limit is explicit: this is
not an atomic point-in-time snapshot. Retaining MongoDB and BlobStore together
does not imply atomicity just because the application is stopped.

## Environment

- Windows host, Docker Desktop Linux containers, Docker Engine **28.3.2**.
- Unchanged locally built Metric image `metric-helm-test:0.1.5`, image ID
  `sha256:0aca094afef4f17234bc0a6fbddaf6d46124e28f935f52056882363c1be29952`.
  Application source matches the baseline behind commit `677c735`; its Helm
  changes do not change the Rust binary. This is **not** a claim that the older
  public `v0.1.5` image contains this checkout's generation-20 behavior.
- MongoDB `mongo:8.0.12`; bundled `mongodump` and `mongorestore` both
  **100.12.2**, Linux/amd64.
- MinIO `RELEASE.2025-09-07T16-13-09Z`, digest
  `sha256:14cea493d9a34af32f524e538b8346cf79f3321eff8e708c1e2960462bd8936e`.
  This is a disposable interoperability fixture, not a production-version recommendation.
- AWS CLI **2.36.21**, image digest
  `sha256:fb09d91d0640088f15475d9a9366a215c88a24f9871ae0f6124bede0d2157e9a`.
- Low application profile, attachments enabled, Symbolicator/cold archive disabled.
  MongoDB cache `0.256` GB. Application/MongoDB test containers had 768 MiB memory
  ceilings. This was not a memory or capacity benchmark.

All databases, volumes, buckets, accounts and secrets were generated fixtures.
Resources used the `metric-backup-lab-20260905` prefix. Only Metric HTTP ports were
published, on loopback with dynamically allocated ports. Host MongoDB, user data,
production S3 and real notification/uptime endpoints were not used. A temporary
Python harness under ignored `target/manual-backup-lab` orchestrated commands and
HTTP checks; it is not a shipped backup utility.

## Procedure exercised

1. Create an owner and project through Metric's API. Send real Sentry envelopes
   with errors and JSON attachments, then retrieve the resulting data.
2. Stop source Metric, keeping MongoDB and S3 running.
3. Use `mongodump --db DATABASE --archive=PATH --gzip --numParallelCollections=1`.
   Copy the archive using Docker, without Windows text redirection.
4. Restore to an empty database with `mongorestore --archive=PATH --gzip
   --nsInclude='DATABASE.*' --stopOnError --numParallelCollections=1
   --numInsertionWorkersPerCollection=1`. Test both namespace remapping and a
   fresh separate MongoDB server without remapping.
5. For local storage, tar a read-only source volume and extract into a fresh volume,
   preserving paths and ownership.
6. For S3, copy source bucket -> fresh backup prefix -> empty target bucket using
   `aws s3 sync --copy-props default`; compare object sizes and user metadata.
7. Start a separate Metric instance using the restored database/files and original
   application key. Verify login, project/event retrieval, attachment bytes and
   ingestion through the original DSN key.
8. Repeat the operator workflow using the unchanged `deploy/compose.yml` under a
   unique Compose project: start only MongoDB, verify an empty destination, use
   `compose cp` and `compose exec` for restore, extract files, start Metric, verify
   APIs, stop only Metric and create another scoped dump.

## Results

| Check | Observed result |
| --- | --- |
| Local database round trip | All **38** collections retained, including empty collections; per-collection counts, index definitions and validators matched before application startup |
| S3-backed database round trip | Same 38-collection/index/validator checks passed |
| Local BlobStore round trip | Original account/project/event readable; attachment bytes identical; old DSN accepted a new event |
| S3 BlobStore round trip | Same API checks; `metric-blake3`, `metric-kind`, `metric-created-ms` preserved |
| Separate MongoDB server | New server initialized its own credentials; logical restore, local files and tested API path passed |
| Unchanged Compose deployment | Fresh project/database/volume restore and subsequent scoped dump passed using the documented command forms |
| Neighboring database | **16** acknowledged writes during scoped dump/restore runs; no global lock; unrelated collections absent from restored databases |
| Least-privilege source | A user with only `read` on the Metric fixture database could dump it; dumping the neighboring database with that user failed |
| Password handling | Database Tools accepted interactive/stdin passwords without secrets in argv; `umask 077` produced a mode-600 dump |
| `--oplog` with `--db` | Tools rejected the combination; no full-server fallback |
| Multipart S3 copy | Separate 10 MiB random tool fixture retained bytes and metadata through both S3 copy legs; SHA-256 matched |
| S3 -> ordinary files -> S3 | Negative test: all three Metric metadata fields lost; not a valid backup recipe |
| Missing local attachment | Download returned HTTP **503**, `temporarily_unavailable`; `/ready` passed and the event remained readable |
| Truncated dump | `mongorestore` returned nonzero; partial destination was not started as a recovered installation |
| Events-only restore | Metric exited with `configured database contains data but no Metric schema`; one collection is insufficient |
| TTL while Metric stopped | Two fixture events deliberately expired; five successive scoped dumps bracketed normal TTL deletion. Restored Metric started, allowed login/project access and accepted a new event |

The multipart object exercises S3 property-copy mechanics; it is not a
Metric-ingested 10 MiB attachment. Its synthetic checksum metadata is not a Metric
integrity claim. The real application attachment was separately copied and
downloaded byte-for-byte through Metric.

TTL testing demonstrates this bounded expiry scenario only. It does not establish
that arbitrary document corruption, missing control-plane collections or live
concurrent writers are harmless. A ready server and a complete recoverable backup
are different properties. Expired data is not promised to survive restoration.

## Documentation and verification limits

`docs/backup-restore.md` supplies the scoped dump, local/S3 copies, protected
credentials, empty-target restore and checks. Operations, upgrading, known limits,
capacity and the sidebar link to it or explain the resource/consistency boundary.
Configuration documentation now explains the recovery significance of
`SCRUB_HMAC_KEY`.

- Tested MongoDB 8.0.12 standalone / Tools 100.12.2, not every server/tool/FCV
  combination. Reproduce source versions before separately upgrading.
- S3 tested against MinIO, not live AWS, KMS, cross-endpoint transfer, bucket
  replication or version-history recovery.
- Small functional fixtures, not throughput, long-duration soak, maximum RAM or
  recovery-time evidence. Low concurrency does not eliminate I/O load.
- Real Replay recordings, debug-file/source-map packages, enabled cold archives
  and real notification delivery were not separately round-tripped. Their
  storage/secret dependencies are documented, not claimed as independent tests.
- No built-in scheduling, incremental backup, Metric-managed encryption,
  partial-restore rollback or lossless online backup.
- No application tests were changed or a full Rust suite rerun: the runtime binary
  was unchanged. No release, issue comment or issue closure was performed.

## Final checks and cleanup

- `python scripts/validate-documentation.py` passed.
- `python scripts/validate-deployment-profiles.py` passed.
- `npm run docs:build --prefix docs` passed, including the new guide and sidebar.
- `git diff --check` passed. Tracked changes are documentation/navigation only.
- The disposable Compose project and its data volumes were removed after its
  round trip. Remaining labelled test containers, named volumes and test networks
  were removed after inspection of their exact ownership. Docker images remain
  cached; small fixture archives and the local harness remain under ignored `target`.

## References

- [MongoDB dump/oplog constraints](https://www.mongodb.com/docs/database-tools/mongodump/).
- [Restore behavior and compatibility](https://www.mongodb.com/docs/database-tools/mongorestore/mongorestore-behavior-access-usage/).
- [MongoDB TTL behavior](https://www.mongodb.com/docs/manual/core/index-ttl/).
- [AWS CLI property copying](https://docs.aws.amazon.com/cli/latest/reference/s3/sync.html).
- [Docker volume backup and restore](https://docs.docker.com/engine/storage/volumes/#back-up-restore-or-migrate-data-volumes).
