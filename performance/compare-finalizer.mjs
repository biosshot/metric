import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-finalizer.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}

const [, , baselinePath, candidatePath, budgetText = "15"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "finalizer-phase-9" || !artifact.metrics) {
    throw new Error("unsupported or incomplete Finalizer artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "storage"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`);
  }
}

const change = ((candidate.metrics.rps - baseline.metrics.rps) / baseline.metrics.rps) * 100;
console.log(`rps: ${baseline.metrics.rps} -> ${candidate.metrics.rps} (${change.toFixed(2)}%)`);
const failures = [];
if (change < -budget) failures.push(`RPS regressed by ${(-change).toFixed(2)}%`);
if (candidate.metrics.rps < candidate.metrics.minimum_rps) failures.push("RPS is below gate");
for (const field of ["events", "issues"]) {
  if (candidate.metrics[field] !== baseline.metrics[field]) failures.push(`${field} changed`);
}
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
