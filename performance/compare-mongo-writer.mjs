import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-mongo-writer.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}
const [, , baselinePath, candidatePath, budgetText = "10"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);

const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "mongo-writer-phase-3" || !artifact.metrics) {
    throw new Error("unsupported or incomplete mongo-writer artifact");
  }
}
for (const field of ["mongo_server", "mongo_driver", "unique_events", "concurrency"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`);
  }
}

const percent = (before, after) => before === 0 ? (after === 0 ? 0 : Infinity) : ((after - before) / before) * 100;
const checks = [
  ["RPS", baseline.metrics.rps, candidate.metrics.rps, "lower"],
  ["p95", baseline.metrics.p95_ms, candidate.metrics.p95_ms, "higher"],
  ["p99", baseline.metrics.p99_ms, candidate.metrics.p99_ms, "higher"],
  ["batch occupancy", baseline.metrics.average_batch_occupancy, candidate.metrics.average_batch_occupancy, "lower"],
];
const failures = [];
for (const [name, before, after, direction] of checks) {
  const change = percent(before, after);
  console.log(`${name}: ${before} -> ${after} (${change.toFixed(2)}%)`);
  if ((direction === "lower" && change < -budget) || (direction === "higher" && change > budget)) {
    failures.push(`${name} regressed by ${Math.abs(change).toFixed(2)}%`);
  }
}
if (candidate.metrics.rps < candidate.metrics.minimum_gate_rps) failures.push("RPS is below 5,000/s");
if (candidate.metrics.p95_ms >= candidate.metrics.maximum_gate_p95_ms) failures.push("p95 is at or above 100 ms");
if (candidate.metrics.p99_ms >= candidate.metrics.maximum_gate_p99_ms) failures.push("p99 is at or above 250 ms");
if (candidate.metrics.acknowledged_loss !== 0) failures.push("acknowledged Event loss is non-zero");
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
