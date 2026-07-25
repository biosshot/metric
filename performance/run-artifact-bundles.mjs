import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { cpus, platform, totalmem } from "node:os";

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: new URL("..", import.meta.url),
    encoding: "utf8",
    env: { ...process.env, ...options.env },
    timeout: options.timeout ?? 180_000,
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

const output = run(
  "cargo",
  [
    "test", "--locked", "-p", "metric-server", "--test", "debug_files_e2e",
    "real_pinned_sentry_cli_upload_private_isolation_and_exact_delete", "--", "--ignored",
    "--exact", "--nocapture",
  ],
  { env: { METRIC_PHASE18_PERF: "1" } },
);
const match = output.match(
  /Phase18 Artifact lookup: samples=(\d+),modern_hit_rps=(\d+),legacy_hit_rps=(\d+),miss_rps=(\d+),open_circuit_rps=(\d+)/,
);
if (!match) throw new Error("benchmark output did not match the Phase 18 result schema");

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "javascript-artifact-bundles-phase-18",
    source_commit: run("git", ["rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "real sentry-cli 3.6.2 and 2.58.6; one ready Debug-ID bundle; release/dist binding; 300 sequential lookups per outcome",
    storage: "local MongoDB standalone and local filesystem BlobStore; Windows development machine; debug profile",
  },
  metrics: {
    samples_per_lookup_outcome: Number(match[1]),
    modern_hit_rps: Number(match[2]),
    legacy_hit_rps: Number(match[3]),
    miss_rps: Number(match[4]),
    open_circuit_rps: Number(match[5]),
    minimum_lookup_rps: 50,
    minimum_open_circuit_rps: 100_000,
  },
};

const path = process.argv[2] ??
  `performance/results/artifact-bundles-${new Date().toISOString().replaceAll(":", "-")}.json`;
mkdirSync(new URL("results/", import.meta.url), { recursive: true });
writeFileSync(new URL(`../${path}`, import.meta.url), `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
