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
  "performance_application_metric_streaming_fold_rps",
  "--",
  "--ignored",
  "--nocapture",
]);
const result = output.match(
  /Application Metrics Phase 37: containers=(\d+),measurements=(\d+),container_rps=(\d+),measurement_rps=(\d+),deltas_per_container=(\d+)/,
);
if (!result) throw new Error("benchmark output did not match the Phase 37 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "application-metrics-streaming-fold-phase-37",
    source_commit: run("git", ["rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "500 pinned trace_metric containers; 1000 same-series counters each",
    scope: "request-local JSON streaming fold only; excludes HTTP and MongoDB",
  },
  metrics: {
    containers: Number(result[1]),
    measurements: Number(result[2]),
    container_rps: Number(result[3]),
    measurement_rps: Number(result[4]),
    deltas_per_container: Number(result[5]),
    minimum_measurement_rps: 100_000,
  },
};

const path =
  process.argv[2] ??
  `performance/results/application-metrics-${new Date().toISOString().replaceAll(":", "-")}.json`;
const destination = new URL(`../${path}`, import.meta.url);
mkdirSync(new URL(".", destination), { recursive: true });
writeFileSync(destination, `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
