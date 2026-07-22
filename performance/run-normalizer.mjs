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
  "faultkeep-application",
  "performance_normalizer_adr0037_corpus_rps",
  "--",
  "--ignored",
  "--nocapture",
]);
const metrics = output.match(
  /Normalizer Phase 5: rps_1k=(\d+),rps_4k=(\d+),rps_16k=(\d+),rps_128k=(\d+),weighted_rps=(\d+),out_1k=(\d+),out_4k=(\d+),out_16k=(\d+),out_128k=(\d+)/,
);
if (!metrics) throw new Error("benchmark output did not match the Phase 5 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "normalizer-adr0037-phase-5",
    source_commit: run("git", ["-c", "safe.directory=D:/MyProject/rust/faultkeep", "rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "scrubbed synthetic Rust Error Event at 1/4/16/128 KiB input classes",
    weights: "60/30/9/1",
    normalizer_limits: "phase-5-default-v1",
    allocation_proxy: "canonical output bytes; excludes allocator bookkeeping",
  },
  metrics: {
    rps: {
      size_1k: Number(metrics[1]),
      size_4k: Number(metrics[2]),
      size_16k: Number(metrics[3]),
      size_128k: Number(metrics[4]),
      weighted: Number(metrics[5]),
    },
    canonical_output_bytes: {
      size_1k: Number(metrics[6]),
      size_4k: Number(metrics[7]),
      size_16k: Number(metrics[8]),
      size_128k: Number(metrics[9]),
    },
    minimum_weighted_rps: 7500,
  },
};

const path = process.argv[2] ??
  `performance/results/normalizer-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
