import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";

const require = createRequire(import.meta.url);
const expected = [
  ["current", "@sentry/cli/package.json", "3.6.2"],
  ["retained", "sentry-cli-v2/package.json", "2.58.6"],
];

for (const [name, manifest, version] of expected) {
  const resolved = require.resolve(manifest);
  const binary = path.join(path.dirname(resolved), "bin", "sentry-cli");
  const output = execFileSync(process.execPath, [binary, "--version"], {
    encoding: "utf8",
  }).trim();
  if (!output.includes(version)) {
    throw new Error(`${name} sentry-cli expected ${version}, got ${output}`);
  }
  console.log(`${name}: ${output}`);
}
