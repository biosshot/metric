import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error(
    "usage: node performance/compare-notifications.mjs <baseline.json> <candidate.json> [max-regression-percent]",
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
    artifact.metadata?.scenario !== "notification-transition-expansion-phase-20" ||
    !artifact.metrics
  ) {
    throw new Error("unsupported or incomplete Phase 20 benchmark artifact");
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
if (baseline.metrics.transitions !== candidate.metrics.transitions) {
  throw new Error("incomparable transition count");
}
const change =
  ((candidate.metrics.expansion_rps - baseline.metrics.expansion_rps) /
    baseline.metrics.expansion_rps) *
  100;
console.log(
  `expansion_rps: ${baseline.metrics.expansion_rps} -> ${candidate.metrics.expansion_rps} (${change.toFixed(2)}%)`,
);
const failures = [];
if (!(candidate.metrics.expansion_rps > 0) || change < -budget) {
  failures.push(`expansion_rps regressed by ${(-change).toFixed(2)}%`);
}
if (
  candidate.metrics.expansion_rps <
  candidate.metrics.minimum_expansion_rps
) {
  failures.push("expansion_rps is below gate");
}
if (failures.length) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
