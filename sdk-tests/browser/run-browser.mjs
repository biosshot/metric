import { chromium } from "@playwright/test";
import process, { argv, stderr, stdout } from "node:process";
import { URL } from "node:url";

const [pageUrl, dsn] = argv.slice(2);
if (!pageUrl || !dsn) {
  throw new Error("Metric page URL and DSN arguments are required");
}

let browser;
let page;
const diagnostics = [];
const browserErrors = [];
const deadline = new Promise((_, reject) => {
  const timer = setTimeout(() => {
    reject(
      new Error("real Browser SDK harness exceeded its 25 second deadline"),
    );
  }, 25_000);
  timer.unref();
});

async function run() {
  browser = await chromium.launch({ headless: true });
  page = await browser.newPage();
  page.on("pageerror", (error) => browserErrors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") {
      browserErrors.push(message.text());
    }
  });
  page.on("requestfailed", (request) => {
    diagnostics.push(
      `${request.method()} ${request.url()} failed: ${request.failure()?.errorText ?? "unknown"}`,
    );
  });
  page.on("response", (response) => {
    if (!response.ok()) {
      diagnostics.push(
        `${response.request().method()} ${response.url()} returned HTTP ${response.status()}`,
      );
    }
  });

  const url = new URL(pageUrl);
  url.searchParams.set("dsn", dsn);
  await page.goto(url.toString(), { waitUntil: "load", timeout: 10_000 });
  await page.waitForFunction(
    () => window.__metricSdkResult?.complete === true,
    null,
    {
      timeout: 22_000,
    },
  );
  const result = await page.evaluate(() => window.__metricSdkResult);
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
  const state = await page
    ?.evaluate(() => window.__metricSdkResult)
    .catch(() => undefined);
  stderr.write(
    `${error instanceof Error ? error.stack : String(error)}\n` +
      `SDK state: ${JSON.stringify(state ?? null)}\n` +
      `Diagnostics: ${
        [...diagnostics, ...browserErrors].length > 0
          ? [...diagnostics, ...browserErrors].join("; ")
          : "none"
      }\n`,
  );
  process.exitCode = 2;
} finally {
  await browser?.close();
}
