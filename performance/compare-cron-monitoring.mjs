import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error(
    "usage: node performance/compare-cron-monitoring.mjs <baseline.json> <candidate.json> [max-regression-percent]",
  );
  process.exit(2);
}
const [, , baselinePath, candidatePath, budgetText = "20"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (
    artifact.schema_version !== 1 ||
    artifact.metadata?.scenario !==
      "cron-monitor-run-durable-writer-phase-35" ||
    !artifact.metrics
  ) {
    throw new Error("unsupported or incomplete Phase 35 benchmark artifact");
  }
}
for (const field of [
  "hardware",
  "rust_toolchain",
  "mongodb_version",
  "fixture",
  "storage",
]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}`);
  }
}
if (
  baseline.metrics.runs !== candidate.metrics.runs ||
  baseline.metrics.batch !== candidate.metrics.batch
) {
  throw new Error("incomparable monitor-run fixture");
}
const change =
  ((candidate.metrics.durable_rps - baseline.metrics.durable_rps) /
    baseline.metrics.durable_rps) *
  100;
console.log(
  `durable_rps: ${baseline.metrics.durable_rps} -> ${candidate.metrics.durable_rps} (${change.toFixed(2)}%)`,
);
if (
  !(candidate.metrics.durable_rps > 0) ||
  change < -budget ||
  candidate.metrics.durable_rps < candidate.metrics.minimum_durable_rps
) {
  console.error(
    "performance regression: durable monitor-run RPS is outside the gate",
  );
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
