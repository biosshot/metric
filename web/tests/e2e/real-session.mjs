import { chromium } from 'playwright';
import { argv } from 'node:process';

const [baseUrl, email, password, organizationId] = argv.slice(2);
if (!baseUrl || !email || !password || !organizationId) {
  throw new Error('base URL and credentials are required');
}

const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  await page.goto(baseUrl);
  await page.getByLabel('Email').fill(email);
  await page.getByLabel('Password').fill(password);
  await page.getByLabel('Organization ID').fill(organizationId);
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await page.getByRole('heading', { name: 'Issues' }).waitFor();

  if ((await page.evaluate(() => globalThis.document.cookie)).includes('faultkeep_session')) {
    throw new Error('HttpOnly session cookie became visible to JavaScript');
  }

  await page.getByRole('link', { name: /Project settings/ }).click();
  await page.getByLabel('IP address handling').selectOption('remove');
  await page.getByRole('button', { name: 'Save policy' }).click();
  await page.getByText('Project policy saved.').waitFor();
} finally {
  await browser.close();
}
