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
  "test",
  "--locked",
  "--release",
  "-p",
  "faultkeep-server",
  "--test",
  "durable_ingest_e2e",
  "performance_mongo_writer_rps_latency_and_occupancy",
  "--",
  "--ignored",
  "--nocapture",
]);
const metrics = output.match(
  /MongoWriter: (\d+) events\/s, batches=(\d+), avg occupancy=([\d.]+), p95=(\d+) ms, p99=(\d+) ms/,
);
if (!metrics) {
  throw new Error("benchmark output did not match the Phase 3 result schema");
}

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "mongo-writer-phase-3",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/faultkeep", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    mongo_server: "8.0.12 standalone in Docker Desktop",
    mongo_driver: "3.8.0",
    unique_events: 20000,
    duplicate_retries: 100,
    concurrency: 512,
  },
  metrics: {
    rps: Number(metrics[1]),
    batches: Number(metrics[2]),
    average_batch_occupancy: Number(metrics[3]),
    p95_ms: Number(metrics[4]),
    p99_ms: Number(metrics[5]),
    minimum_gate_rps: 5000,
    maximum_gate_p95_ms: 100,
    maximum_gate_p99_ms: 250,
    acknowledged_loss: 0,
  },
};

const path = process.argv[2] ??
  `performance/results/mongo-writer-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
