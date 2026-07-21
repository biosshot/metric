import { readFileSync } from "node:fs";

function usage() {
  console.error(
    "usage: node performance/compare-k6.mjs <baseline.json> <candidate.json> [max-regression-percent]",
  );
  process.exit(2);
}

if (process.argv.length < 4 || process.argv.length > 5) usage();

const [, , baselinePath, candidatePath, budgetText = "10"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) usage();

const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));

for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || !artifact.metadata || !artifact.metrics) {
    throw new Error("unsupported or incomplete k6 artifact");
  }
}

for (const field of ["fixture_revision", "target_rps", "mode"]) {
  const before = baseline.metadata[field] ?? "arrival-rate";
  const after = candidate.metadata[field] ?? "arrival-rate";
  if (before !== after) {
    throw new Error(`incomparable metadata ${field}: ${before} != ${after}`);
  }
}

function percentChange(before, after) {
  return before === 0 ? (after === 0 ? 0 : Infinity) : ((after - before) / before) * 100;
}

const rpsChange = percentChange(
  baseline.metrics.achieved_rps,
  candidate.metrics.achieved_rps,
);
const p95Change = percentChange(
  baseline.metrics.latency_ms.p95,
  candidate.metrics.latency_ms.p95,
);
const errorChange = candidate.metrics.error_rate - baseline.metrics.error_rate;
const dropped = candidate.metrics.dropped_iterations ?? 0;

console.log(`RPS: ${baseline.metrics.achieved_rps.toFixed(2)} -> ${candidate.metrics.achieved_rps.toFixed(2)} (${rpsChange.toFixed(2)}%)`);
console.log(`p95: ${baseline.metrics.latency_ms.p95.toFixed(3)} ms -> ${candidate.metrics.latency_ms.p95.toFixed(3)} ms (${p95Change.toFixed(2)}%)`);
console.log(`error rate delta: ${errorChange.toFixed(6)}; dropped iterations: ${dropped}`);

const failures = [];
if (rpsChange < -budget) failures.push(`RPS regressed by ${(-rpsChange).toFixed(2)}%`);
if (p95Change > budget) failures.push(`p95 regressed by ${p95Change.toFixed(2)}%`);
if (errorChange > 0) failures.push("error rate increased");
if (dropped > 0) failures.push(`${dropped} iterations were dropped`);

if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}

console.log(`PASS: no regression beyond ${budget}%`);
