import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-incident-capsule.mjs <baseline.json> <candidate.json> [max-regression-percent]");
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
    artifact.metadata?.scenario !== "incident-capsule-phase-19" ||
    !artifact.metrics
  ) {
    throw new Error("unsupported or incomplete Phase 19 benchmark artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "storage"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}`);
  }
}
if (baseline.metrics.samples !== candidate.metrics.samples) {
  throw new Error("incomparable sample count");
}
const change =
  ((candidate.metrics.capsule_rps - baseline.metrics.capsule_rps) /
    baseline.metrics.capsule_rps) *
  100;
console.log(
  `capsule_rps: ${baseline.metrics.capsule_rps} -> ${candidate.metrics.capsule_rps} (${change.toFixed(2)}%)`,
);
const failures = [];
if (!(candidate.metrics.capsule_rps > 0) || change < -budget) {
  failures.push(`capsule_rps regressed by ${(-change).toFixed(2)}%`);
}
if (candidate.metrics.capsule_rps < candidate.metrics.minimum_capsule_rps) {
  failures.push("capsule_rps is below gate");
}
if (failures.length) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
