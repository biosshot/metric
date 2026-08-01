import { chromium } from 'playwright';
import { error as logError } from 'node:console';
import { argv } from 'node:process';

const [baseUrl, email, password] = argv.slice(2);
if (!baseUrl || !email || !password) {
  throw new Error('base URL and credentials are required');
}

const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  page.on('response', async (response) => {
    if (response.url().includes('/api/v1/') && response.status() >= 400) {
      const body = await response.text().catch(() => '<unreadable response>');
      const requestBody =
        response.request().method() === 'PATCH'
          ? ` request=${response.request().postData() ?? '<empty>'}`
          : '';
      logError(`Metric API ${response.status()} ${response.url()}: ${body}${requestBody}`);
    }
  });
  await page.goto(baseUrl);
  await page.getByLabel('Email').fill(email);
  await page.getByLabel('Password').fill(password);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await page.getByRole('heading', { name: 'Create your first project' }).waitFor();

  if ((await page.evaluate(() => globalThis.document.cookie)).includes('metric_session')) {
    throw new Error('HttpOnly session cookie became visible to JavaScript');
  }

  await page.getByLabel('Project name').fill('Backend');
  await page.getByRole('button', { name: 'Create project and DSN' }).click();
  await page.getByRole('heading', { name: 'Connect an SDK' }).waitFor();
  await page.getByText('Available DSNs').waitFor();

  await page.getByRole('link', { name: /Project settings/ }).click();
  await page.getByText(/Raw Events are retained for/).waitFor();
  await page.getByLabel('IP address handling').selectOption('remove');
  await page.getByRole('button', { name: 'Save policy' }).click();
  await Promise.race([
    page.getByText('Project policy saved.').waitFor(),
    page
      .getByRole('alert')
      .waitFor()
      .then(async () => {
        throw new Error(`policy save failed: ${await page.getByRole('alert').innerText()}`);
      }),
  ]);
} finally {
  await browser.close();
}
