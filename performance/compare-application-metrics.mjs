import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error(
    "usage: node performance/compare-application-metrics.mjs <baseline.json> <candidate.json> [max-regression-percent]",
  );
  process.exit(2);
}
const [, , baselinePath, candidatePath, budgetText = "15"] = process.argv;
const budget = Number(budgetText);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (
    artifact.schema_version !== 1 ||
    artifact.metadata?.scenario !== "application-metrics-streaming-fold-phase-37"
  ) {
    throw new Error("unsupported Application Metrics artifact");
  }
}
for (const field of ["rust_toolchain", "hardware", "fixture", "scope"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}`);
  }
}
const change =
  ((candidate.metrics.measurement_rps - baseline.metrics.measurement_rps) /
    baseline.metrics.measurement_rps) *
  100;
console.log(
  `measurement RPS: ${baseline.metrics.measurement_rps} -> ${candidate.metrics.measurement_rps} (${change.toFixed(2)}%)`,
);
const failures = [];
if (change < -budget) failures.push(`measurement RPS regressed by ${(-change).toFixed(2)}%`);
if (candidate.metrics.measurement_rps < candidate.metrics.minimum_measurement_rps) {
  failures.push("measurement RPS is below the minimum gate");
}
if (candidate.metrics.deltas_per_container !== 1) {
  failures.push("1000 same-series measurements no longer collapse to one delta");
}
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
