import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-artifact-bundles.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}
const [, , baselinePath, candidatePath, budgetText = "20"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "javascript-artifact-bundles-phase-18" || !artifact.metrics) {
    throw new Error("unsupported or incomplete Phase 18 artifact benchmark");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "storage"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}`);
  }
}
if (baseline.metrics.samples_per_lookup_outcome !== candidate.metrics.samples_per_lookup_outcome) {
  throw new Error("incomparable sample count");
}
const failures = [];
for (const field of ["modern_hit_rps", "legacy_hit_rps", "miss_rps", "open_circuit_rps"]) {
  const before = baseline.metrics[field];
  const after = candidate.metrics[field];
  const change = ((after - before) / before) * 100;
  console.log(`${field}: ${before} -> ${after} (${change.toFixed(2)}%)`);
  if (!(after > 0) || change < -budget) failures.push(`${field} regressed by ${(-change).toFixed(2)}%`);
}
for (const field of ["modern_hit_rps", "legacy_hit_rps", "miss_rps"]) {
  if (candidate.metrics[field] < candidate.metrics.minimum_lookup_rps) failures.push(`${field} is below gate`);
}
if (candidate.metrics.open_circuit_rps < candidate.metrics.minimum_open_circuit_rps) failures.push("open_circuit_rps is below gate");
if (failures.length) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
