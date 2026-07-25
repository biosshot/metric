import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { cpus, platform, totalmem } from "node:os";

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: new URL("..", import.meta.url),
    encoding: "utf8",
    env: process.env,
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.status !== 0) {
    process.stderr.write(result.stdout ?? "");
    process.stderr.write(result.stderr ?? "");
    throw new Error(`${command} exited with status ${result.status}`);
  }
  return `${result.stdout ?? ""}${result.stderr ?? ""}`;
}

const output = run("cargo", [
  "test", "--locked", "--release", "-p", "metric-mongo", "--test", "issue_store",
  "performance_issue_upsert_hot_and_distributed_rps", "--", "--ignored", "--nocapture",
]);
const metrics = output.match(
  /IssueStore Phase 8: hot_rps=(\d+),distributed_rps=(\d+),operations=(\d+),concurrency=(\d+)/,
);
if (!metrics) throw new Error("benchmark output did not match the Phase 8 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "issue-store-phase-8",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/metric", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "64-way atomic aggregation-pipeline upsert; hot one-Issue and 250-Issue distribution",
    storage: "MongoDB 8.0.12 local Docker tmpfs; retryWrites=false",
  },
  metrics: {
    hot_rps: Number(metrics[1]),
    distributed_rps: Number(metrics[2]),
    operations_per_profile: Number(metrics[3]),
    concurrency: Number(metrics[4]),
    minimum_hot_rps: 250,
    minimum_distributed_rps: 500,
  },
};

const path = process.argv[2] ??
  `performance/results/issue-store-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
