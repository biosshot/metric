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
  "test", "--locked", "--release", "-p", "faultkeep-server", "--test", "durable_ingest_e2e",
  "performance_processor_recovery_rps", "--", "--ignored", "--exact", "--nocapture",
]);
const metrics = output.match(
  /Processor Phase 10: recovery_rps=(\d+),events=(\d+),concurrency=(\d+),accepted_steady_rps=(\d+),recovery_ratio=([0-9.]+),elapsed_ms=(\d+)/,
);
if (!metrics) throw new Error("benchmark output did not match the Phase 10 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "processor-recovery-phase-10",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/faultkeep", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "1000 pending Error Events in one hot Issue; full baseline Processor stages; bounded Finalizer batches",
    storage: "MongoDB 8.0.12 local Docker tmpfs; retryWrites=false; MongoDB 8 bulk_write",
  },
  metrics: {
    recovery_rps: Number(metrics[1]),
    events: Number(metrics[2]),
    concurrency: Number(metrics[3]),
    accepted_steady_rps: Number(metrics[4]),
    recovery_ratio: Number(metrics[5]),
    elapsed_ms: Number(metrics[6]),
    minimum_recovery_ratio: 1.5,
  },
};

const path = process.argv[2] ??
  `performance/results/processor-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
