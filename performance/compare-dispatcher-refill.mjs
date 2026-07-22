import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-dispatcher-refill.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}
const [, , baselinePath, candidatePath, budgetText = "10"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "dispatcher-mongodb-refill-phase-4" || !artifact.metrics) {
    throw new Error("unsupported or incomplete dispatcher-refill artifact");
  }
}
for (const field of ["mongo_server", "mongo_driver", "fixture", "query_order"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`);
  }
}
const change = ((candidate.metrics.refill_rps - baseline.metrics.refill_rps) / baseline.metrics.refill_rps) * 100;
console.log(`refill RPS: ${baseline.metrics.refill_rps} -> ${candidate.metrics.refill_rps} (${change.toFixed(2)}%)`);
const failures = [];
if (change < -budget) failures.push(`refill RPS regressed by ${(-change).toFixed(2)}%`);
if (candidate.metrics.refill_rps < candidate.metrics.minimum_recovery_rps) failures.push("refill RPS is below 7,500/s recovery gate");
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
