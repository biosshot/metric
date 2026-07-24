import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { cpus, platform, totalmem } from "node:os";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: new URL("..", import.meta.url),
    encoding: "utf8",
    env: process.env,
    timeout: options.timeout ?? 600_000,
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
  "archive::tests::performance_archive_writer_rps_mib_with_foreground_work",
  "--lib", "--", "--ignored", "--exact", "--nocapture",
]);
const match = output.match(
  /\{"segments":24,"events":12000,[^\r\n]+\}/,
);
if (!match) throw new Error("benchmark output did not match the Phase 21 result schema");
const measured = JSON.parse(match[0]);

const artifact = {
  schema_version: 1,
  metadata: {
    scenario: "cold-event-archive-phase-21",
    source_commit: run("git", ["rev-parse", "HEAD"]).trim(),
    generated_at: new Date().toISOString(),
    rust_toolchain: run("rustc", ["--version"]).trim(),
    hardware: `${cpus()[0]?.model ?? "unknown CPU"}; ${(totalmem() / 2 ** 30).toFixed(1)} GiB RAM; ${platform()}`,
    fixture: "24 project/day Parquet segments; 500 Events per segment; 2048-byte scrubbed JSON Event payload; concurrent BLAKE3 foreground worker",
    storage: "in-memory segment source and sink; Parquet/Zstd level 3; release profile; no MongoDB or S3 latency in timing window",
  },
  metrics: {
    ...measured,
    minimum_archive_events_rps: 25000,
    maximum_peak_input_bytes: 64 * 1024 * 1024,
  },
};

const path = process.argv[2] ??
  `performance/results/archive-${new Date().toISOString().replaceAll(":", "-")}.json`;
const outputUrl = new URL(`../${path}`, import.meta.url);
mkdirSync(dirname(fileURLToPath(outputUrl)), { recursive: true });
writeFileSync(outputUrl, `${JSON.stringify(artifact, null, 2)}\n`);
console.log(path);
