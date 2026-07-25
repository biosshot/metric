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
  "performance_symbolication_baseline_rps",
  "--",
  "--ignored",
  "--nocapture",
]);
const metrics = output.match(/Symbolication Phase 6 baseline: rps=(\d+),events=(\d+)/);
if (!metrics) throw new Error("benchmark output did not match the Phase 6 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "symbolication-disabled-baseline-phase-6",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/metric", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "round-robin Python no-work, native address, JavaScript frame",
    backend: "disabled production baseline; no network",
  },
  metrics: {
    rps: Number(metrics[1]),
    events: Number(metrics[2]),
    minimum_rps: 20000,
  },
};

const path = process.argv[2] ??
  `performance/results/symbolication-baseline-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
