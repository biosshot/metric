import fs from "node:fs";

const [baselinePath, candidatePath] = process.argv.slice(2);
if (!baselinePath || !candidatePath) {
  throw new Error(
    "usage: node performance/compare-debug-files.mjs <baseline.json> <candidate.json>",
  );
}

const baseline = JSON.parse(fs.readFileSync(baselinePath, "utf8"));
const candidate = JSON.parse(fs.readFileSync(candidatePath, "utf8"));
const maximumRegression = 0.2;

for (const metric of [
  "private_index_hit_rps",
  "private_index_miss_rps",
  "backend_failure_circuit_rps",
]) {
  const before = baseline.result[metric];
  const after = candidate.result[metric];
  if (!(before > 0) || !(after > 0)) {
    throw new Error(`${metric} must be a positive number`);
  }
  const change = (after - before) / before;
  console.log(`${metric}: ${(change * 100).toFixed(1)}%`);
  if (change < -maximumRegression) {
    throw new Error(`${metric} regressed by more than 20%`);
  }
}
