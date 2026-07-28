import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error(
    "usage: node performance/compare-session-replay.mjs <baseline.json> <candidate.json> [max-regression-percent]",
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
    artifact.metadata?.scenario !== "session-replay-validation-phase-38"
  ) {
    throw new Error("unsupported Session Replay artifact");
  }
}
for (const field of ["rust_toolchain", "hardware", "fixture", "scope"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}`);
  }
}
const change =
  ((candidate.metrics.validation_rps - baseline.metrics.validation_rps) /
    baseline.metrics.validation_rps) *
  100;
console.log(
  `Replay validation RPS: ${baseline.metrics.validation_rps} -> ${candidate.metrics.validation_rps} (${change.toFixed(2)}%)`,
);
const failures = [];
if (change < -budget) failures.push(`validation RPS regressed by ${(-change).toFixed(2)}%`);
if (candidate.metrics.validation_rps < candidate.metrics.minimum_validation_rps) {
  failures.push("validation RPS is below the minimum gate");
}
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
