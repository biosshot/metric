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

const output = run("cargo", [
  "test", "--locked", "--release", "-p", "faultkeep-application",
  "incident_capsule::tests::performance_incident_capsule_streaming_rps",
  "--", "--ignored", "--exact", "--nocapture",
]);
const match = output.match(
  /Phase19 Incident Capsule: samples=(\d+),capsule_rps=(\d+),mib_per_second=([\d.]+),fixture_events=(\d+)/,
);
if (!match) throw new Error("benchmark output did not match the Phase 19 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "incident-capsule-phase-19",
    source_commit: run("git", ["rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: `${match[4]} Event DTOs with 8 KiB payload each; Issue, statistics, activity, capabilities, README and final manifest; bounded 64 KiB chunks`,
    storage: "in-memory DTO fixture and streaming ZIP64 writer; no MongoDB or BlobStore in timing window; release profile",
  },
  metrics: {
    samples: Number(match[1]),
    capsule_rps: Number(match[2]),
    mib_per_second: Number(match[3]),
    minimum_capsule_rps: 20,
  },
};

const path = process.argv[2] ??
  `performance/results/incident-capsule-${new Date().toISOString().replaceAll(":", "-")}.json`;
const outputUrl = new URL(`../${path}`, import.meta.url);
mkdirSync(dirname(fileURLToPath(outputUrl)), { recursive: true });
writeFileSync(outputUrl, `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
