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
  "performance_replay_validation_rps",
  "--",
  "--ignored",
  "--nocapture",
]);
const result = output.match(/replay validation: (\d+) requests\/s/);
if (!result) throw new Error("benchmark output did not match the Phase 38 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "session-replay-validation-phase-38",
    source_commit: run("git", ["rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "20000 raw rrweb segments; 4 events per segment",
    scope: "request-local Replay recording validation only; excludes HTTP, BlobStore and MongoDB",
  },
  metrics: {
    iterations: 20_000,
    events_per_segment: 4,
    validation_rps: Number(result[1]),
    minimum_validation_rps: 10_000,
  },
};

const path =
  process.argv[2] ??
  `performance/results/session-replay-${new Date().toISOString().replaceAll(":", "-")}.json`;
const destination = new URL(`../${path}`, import.meta.url);
mkdirSync(new URL(".", destination), { recursive: true });
writeFileSync(destination, `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
