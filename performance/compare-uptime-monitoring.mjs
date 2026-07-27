import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-uptime-monitoring.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}
const [, , baselinePath, candidatePath, budgetText = "20"] = process.argv;
const budget = Number(budgetText);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "uptime-monitor-run-durable-writer-phase-36") {
    throw new Error("unsupported Phase 36 benchmark artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "storage"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) throw new Error(`incomparable metadata ${field}`);
}
const change =
  ((candidate.metrics.durable_rps - baseline.metrics.durable_rps) /
    baseline.metrics.durable_rps) *
  100;
console.log(`durable_rps: ${baseline.metrics.durable_rps} -> ${candidate.metrics.durable_rps} (${change.toFixed(2)}%)`);
if (change < -budget || candidate.metrics.durable_rps < candidate.metrics.minimum_durable_rps) process.exit(1);
console.log(`PASS: no regression beyond ${budget}%`);
