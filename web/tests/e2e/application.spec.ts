import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page, type Route } from '@playwright/test';

const project = {
  id: '42',
  organization_id: '7',
  slug: 'backend',
  display_name: 'Backend',
  state: 'active',
  policy: {
    revision: 1,
    ip_policy: 'hmac',
    items: { error: true, client_report: true },
    limits: { max_event_bytes: 1048576, max_events_per_second: null, burst: null },
  },
  grouping_revision: 1,
  created_at: '2026-07-23T08:00:00Z',
};

const issue = {
  id: '00112233445566778899aabbccddeeff',
  project_id: '42',
  title: 'TypeError: cannot read session',
  culprit: 'src/session.ts in restoreSession',
  status: 'open',
  first_seen: '2026-07-23T08:00:00Z',
  last_seen: '2026-07-23T09:00:00Z',
  first_event_id: '11112222333344445555666677778888',
  latest_event_id: '11112222333344445555666677778888',
  representative_event_id: '11112222333344445555666677778888',
  occurrence_count: 17,
  occurrence_count_approximate: true,
  assignee: null,
  first_release: 'backend@1.0',
  last_release: 'backend@1.1',
  grouping: { strategy: 'stacktrace', summary: 'Application stack trace' },
};

const event = {
  event_id: issue.latest_event_id,
  project_id: '42',
  issue_id: issue.id,
  received_at: '2026-07-23T09:00:01Z',
  occurred_at: '2026-07-23T09:00:00Z',
  level: 'error',
  platform: 'javascript',
  body: {
    message: 'TypeError: cannot read session',
    exception: {
      values: [
        {
          type: 'TypeError',
          value: 'cannot read session',
          stacktrace: {
            frames: Array.from({ length: 120 }, (_, index) => ({
              filename: `src/frame-${index}.ts`,
              function: `function${index}`,
              lineno: index + 1,
              context_line: `throw new Error("frame ${index}")`,
              in_app: index % 2 === 0,
            })),
          },
        },
      ],
    },
  },
};

interface ApiState {
  role: 'owner' | 'viewer';
  csrfSeen: boolean;
  sessionCookieSeen: boolean;
  failIssues: boolean;
  emptyIssues?: boolean;
  slowIssues?: boolean;
  requestedProjects?: string[];
}

async function installApi(page: Page, state: ApiState): Promise<void> {
  await page.route('**/api/v1/**', async (route) => handleApi(route, state));
}

async function handleApi(route: Route, state: ApiState): Promise<void> {
  const request = route.request();
  const url = new URL(request.url());
  const path = url.pathname;
  const projectMatch = path.match(/^\/api\/v1\/projects\/([^/]+)/);
  if (projectMatch) state.requestedProjects?.push(projectMatch[1]);
  const json = (value: unknown, status = 200, headers: Record<string, string> = {}) =>
    route.fulfill({
      status,
      headers: { 'content-type': 'application/json', ...headers },
      body: JSON.stringify(value),
    });

  if (path === '/api/v1/auth/login') {
    const body = request.postDataJSON() as { email: string };
    state.role = body.email.startsWith('viewer') ? 'viewer' : 'owner';
    return json({ csrf_token: 'c'.repeat(64), expires_at: '2026-08-23T09:00:00Z' }, 200, {
      'set-cookie': `faultkeep_session=${'d'.repeat(64)}; Path=/api/v1; HttpOnly; SameSite=Lax`,
    });
  }
  if (path === '/api/v1/auth/me') {
    state.sessionCookieSeen = request.headers().cookie?.includes('faultkeep_session=') ?? false;
    return json({
      actor: 'web_session',
      user_id: state.role === 'viewer' ? '9' : '8',
      organization_id: '7',
      role: state.role,
      permissions:
        state.role === 'viewer'
          ? ['event:read', 'issue:read', 'project:read']
          : ['event:read', 'issue:read', 'issue:write', 'project:read', 'project:admin'],
      credential_id: '12',
    });
  }
  if (path === '/api/v1/projects') return json({ items: [project] });
  if (path === '/api/v1/projects/42/issues') {
    if (state.slowIssues) await new Promise((resolve) => setTimeout(resolve, 700));
    if (state.failIssues) {
      return json(
        {
          error: {
            code: 'temporarily_unavailable',
            message: 'service is temporarily unavailable',
            request_id: 'browser-request-503',
          },
        },
        503,
      );
    }
    if (state.emptyIssues) return json({ items: [], next_cursor: null });
    return json({ items: [issue], next_cursor: null });
  }
  if (path === `/api/v1/projects/42/issues/${issue.id}`) return json(issue);
  if (path.endsWith('/statistics')) {
    return json({
      items: [{ bucket_start: '2026-07-23T09:00:00Z', occurrence_count: 17, approximate: true }],
    });
  }
  if (path.endsWith('/activity')) return json({ items: [], next_cursor: null });
  if (path.endsWith('/events') && path.includes('/issues/')) {
    return json({ items: [event], next_cursor: null });
  }
  if (path.endsWith('/lifecycle')) {
    state.csrfSeen =
      request.headers()['x-csrf-token'] === 'c'.repeat(64) &&
      request.headers()['x-faultkeep-organization-id'] === '7';
    issue.status =
      (request.postDataJSON() as { action: 'resolve' | 'ignore' | 'reopen' }).action === 'resolve'
        ? 'resolved'
        : 'open';
    return json({ applied: true, issue });
  }
  if (path === `/api/v1/projects/42/events/${event.event_id}`) return json(event);
  if (path === '/api/v1/capabilities') {
    return json({
      api_version: 'v1',
      search: {
        fields: ['event.id', 'issue', 'environment', 'release'],
        full_text: false,
        custom_tags: false,
        max_page_size: 100,
      },
      features: { native_api: true, web: true, retention: false, mcp: false },
    });
  }
  if (path === '/api/v1/status') {
    return json({
      status: 'ready',
      components: { mongodb: 'available', writer: 'running', processor: 'running' },
    });
  }
  return json({ error: { code: 'not_found', message: 'not found', request_id: 'mock-404' } }, 404);
}

async function login(page: Page, email = 'owner@example.com'): Promise<void> {
  await page.goto('/');
  await page.getByLabel('Email').fill(email);
  await page.getByLabel('Password').fill('correct horse battery staple');
  await page.getByLabel('Organization ID').fill('7');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Issues' })).toBeVisible();
}

test('login session, investigation and CSRF lifecycle are coherent', async ({ page }) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
  };
  await installApi(page, state);
  await login(page);
  expect(state.sessionCookieSeen).toBe(true);
  expect(await page.evaluate(() => document.cookie)).not.toContain('faultkeep_session');

  await page.getByRole('link', { name: /TypeError/ }).click();
  await expect(page.getByRole('heading', { name: 'TypeError: cannot read session' })).toBeVisible();
  await page.getByRole('button', { name: 'Resolve' }).click();
  await expect(page.getByRole('status')).toContainText('Issue marked resolved');
  expect(state.csrfSeen).toBe(true);

  await page.getByRole('link', { name: /javascript · error/ }).click();
  await expect(page.getByRole('heading', { name: '120 frames' })).toBeVisible();
  await expect(page.locator('.stack-frame')).toHaveCount(40);
  await page.getByRole('button', { name: 'Show all 120' }).click();
  await expect(page.locator('.stack-frame')).toHaveCount(120);
});

test('viewer sees read-only controls and no hidden write action', async ({ page }) => {
  const state: ApiState = {
    role: 'viewer',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
    requestedProjects: [],
  };
  await installApi(page, state);
  await page.addInitScript(() => localStorage.setItem('faultkeep.project', '666'));
  await login(page, 'viewer@example.com');
  await page.getByRole('link', { name: /TypeError/ }).click();

  await expect(page.getByText('Read-only role: lifecycle controls are unavailable.')).toBeVisible();
  await expect(page.getByRole('button', { name: 'Resolve' })).toHaveCount(0);
  expect(state.requestedProjects).not.toContain('666');
  expect(state.requestedProjects).toContain('42');
});

test('server failures expose status, code and request ID', async ({ page }) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: true,
  };
  await installApi(page, state);
  await login(page);

  const alert = page.getByRole('alert');
  await expect(alert).toContainText('temporarily_unavailable');
  await expect(alert).toContainText('HTTP');
  await expect(alert).toContainText('503');
  await expect(alert).toContainText('browser-request-503');
});

test('loading and empty states explain what is happening', async ({ page }) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
    emptyIssues: true,
    slowIssues: true,
  };
  await installApi(page, state);
  await login(page);

  await expect(page.getByRole('status')).toContainText('Loading investigation data');
  await expect(page.getByRole('heading', { name: 'No Issues in this view' })).toBeVisible();
  await expect(page.getByText('Events sent by your SDK will appear here')).toBeVisible();
});

test('primary investigation view has no serious accessibility violations', async ({ page }) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
  };
  await installApi(page, state);
  await login(page);

  const results = await new AxeBuilder({ page }).disableRules(['color-contrast']).analyze();
  expect(
    results.violations.filter((item) => item.impact === 'serious' || item.impact === 'critical'),
  ).toEqual([]);
});
