import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-native-api-query.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}

const [, , baselinePath, candidatePath, budgetText = "15"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "native-api-query-phase-12" || !artifact.metrics) {
    throw new Error("unsupported or incomplete Phase 12 query artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "storage"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`);
  }
}
for (const field of ["dataset_events", "queries", "page_size"]) {
  if (baseline.metrics[field] !== candidate.metrics[field]) {
    throw new Error(`incomparable fixture metric ${field}`);
  }
}

const failures = [];
const rpsChange = ((candidate.metrics.rps - baseline.metrics.rps) / baseline.metrics.rps) * 100;
console.log(`rps: ${baseline.metrics.rps} -> ${candidate.metrics.rps} (${rpsChange.toFixed(2)}%)`);
if (rpsChange < -budget) failures.push(`RPS regressed by ${(-rpsChange).toFixed(2)}%`);
for (const field of ["p95_ms", "p99_ms"]) {
  const change = ((candidate.metrics[field] - baseline.metrics[field]) / baseline.metrics[field]) * 100;
  console.log(`${field}: ${baseline.metrics[field]} -> ${candidate.metrics[field]} (${change.toFixed(2)}%)`);
  if (change > budget) failures.push(`${field} regressed by ${change.toFixed(2)}%`);
}
if (candidate.metrics.rps < candidate.metrics.minimum_rps) failures.push("RPS is below gate");
if (candidate.metrics.p95_ms > candidate.metrics.maximum_p95_ms) failures.push("p95 is above gate");
if (candidate.metrics.p99_ms > candidate.metrics.maximum_p99_ms) failures.push("p99 is above gate");
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
