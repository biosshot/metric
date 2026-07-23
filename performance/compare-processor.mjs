import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-processor.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}

const [, , baselinePath, candidatePath, budgetText = "15"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "processor-recovery-phase-10" || !artifact.metrics) {
    throw new Error("unsupported or incomplete Processor artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "storage"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`);
  }
}

const change = ((candidate.metrics.recovery_rps - baseline.metrics.recovery_rps) /
  baseline.metrics.recovery_rps) * 100;
console.log(
  `recovery_rps: ${baseline.metrics.recovery_rps} -> ${candidate.metrics.recovery_rps} (${change.toFixed(2)}%)`,
);
console.log(`recovery_ratio: ${candidate.metrics.recovery_ratio.toFixed(2)}x`);
const failures = [];
if (change < -budget) failures.push(`recovery RPS regressed by ${(-change).toFixed(2)}%`);
if (candidate.metrics.recovery_ratio < candidate.metrics.minimum_recovery_ratio) {
  failures.push("ADR-0037 recovery ratio is below 1.5x");
}
for (const field of ["events", "concurrency", "accepted_steady_rps"]) {
  if (candidate.metrics[field] !== baseline.metrics[field]) failures.push(`${field} changed`);
}
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}% and recovery ratio gate passed`);
