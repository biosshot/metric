import { readFileSync } from "node:fs";

if (process.argv.length < 4 || process.argv.length > 5) {
  console.error("usage: node performance/compare-archive.mjs <baseline.json> <candidate.json> [max-regression-percent]");
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
    artifact.metadata?.scenario !== "cold-event-archive-phase-21" ||
    !artifact.metrics
  ) {
    throw new Error("unsupported or incomplete Phase 21 benchmark artifact");
  }
}
for (const field of ["hardware", "rust_toolchain", "fixture", "storage"]) {
  if (baseline.metadata[field] !== candidate.metadata[field]) {
    throw new Error(`incomparable metadata ${field}`);
  }
}
for (const field of ["segments", "events", "payload_bytes"]) {
  if (baseline.metrics[field] !== candidate.metrics[field]) {
    throw new Error(`incomparable metric ${field}`);
  }
}

const failures = [];
for (const metric of [
  "archive_events_rps",
  "archive_input_mib_per_second",
  "foreground_ops_rps",
]) {
  const before = baseline.metrics[metric];
  const after = candidate.metrics[metric];
  const change = ((after - before) / before) * 100;
  console.log(`${metric}: ${before} -> ${after} (${change.toFixed(2)}%)`);
  if (!(after > 0) || change < -budget) {
    failures.push(`${metric} regressed by ${(-change).toFixed(2)}%`);
  }
}
if (
  candidate.metrics.archive_events_rps <
  candidate.metrics.minimum_archive_events_rps
) {
  failures.push("archive_events_rps is below gate");
}
if (
  candidate.metrics.peak_input_bytes >
  candidate.metrics.maximum_peak_input_bytes
) {
  failures.push("peak_input_bytes exceeds the bounded-memory gate");
}
if (failures.length) {
  console.error(`performance regression: ${failures.join("; ")}`);
  process.exit(1);
}
console.log(`PASS: no regression beyond ${budget}%`);
