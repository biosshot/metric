import { readFileSync } from "node:fs";

if (process.argv.length < 4) {
  throw new Error(
    "usage: node performance/compare-log-writer.mjs <baseline.json> <candidate.json> [max-regression-percent]",
  );
}

const baseline = JSON.parse(readFileSync(process.argv[2], "utf8"));
const candidate = JSON.parse(readFileSync(process.argv[3], "utf8"));
const budget = Number(process.argv[4] ?? "20");
if (
  baseline.metadata.scenario !== candidate.metadata.scenario ||
  baseline.metrics.records !== candidate.metrics.records ||
  baseline.metrics.concurrency !== candidate.metrics.concurrency
) {
  throw new Error("Log writer artifacts are not like-for-like");
}
const change =
  ((candidate.metrics.rps - baseline.metrics.rps) / baseline.metrics.rps) * 100;
console.log(
  `Log writer RPS: ${baseline.metrics.rps} -> ${candidate.metrics.rps} (${change.toFixed(2)}%)`,
);
console.log(
  `Average batch occupancy: ${baseline.metrics.average_batch_occupancy} -> ${candidate.metrics.average_batch_occupancy}`,
);
const failures = [];
if (change < -budget) failures.push(`RPS regressed by ${(-change).toFixed(2)}%`);
if (candidate.metrics.rps < candidate.metrics.minimum_rps) {
  failures.push("RPS is below the Phase 24 gate");
}
if (
  candidate.metrics.average_batch_occupancy <
  candidate.metrics.minimum_average_batch_occupancy
) {
  failures.push("average batch occupancy is below the Phase 24 gate");
}
if (candidate.metrics.acknowledged_loss !== 0) {
  failures.push("acknowledged Log loss is non-zero");
}
if (failures.length > 0) {
  throw new Error(failures.join("; "));
}
console.log(`PASS: no regression beyond ${budget}% and Phase 24 gates passed`);
