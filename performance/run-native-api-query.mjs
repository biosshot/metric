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
  "test", "--locked", "--release", "-p", "metric-server", "--test", "native_api_e2e",
  "performance_native_event_query_rps_p95_p99", "--", "--ignored", "--exact", "--nocapture",
]);
const metrics = output.match(
  /Phase12 Native API query: dataset_events=(\d+),queries=(\d+),page=(\d+),rps=(\d+),p95_ms=([\d.]+),p99_ms=([\d.]+),elapsed_ms=(\d+)/,
);
if (!metrics) throw new Error("benchmark output did not match the Phase 12 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "native-api-query-phase-12",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/metric", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "2,000 finalized Events; newest-first project timeline; page size 50; 1,000 sequential queries",
    storage: "MongoDB 8.0.12 local Docker tmpfs; standalone",
  },
  metrics: {
    dataset_events: Number(metrics[1]),
    queries: Number(metrics[2]),
    page_size: Number(metrics[3]),
    rps: Number(metrics[4]),
    p95_ms: Number(metrics[5]),
    p99_ms: Number(metrics[6]),
    elapsed_ms: Number(metrics[7]),
    minimum_rps: 100,
    maximum_p95_ms: 100,
    maximum_p99_ms: 250,
  },
};

const path = process.argv[2] ??
  `performance/results/native-api-query-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
