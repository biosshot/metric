# Capacity and sizing

Metric does not have one hardware requirement that fits every installation.
Capacity depends on:

- how many events your applications send;
- average event and attachment size;
- enabled features and retention periods;
- MongoDB and disk performance;
- Symbolicator workload and cache size;
- whether all containers share the same machine.

## Reference result

A committed reference test reached 4,983 durable error events per second with no
acknowledged loss. It used:

- AMD Ryzen 5 5600H, 6 cores and 12 threads;
- 16 GiB RAM;
- MongoDB 8 on the same machine;
- a 5,000 events-per-second target for 15 seconds.

This result proves that workload on that machine. It is not a guarantee for a
different event shape, configuration or server.

Raw results are available in the
[`performance/baselines`](https://github.com/biosshot/metric/tree/main/performance/baselines)
directory.

## Plan disk space

Metric stores data in two places:

- MongoDB stores events, issues, users and settings;
- file storage stores attachments, replays and other large objects.

The supplied Docker setup keeps them in the `mongo-data` and `blob-data` volumes.
Monitor free disk space for both volumes. Include extra space for MongoDB indexes,
temporary work and backups.

Symbolicator uses a third `symbolicator-cache` volume. It can be rebuilt, but it
also needs free disk space while Metric is running.

## Measure your installation

For an important deployment, test with event sizes and traffic similar to your
applications. Start below the expected peak, increase the rate gradually and
watch:

- HTTP `429` and `503` responses;
- `/ready` failures;
- event processing delay;
- MongoDB CPU, memory and disk use;
- free space in the data and Symbolicator cache volumes.

Developers who cloned the repository can use the repeatable load scripts described
in [`performance/README.md`](https://github.com/biosshot/metric/blob/main/performance/README.md).
