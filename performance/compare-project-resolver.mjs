import { readFileSync } from "node:fs";

function usage() {
  console.error(
    "usage: node performance/compare-project-resolver.mjs <baseline.json> <candidate.json> [max-regression-percent]",
  );
  process.exit(2);
}

if (process.argv.length < 4 || process.argv.length > 5) usage();
const [, , baselinePath, candidatePath, budgetText = "10"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) usage();

const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (
    artifact.schema_version !== 1 ||
    artifact.metadata?.scenario !== "project-resolver-phase-2" ||
    !artifact.metrics?.warm_cache ||
    !artifact.metrics?.direct_mongodb
  ) {
    throw new Error("unsupported or incomplete project-resolver artifact");
  }
}

for (const field of ["mongo_server", "mongo_driver"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(
      `incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`,
    );
  }
}

function percentChange(before, after) {
  return before === 0
    ? after === 0
      ? 0
      : Infinity
    : ((after - before) / before) * 100;
}

const checks = [
  ["warm cache RPS", baseline.metrics.warm_cache.rps, candidate.metrics.warm_cache.rps, "lower"],
  ["warm cache average", baseline.metrics.warm_cache.average_ns, candidate.metrics.warm_cache.average_ns, "higher"],
  ["direct MongoDB RPS", baseline.metrics.direct_mongodb.rps, candidate.metrics.direct_mongodb.rps, "lower"],
  ["direct MongoDB p95", baseline.metrics.direct_mongodb.p95_us, candidate.metrics.direct_mongodb.p95_us, "higher"],
];
const failures = [];
for (const [name, before, after, regression] of checks) {
  const change = percentChange(before, after);
  console.log(`${name}: ${before} -> ${after} (${change.toFixed(2)}%)`);
  if (regression === "lower" && change < -budget) {
    failures.push(`${name} regressed by ${(-change).toFixed(2)}%`);
  }
  if (regression === "higher" && change > budget) {
    failures.push(`${name} regressed by ${change.toFixed(2)}%`);
  }
}
if (candidate.metrics.warm_cache.rps < candidate.metrics.warm_cache.minimum_gate_rps) {
  failures.push("warm cache RPS is below the ADR-0037 gate");
}
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
