import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";

const require = createRequire(import.meta.url);
const expected = [
  ["current", "@sentry/cli/package.json", "bin/sentry-cli", "3.6.2"],
  ["retained", "sentry-cli-v2/package.json", "bin/sentry-cli", "2.58.6"],
  // getsentry/cli, which @sentry/bundler-plugins 11 uploads with.
  ["bundler plugins", "sentry", "bin.cjs", "0.44.1"],
];

for (const [name, entry, bin, version] of expected) {
  const resolved = require.resolve(entry);
  const binary = path.join(path.dirname(resolved), bin);
  const output = execFileSync(process.execPath, [binary, "--version"], {
    encoding: "utf8",
  }).trim();
  if (!output.includes(version)) {
    throw new Error(`${name} sentry-cli expected ${version}, got ${output}`);
  }
  console.log(`${name}: ${output}`);
}
