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
    timeout: options.timeout ?? 300_000,
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
    "--release",
    "-p",
    "metric-server",
    "--test",
    "notifications_e2e",
    "performance_notification_transition_expansion_rps",
    "--",
    "--ignored",
    "--exact",
    "--nocapture",
  ],
  { env: { METRIC_TEST_MONGODB_URI: mongodbUri } },
);
const match = output.match(
  /Phase20 Notification: transitions=(\d+),expansion_rps=([\d.]+),elapsed_ms=(\d+)/,
);
if (!match) throw new Error("benchmark output did not match the Phase 20 result schema");

let mongoVersion = process.env.METRIC_MONGODB_VERSION ?? "8.0.12";
try {
  mongoVersion = run("mongosh", [
    mongodbUri,
    "--quiet",
    "--eval",
    "db.version()",
  ]).trim();
} catch {
  // The required version remains explicit when mongosh is not on PATH.
}

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "notification-transition-expansion-phase-20",
    source_commit: run("git", ["rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    mongodb_version: mongoVersion,
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture:
      "300 distinct Issue-owned new_issue transitions; one enabled rule and one destination; batches of 100",
    storage:
      "local MongoDB direct connection; journaled Issue intents and idempotent delivery upserts; release profile",
  },
  metrics: {
    transitions: Number(match[1]),
    expansion_rps: Number(match[2]),
    elapsed_ms: Number(match[3]),
    minimum_expansion_rps: 50,
  },
};

const path =
  process.argv[2] ??
  `performance/results/notifications-${new Date().toISOString().replaceAll(":", "-")}.json`;
const outputUrl = new URL(`../${path}`, import.meta.url);
mkdirSync(dirname(fileURLToPath(outputUrl)), { recursive: true });
writeFileSync(outputUrl, `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
