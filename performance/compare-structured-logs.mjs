import { readFileSync } from "node:fs";

if (process.argv.length < 4) {
  throw new Error(
    "usage: node performance/compare-structured-logs.mjs <baseline.json> <candidate.json> [max-regression-percent]",
  );
}

const baseline = JSON.parse(readFileSync(process.argv[2], "utf8"));
const candidate = JSON.parse(readFileSync(process.argv[3], "utf8"));
const budget = Number(process.argv[4] ?? "20");
for (const field of [
  "scenario",
  "fixture_revision",
  "log_target_rps",
  "error_target_rps",
  "duration",
]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`structured Log artifacts differ at metadata.${field}`);
  }
}

const failures = [];
for (const signal of ["log", "error"]) {
  const rpsField = `${signal}_achieved_rps`;
  const change =
    ((candidate.metrics[rpsField] - baseline.metrics[rpsField]) /
      baseline.metrics[rpsField]) *
    100;
  console.log(
    `${signal} RPS: ${baseline.metrics[rpsField].toFixed(2)} -> ${candidate.metrics[rpsField].toFixed(2)} (${change.toFixed(2)}%)`,
  );
  if (change < -budget) {
    failures.push(`${signal} RPS regressed by ${(-change).toFixed(2)}%`);
  }
  for (const percentile of ["p95", "p99"]) {
    const before = baseline.metrics[`${signal}_latency_ms`][percentile];
    const after = candidate.metrics[`${signal}_latency_ms`][percentile];
    const latencyChange = before === 0 ? 0 : ((after - before) / before) * 100;
    console.log(
      `${signal} ${percentile}: ${before.toFixed(2)} -> ${after.toFixed(2)} ms (${latencyChange.toFixed(2)}%)`,
    );
    if (latencyChange > budget) {
      failures.push(`${signal} ${percentile} regressed by ${latencyChange.toFixed(2)}%`);
    }
  }
}
if (candidate.metrics.dropped_iterations !== 0) {
  failures.push("dropped iterations are non-zero");
}
for (const field of [
  "tcp_errors",
  "status_429",
  "status_503",
  "status_other",
]) {
  if (candidate.metrics.failures[field] !== 0) {
    failures.push(`${field} is non-zero`);
  }
}
if (candidate.metrics.log_requests !== candidate.metrics.log_status_200) {
  failures.push("not every Log request returned HTTP 200");
}
if (candidate.metrics.error_requests !== candidate.metrics.error_status_200) {
  failures.push("not every Error request returned HTTP 200");
}
if (failures.length > 0) {
  throw new Error(failures.join("; "));
}
console.log(`PASS: no regression beyond ${budget}% and durability counters passed`);
