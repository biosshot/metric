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
  "test", "--locked", "--release", "-p", "metric-mongo", "--test", "finalizer_store",
  "performance_finalize_batch_rps", "--", "--ignored", "--exact", "--nocapture",
]);
const metrics = output.match(
  /Finalizer Phase 9: rps=(\d+),events=(\d+),issues=(\d+),elapsed_ms=(\d+)/,
);
if (!metrics) throw new Error("benchmark output did not match the Phase 9 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "finalizer-phase-9",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/metric", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "one bounded FinalizeBatch; 1000 Events across 100 Issues; one Release and Environment",
    storage: "MongoDB 8.0.12 local Docker tmpfs; retryWrites=false",
  },
  metrics: {
    rps: Number(metrics[1]),
    events: Number(metrics[2]),
    issues: Number(metrics[3]),
    elapsed_ms: Number(metrics[4]),
    minimum_rps: 150,
  },
};

const path = process.argv[2] ??
  `performance/results/finalizer-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
