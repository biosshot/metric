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
  "metric-application",
  "performance_span_writer_rps_and_batch_occupancy",
  "--",
  "--ignored",
  "--nocapture",
]);
const metrics = output.match(
  /SpanWriter Phase 25: rps=(\d+),records=(\d+),batches=(\d+),average_occupancy=([\d.]+),concurrency=(\d+),elapsed_ms=(\d+)/,
);
if (!metrics) {
  throw new Error("benchmark output did not match the Phase 25 Span writer schema");
}
const commit = run("git", ["rev-parse", "HEAD"]).trim();
const dirty =
  spawnSync("git", ["diff", "--quiet"], {
    cwd: new URL("..", import.meta.url),
    stdio: "ignore",
  }).status !== 0;

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "phase-25-span-writer",
    source_commit: `${commit}${dirty ? "-dirty" : ""}`,
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
  },
  metrics: {
    rps: Number(metrics[1]),
    records: Number(metrics[2]),
    batches: Number(metrics[3]),
    average_batch_occupancy: Number(metrics[4]),
    concurrency: Number(metrics[5]),
    elapsed_ms: Number(metrics[6]),
    minimum_rps: 20_000,
    minimum_average_batch_occupancy: 100,
    acknowledged_loss: 0,
  },
};

const path =
  process.argv[2] ??
  `performance/results/span-writer-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
