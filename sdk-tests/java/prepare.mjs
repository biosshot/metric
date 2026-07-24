import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const VERSION = "8.50.1";
const SHA256 = "0bac4f4330e94ca19eb91640618d22a89b47ce45ffb79aa3d55e41f9b4f33517";
const root = dirname(fileURLToPath(import.meta.url));
const dependencyDirectory = join(root, ".deps");
const jar = join(dependencyDirectory, `sentry-${VERSION}.jar`);
const classes = join(dependencyDirectory, "classes");
const source = join(root, "FaultkeepSdkCompatibility.java");

await mkdir(classes, { recursive: true });
let bytes;
try {
  bytes = await readFile(jar);
} catch {
  const response = await fetch(
    `https://repo.maven.apache.org/maven2/io/sentry/sentry/${VERSION}/sentry-${VERSION}.jar`,
  );
  if (!response.ok) {
    throw new Error(`Sentry Java download failed with HTTP ${response.status}`);
  }
  bytes = Buffer.from(await response.arrayBuffer());
  await writeFile(jar, bytes);
}
const digest = createHash("sha256").update(bytes).digest("hex");
if (digest !== SHA256) {
  throw new Error(`Sentry Java checksum mismatch: ${digest}`);
}
execFileSync("javac", ["-cp", jar, "-d", classes, source], { stdio: "inherit" });
console.log(`prepared sentry-java ${VERSION} (${digest})`);
