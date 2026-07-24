import { chromium } from "@playwright/test";
import process, { argv, stderr, stdout } from "node:process";
import { URL } from "node:url";

const [pageUrl, dsn] = argv.slice(2);
if (!pageUrl || !dsn) {
  throw new Error("Faultkeep page URL and DSN arguments are required");
}

let browser;
const deadline = new Promise((_, reject) => {
  const timer = setTimeout(() => {
    reject(
      new Error("real Browser SDK harness exceeded its 20 second deadline"),
    );
  }, 20_000);
  timer.unref();
});

async function run() {
  browser = await chromium.launch({ headless: true });
  const page = await browser.newPage();
  const browserErrors = [];
  page.on("pageerror", (error) => browserErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      browserErrors.push(message.text());
    }
  });

  const url = new URL(pageUrl);
  url.searchParams.set("dsn", dsn);
  await page.goto(url.toString(), { waitUntil: "load", timeout: 10_000 });
  await page.waitForFunction(
    () => window.__faultkeepSdkResult?.complete === true,
    null,
    {
      timeout: 12_000,
    },
  );
  const result = await page.evaluate(() => window.__faultkeepSdkResult);
  if (result.error) {
    throw new Error(result.error);
  }
  if (!result.flushed || !result.event_id) {
    throw new Error("real Browser SDK returned an incomplete result");
  }
  if (browserErrors.length > 0) {
    throw new Error(`browser errors: ${browserErrors.join("; ")}`);
  }
  stdout.write(`${JSON.stringify(result)}\n`);
}

try {
  await Promise.race([run(), deadline]);
} catch (error) {
  stderr.write(`${error instanceof Error ? error.stack : String(error)}\n`);
  process.exitCode = 2;
} finally {
  await browser?.close();
}
