import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-auth-rate-limit.mjs <baseline.json> <candidate.json> [max-regression-percent]");
  process.exit(2);
}

const [, , baselinePath, candidatePath, budgetText = "15"] = process.argv;
const budget = Number(budgetText);
if (!Number.isFinite(budget) || budget < 0) process.exit(2);
const baseline = JSON.parse(readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(readFileSync(candidatePath, "utf8"));
for (const artifact of [baseline, candidate]) {
  if (artifact.schema_version !== 1 ||
      artifact.metadata?.scenario !== "auth-login-rate-limit-phase-11" ||
      !artifact.metrics) {
    throw new Error("unsupported or incomplete auth rate-limit artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "configuration"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}: ${baseline.metadata[field]} != ${candidate.metadata[field]}`);
  }
}
for (const field of ["attempts", "rejected", "capacity"]) {
  if (baseline.metrics[field] !== candidate.metrics[field]) {
    throw new Error(`incomparable metric ${field}`);
  }
}

const change = ((candidate.metrics.rate_limit_rps - baseline.metrics.rate_limit_rps) /
  baseline.metrics.rate_limit_rps) * 100;
console.log(
  `rate_limit_rps: ${baseline.metrics.rate_limit_rps} -> ${candidate.metrics.rate_limit_rps} (${change.toFixed(2)}%)`,
);
const failures = [];
if (change < -budget) failures.push(`rate-limit RPS regressed by ${(-change).toFixed(2)}%`);
if (candidate.metrics.rate_limit_rps < candidate.metrics.minimum_rps) {
  failures.push(`rate-limit RPS is below ${candidate.metrics.minimum_rps}`);
}
if (candidate.metrics.rejected !== candidate.metrics.attempts) {
  failures.push("saturated attempts were not all rejected");
}
if (failures.length > 0) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%, minimum RPS and rejection gates passed`);
