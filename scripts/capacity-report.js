// Run with mongosh. Output contains only aggregate measurements and safe metadata.
const databaseName = process.env.METRIC_CAPACITY_DATABASE || "metric";
const sampleLimit = Number(process.env.METRIC_CAPACITY_SAMPLE || "1000");
const acceptedRps = Number(process.env.METRIC_CAPACITY_RPS || "1158");
const retentionDays = Number(process.env.METRIC_CAPACITY_RETENTION_DAYS || "30");
const replicationFactor = Number(process.env.METRIC_CAPACITY_REPLICATION || "1");

if (!/^[A-Za-z0-9_-]{1,64}$/.test(databaseName)) {
  throw new Error("capacity database name is invalid");
}
if (!Number.isInteger(sampleLimit) || sampleLimit < 1 || sampleLimit > 10000) {
  throw new Error("capacity sample must be between 1 and 10000");
}
if (!(acceptedRps > 0 && acceptedRps <= 1000000)) {
  throw new Error("capacity RPS is outside the supported report range");
}
if (!Number.isInteger(retentionDays) || retentionDays < 1 || retentionDays > 3650) {
  throw new Error("capacity retention days are outside the supported report range");
}
if (!Number.isInteger(replicationFactor) || replicationFactor < 1 || replicationFactor > 9) {
  throw new Error("capacity replication factor must be between 1 and 9");
}

const database = db.getSiblingDB(databaseName);
const stats = database.runCommand({ collStats: "events", scale: 1 });
if (stats.ok !== 1) {
  throw new Error(`events collStats failed with code ${stats.code || "unknown"}`);
}

const sample = database.events
  .aggregate([
    { $sort: { _id: -1 } },
    { $limit: sampleLimit },
    { $project: { bytes: { $bsonSize: "$$ROOT" } } },
    {
      $group: {
        _id: null,
        sampled_events: { $sum: 1 },
        sampled_bson_bytes: { $sum: "$bytes" },
        minimum_bson_bytes: { $min: "$bytes" },
        maximum_bson_bytes: { $max: "$bytes" },
      },
    },
  ])
  .toArray()[0] || {
  sampled_events: 0,
  sampled_bson_bytes: 0,
  minimum_bson_bytes: 0,
  maximum_bson_bytes: 0,
};
const sampledEvents = Number(sample.sampled_events);
const sampledBsonBytes = Number(sample.sampled_bson_bytes);
const minimumBsonBytes = Number(sample.minimum_bson_bytes);
const maximumBsonBytes = Number(sample.maximum_bson_bytes);

const eventCount = Number(stats.count || 0);
const logicalBytes = Number(stats.size || 0);
const storageBytes = Number(stats.storageSize || 0);
const indexBytes = Number(stats.totalIndexSize || 0);
const averageBsonBytes = sampledEvents === 0 ? 0 : sampledBsonBytes / sampledEvents;
const averageIndexBytes = eventCount === 0 ? 0 : indexBytes / eventCount;
const averageStorageBytes = eventCount === 0 ? 0 : storageBytes / eventCount;
const dailyEvents = acceptedRps * 86400;
const projectedDailyPrimaryBytes = dailyEvents * (averageBsonBytes + averageIndexBytes);
const projectedHotPrimaryBytes = projectedDailyPrimaryBytes * retentionDays;
const representativeDataset = eventCount >= Math.max(sampleLimit, 10000);

const buildInfo = database.runCommand({ buildInfo: 1 });
const report = {
  schema_version: 1,
  metadata: {
    scenario: "metric-capacity-phase-22",
    source_commit: process.env.METRIC_CAPACITY_COMMIT || "working-tree",
    generated_at: new Date().toISOString(),
    rust_toolchain: process.env.METRIC_CAPACITY_RUST || "unrecorded",
    hardware: process.env.METRIC_CAPACITY_HARDWARE || "unrecorded",
    mongodb_version: buildInfo.version || "unknown",
    database: "<configured>",
    measurement: "bounded newest-first logical BSON sample plus collStats",
    representative_dataset: representativeDataset,
  },
  inputs: {
    accepted_events_per_second: acceptedRps,
    retention_days: retentionDays,
    replication_factor: replicationFactor,
    sample_limit: sampleLimit,
  },
  observed: {
    event_count: eventCount,
    sampled_events: sampledEvents,
    average_bson_bytes: averageBsonBytes,
    minimum_bson_bytes: minimumBsonBytes,
    maximum_bson_bytes: maximumBsonBytes,
    collection_logical_bytes: logicalBytes,
    collection_storage_bytes: storageBytes,
    total_index_bytes: indexBytes,
    average_index_bytes_per_event: averageIndexBytes,
    average_storage_bytes_per_event: averageStorageBytes,
    logical_to_storage_ratio: storageBytes === 0 ? 0 : logicalBytes / storageBytes,
  },
  projected: {
    events_per_day: dailyEvents,
    primary_bytes_per_day: projectedDailyPrimaryBytes,
    primary_hot_retention_bytes: projectedHotPrimaryBytes,
    replicated_hot_retention_bytes: projectedHotPrimaryBytes * replicationFactor,
  },
  exclusions: [
    "journal",
    "oplog",
    "temporary index builds",
    "filesystem reserve",
    "BlobStore objects",
    "network and backup copies",
  ],
  warnings: representativeDataset
    ? []
    : [
        "dataset is too small for a production sizing claim",
        "fixed collection and index allocation dominates per-Event measurements",
      ],
};
print(JSON.stringify(report, null, 2));
