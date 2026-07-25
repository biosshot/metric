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
  "test", "--locked", "--release", "-p", "metric-application",
  "auth::tests::performance_login_rate_limit_rps", "--", "--ignored", "--exact", "--nocapture",
]);
const metrics = output.match(
  /Auth Phase 11: rate_limit_rps=(\d+),attempts=(\d+),rejected=(\d+),capacity=(\d+),elapsed_ms=(\d+)/,
);
if (!metrics) throw new Error("benchmark output did not match the Phase 11 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "auth-login-rate-limit-phase-11",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/metric", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "500000 saturated rejected login attempts using one account digest and one network digest",
    configuration: "max_attempts=5; window=60s; capacity=10000",
  },
  metrics: {
    rate_limit_rps: Number(metrics[1]),
    attempts: Number(metrics[2]),
    rejected: Number(metrics[3]),
    capacity: Number(metrics[4]),
    elapsed_ms: Number(metrics[5]),
    minimum_rps: 100000,
  },
};

const path = process.argv[2] ??
  `performance/results/auth-rate-limit-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
