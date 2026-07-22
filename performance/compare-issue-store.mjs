import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-issue-store.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}

const [, , baselinePath, candidatePath, budgetText = "15"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 || artifact.metadata?.scenario !== "issue-store-phase-8" || !artifact.metrics) {
    throw new Error("unsupported or incomplete IssueStore artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "storage"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`);
  }
}

const failures = [];
for (const field of ["hot_rps", "distributed_rps"]) {
  const change = ((candidate.metrics[field] - baseline.metrics[field]) / baseline.metrics[field]) * 100;
  console.log(`${field}: ${baseline.metrics[field]} -> ${candidate.metrics[field]} (${change.toFixed(2)}%)`);
  if (change < -budget) failures.push(`${field} regressed by ${(-change).toFixed(2)}%`);
}
if (candidate.metrics.hot_rps < candidate.metrics.minimum_hot_rps) failures.push("hot RPS is below gate");
if (candidate.metrics.distributed_rps < candidate.metrics.minimum_distributed_rps) {
  failures.push("distributed RPS is below gate");
}
if (candidate.metrics.operations_per_profile !== baseline.metrics.operations_per_profile) {
  failures.push("operation count changed");
}
if (candidate.metrics.concurrency !== baseline.metrics.concurrency) failures.push("concurrency changed");
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
