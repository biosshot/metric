import { chromium } from "@playwright/test";
import { argv, stdout } from "node:process";
import { writeFileSync } from "node:fs";

const [base, email, password, projectId, nativeId, javascriptId, screenshot] =
  argv.slice(2);
if (!screenshot)
  throw new Error(
    "base, synthetic credentials, project, events and screenshot are required",
  );
const browser = await chromium.launch();
try {
  const page = await browser.newPage({
    locale: "en-US",
    viewport: { width: 1440, height: 1100 },
  });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript((project) => {
    window.localStorage.setItem("metric.project", project);
    window.localStorage.setItem("metric.locale", "en");
  }, projectId);
  await page.goto(base);
  await page.getByLabel("Email", { exact: true }).fill(email);
  await page.getByLabel("Password", { exact: true }).fill(password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await page.getByRole("link", { name: "Issues", exact: true }).waitFor();
  for (const [id, functions, filename, current] of [
    [
      nativeId,
      [
        "metric_pdb_fail(int)",
        "metric_pdb_parse(int)",
        "metric_pdb_handle(int)",
      ],
      "native-pdb.c",
      "int result = numerator / denominator;",
    ],
    [
      javascriptId,
      [
        "metricSourceMapFailure",
        "metricSourceMapParse",
        "metricSourceMapHandle",
      ],
      "source-map.js",
      'throw new Error("Metric real source map contract");',
    ],
  ]) {
    await page.goto(`${base}/events/${id}`);
    for (const name of functions)
      await page
        .locator(".stack-frame strong")
        .filter({ hasText: name })
        .waitFor();
    await page.getByText("Symbols resolved", { exact: true }).waitFor();
    const stack = page.locator(".stack-list");
    const text = await stack.innerText();
    if (!text.includes(filename))
      throw new Error("UI lost resolved source location");
    const displayed = await stack
      .locator(".stack-frame strong")
      .allTextContents();
    if (JSON.stringify(displayed) !== JSON.stringify(functions))
      throw new Error("UI changed caller order");
    const lines = stack
      .locator(".stack-frame")
      .first()
      .locator(".source-context li");
    if ((await lines.count()) !== 11)
      throw new Error(
        "UI did not render five lines before and after the failure",
      );
    const highlighted = await stack
      .locator(".stack-frame")
      .first()
      .locator(".source-context__current code")
      .innerText();
    if (highlighted.trim() !== current)
      throw new Error("UI highlighted the wrong source line");
    await page
      .getByRole("button", { name: "Show original frames", exact: true })
      .click();
    if (
      (await stack.innerText()).includes(functions[0]) ||
      (await stack.locator(".source-context").count())
    )
      throw new Error("raw toggle still displays derived symbols");
    await page
      .getByRole("button", { name: "Show resolved frames", exact: true })
      .click();
    await page.screenshot({
      path:
        id === nativeId
          ? screenshot
          : screenshot.replace("native-event.png", "javascript-event.png"),
      fullPage: true,
    });
  }
  if (errors.length) throw new Error(`browser errors: ${errors.join("; ")}`);
  stdout.write(`${JSON.stringify({ passed: 2, screenshot })}\n`);
} catch (error) {
  writeFileSync(screenshot.replace(/\.png$/, "-error.txt"), String(error));
  throw error;
} finally {
  await browser.close();
}
