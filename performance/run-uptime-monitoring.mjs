import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { cpus, platform, totalmem } from "node:os";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: new URL("..", import.meta.url),
    encoding: "utf8",
    env: { ...process.env, ...options.env },
    timeout: options.timeout ?? 120_000,
    killSignal: "SIGKILL",
    stdio: ["ignore", "pipe", "pipe"],
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    process.stderr.write(result.stdout ?? "");
    process.stderr.write(result.stderr ?? "");
    throw new Error(`${command} exited with status ${result.status}`);
  }
  return `${result.stdout ?? ""}${result.stderr ?? ""}`;
}

const mongodbUri =
  process.env.METRIC_TEST_MONGODB_URI ??
  "mongodb://127.0.0.1:27017/?directConnection=true";
const output = run(
  "cargo",
  [
    "test",
    "--locked",
    "-p",
    "metric-mongo",
    "--test",
    "monitors",
    "durable_uptime_run_writer_reports_rps",
    "--",
    "--ignored",
    "--exact",
    "--nocapture",
  ],
  { env: { METRIC_TEST_MONGODB_URI: mongodbUri } },
);
const match = output.match(
  /\{"runs":(\d+),"batch":(\d+),"elapsed_ms":(\d+),"rps":([\d.]+)\}/,
);
if (!match) throw new Error("benchmark output did not match the Phase 36 schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "uptime-monitor-run-durable-writer-phase-36",
    source_commit: run("git", ["rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture:
      "2000 distinct terminal Uptime runs; compact BSON; status and latency; batches of 200",
    storage: "local MongoDB direct connection; debug profile; unique database dropped by test",
  },
  metrics: {
    runs: Number(match[1]),
    batch: Number(match[2]),
    elapsed_ms: Number(match[3]),
    durable_rps: Number(match[4]),
    minimum_durable_rps: 100,
  },
};
const path =
  process.argv[2] ??
  `performance/results/uptime-monitoring-${new Date().toISOString().replaceAll(":", "-")}.json`;
const outputUrl = new URL(`../${path}`, import.meta.url);
mkdirSync(dirname(fileURLToPath(outputUrl)), { recursive: true });
writeFileSync(outputUrl, `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
