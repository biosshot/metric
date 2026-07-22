import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-normalizer.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}
const [, , baselinePath, candidatePath, budgetText = "10"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "normalizer-adr0037-phase-5" || !artifact.metrics) {
    throw new Error("unsupported or incomplete Normalizer artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "weights", "normalizer_limits", "allocation_proxy"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`);
  }
}

const failures = [];
for (const field of ["size_1k", "size_4k", "size_16k", "size_128k", "weighted"]) {
  const before = baseline.metrics.rps[field];
  const after = candidate.metrics.rps[field];
  const change = ((after - before) / before) * 100;
  console.log(`${field} RPS: ${before} -> ${after} (${change.toFixed(2)}%)`);
  if (change < -budget) failures.push(`${field} RPS regressed by ${(-change).toFixed(2)}%`);
}
for (const field of ["size_1k", "size_4k", "size_16k", "size_128k"]) {
  const before = baseline.metrics.canonical_output_bytes[field];
  const after = candidate.metrics.canonical_output_bytes[field];
  const change = ((after - before) / before) * 100;
  console.log(`${field} output bytes: ${before} -> ${after} (${change.toFixed(2)}%)`);
  if (change > budget) failures.push(`${field} output allocation proxy grew by ${change.toFixed(2)}%`);
}
if (candidate.metrics.rps.weighted < candidate.metrics.minimum_weighted_rps) {
  failures.push("weighted RPS is below the 7,500/s recovery gate");
}
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
