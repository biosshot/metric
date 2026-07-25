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

const cacheOutput = run("cargo", [
  "test",
  "--locked",
  "--release",
  "-p",
  "metric-application",
  "performance_project_cache_hit_rps",
  "--",
  "--ignored",
  "--nocapture",
]);
const mongoOutput = run("cargo", [
  "test",
  "--locked",
  "--release",
  "-p",
  "metric-mongo",
  "--test",
  "project_identity",
  "performance_project_identity_cold_mongodb_lookup",
  "--",
  "--ignored",
  "--nocapture",
]);

const cache = cacheOutput.match(
  /project cache hit: (\d+) lookups\/s, (\d+) ns average/,
);
const mongo = mongoOutput.match(
  /MongoDB direct lookup: (\d+) lookups\/s, (\d+) us average, p50=(\d+) us, p95=(\d+) us, p99=(\d+) us/,
);
if (!cache || !mongo) {
  throw new Error("benchmark output did not match the Phase 2 result schema");
}

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "project-resolver-phase-2",
    source_commit: run("git", ["rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    mongo_server: "8.0.12 standalone in Docker Desktop",
    mongo_driver: "3.8.0",
    warm_cache_iterations: 200000,
    direct_mongo_iterations: 1000,
  },
  metrics: {
    warm_cache: {
      rps: Number(cache[1]),
      average_ns: Number(cache[2]),
      minimum_gate_rps: 20000,
    },
    direct_mongodb: {
      rps: Number(mongo[1]),
      average_us: Number(mongo[2]),
      p50_us: Number(mongo[3]),
      p95_us: Number(mongo[4]),
      p99_us: Number(mongo[5]),
    },
  },
};

const output = process.argv[2] ??
  `performance/results/project-resolver-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${output}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(output);
