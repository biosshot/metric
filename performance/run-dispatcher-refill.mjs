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
  "metric-mongo",
  "--test",
  "event_store",
  "performance_dispatcher_mongodb_refill_rps",
  "--",
  "--ignored",
  "--nocapture",
]);
const metrics = output.match(
  /Dispatcher MongoDB refill: (\d+) events\/s, events=(\d+), elapsed_ms=(\d+)/,
);
if (!metrics) throw new Error("benchmark output did not match the Phase 4 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "dispatcher-mongodb-refill-phase-4",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/metric", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    mongo_server: "8.0.12 standalone in Docker Desktop",
    mongo_driver: "3.8.0",
    fixture: "20000 compact pending Rust Error Events",
    query_order: "q.n,r,_id",
  },
  metrics: {
    refill_rps: Number(metrics[1]),
    events: Number(metrics[2]),
    elapsed_ms: Number(metrics[3]),
    minimum_recovery_rps: 7500,
  },
};

const path = process.argv[2] ??
  `performance/results/dispatcher-refill-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
