# Capacity measurement

The 100-million-Event/day objective is a workload envelope, not a hardware-independent
promise. ADR-0037 translates it to 1,158 accepted Events/s average, 5,000/s steady
headroom and a bounded 20,000/s burst.

Generate a read-only aggregate report from a local MongoDB database:

```powershell
./scripts/run-capacity-report.ps1 `
  -MongoUri 'mongodb://127.0.0.1:27017/?retryWrites=false' `
  -Database faultkeep `
  -AcceptedRps 1158 `
  -RetentionDays 30 `
  -ReplicationFactor 1
```

The report samples at most 10,000 newest Events, calculates their actual BSON sizes,
and combines them with MongoDB `collStats` collection storage and total index bytes.
It publishes no Event body, identifier, database name, query or secret.

Projected numbers exclude journal, oplog, temporary index builds, free-space reserve,
BlobStore objects, backups and network copies. Add those backend-specific costs
before sizing production hardware. A report from a small or unrepresentative dataset
is evidence about that dataset only.

Run a short local durable regression profile with:

```powershell
./performance/run-release-load.ps1 -Rps 5000 -Duration 15s
```

The runner records achieved RPS, p95/p99, dropped iterations, TCP failures and HTTP
`200`/`429`/`503`/other counts. It verifies that every HTTP 200 has exactly one
durable Event, stops its benchmark process in `finally`, and drops only a validated
`faultkeep_phase22_*` database.

The actual release gate requires controlled-hardware 5,000/s for 60 minutes,
20,000/s for 5 minutes, backlog recovery above 1.5 times arrival, restart, retention
interference and long soak. Short Windows runs are regression sentinels only.
