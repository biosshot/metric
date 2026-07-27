import AxeBuilder from '@axe-core/playwright';
import { expect, test, type Page, type Route } from '@playwright/test';
import { mkdir } from 'node:fs/promises';
import path from 'node:path';

const project = {
  id: '42',
  organization_id: '7',
  slug: 'backend',
  display_name: 'Backend',
  state: 'active',
  policy: {
    revision: 1,
    ip_policy: 'hmac',
    items: { error: true, client_report: true, log: true, transaction: true, span: true },
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
              pre_context: ['const state = restore();'],
              context_line: `throw new Error("frame ${index}")`,
              post_context: ['report(state);'],
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
  logoutCsrfSeen?: boolean;
  sessionCookieSeen: boolean;
  failIssues: boolean;
  emptyIssues?: boolean;
  slowIssues?: boolean;
  noProjects?: boolean;
  bootstrapSeen?: boolean;
  projectCreationSeen?: boolean;
  policyRevisionSeen?: boolean;
  createdProjectBody?: Record<string, unknown>;
  requestedProjects?: string[];
  savedQueries?: Array<Record<string, any>>;
  dashboards?: Array<Record<string, any>>;
  dashboardVariableSeen?: boolean;
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

  if (path === '/api/v1/auth/bootstrap') {
    const body = request.postDataJSON() as Record<string, unknown>;
    state.bootstrapSeen =
      body.setup_token === 'a'.repeat(64) &&
      body.organization_slug === 'acme' &&
      body.email === 'owner@example.com';
    return json({
      actor: 'bootstrap',
      user_id: '8',
      organization_id: '7',
      role: 'owner',
      permissions: ['organization:admin', 'project:admin'],
      credential_id: '11',
    });
  }
  if (path === '/api/v1/auth/login') {
    const body = request.postDataJSON() as { email: string };
    state.role = body.email.startsWith('viewer') ? 'viewer' : 'owner';
    return json({ csrf_token: 'c'.repeat(64), expires_at: '2026-08-23T09:00:00Z' }, 200, {
      'set-cookie': `metric_session=${'d'.repeat(64)}; Path=/api/v1; HttpOnly; SameSite=Lax`,
    });
  }
  if (path === '/api/v1/auth/me') {
    state.sessionCookieSeen = request.headers().cookie?.includes('metric_session=') ?? false;
    return json({
      actor: 'web_session',
      user_id: state.role === 'viewer' ? '9' : '8',
      organization_id: '7',
      role: state.role,
      permissions:
        state.role === 'viewer'
          ? ['event:read', 'issue:read', 'project:read']
          : [
              'event:read',
              'issue:read',
              'issue:write',
              'project:read',
              'project:admin',
              'organization:admin',
            ],
      credential_id: '12',
    });
  }
  if (path === '/api/v1/auth/logout' && request.method() === 'POST') {
    state.logoutCsrfSeen = request.headers()['x-csrf-token'] === 'c'.repeat(64);
    return route.fulfill({ status: 204 });
  }
  if (path === '/api/v1/projects' && request.method() === 'POST') {
    state.projectCreationSeen =
      request.headers()['x-csrf-token'] === 'c'.repeat(64) &&
      request.headers()['x-metric-organization-id'] === '7';
    state.createdProjectBody = request.postDataJSON() as Record<string, unknown>;
    state.noProjects = false;
    return json({ project_id: project.id, dsn_key: 'e'.repeat(32) }, 201);
  }
  if (path === '/api/v1/projects') {
    return json({ items: state.noProjects ? [] : [project] });
  }
  if (path === '/api/v1/projects/42/keys') {
    return json({
      items: [
        {
          dsn_key: 'e'.repeat(32),
          project_id: project.id,
          state: 'active',
          label: 'Default',
          created_at: '2026-07-23T08:00:00Z',
        },
      ],
    });
  }
  if (path === '/api/v1/projects/42') return json(project);
  if (path === '/api/v1/projects/42/policy' && request.method() === 'PATCH') {
    const body = request.postDataJSON() as {
      expected_revision: number;
      ip_policy: 'hmac' | 'keep' | 'remove' | 'truncate';
    };
    state.policyRevisionSeen = body.expected_revision === 1;
    return json({ ...project.policy, revision: 2, ip_policy: body.ip_policy });
  }
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
      request.headers()['x-metric-organization-id'] === '7';
    issue.status =
      (request.postDataJSON() as { action: 'resolve' | 'ignore' | 'reopen' }).action === 'resolve'
        ? 'resolved'
        : 'open';
    return json({ applied: true, issue });
  }
  if (path === `/api/v1/projects/42/events/${event.event_id}`) return json(event);
  if (path === '/api/v1/projects/42/explore' && request.method() === 'POST') {
    const body = request.postDataJSON() as {
      dataset: string;
      from: number;
      until: number;
      aggregates: Array<{ function: string }>;
    };
    if (
      body.dataset !== 'errors' ||
      body.aggregates[0]?.function !== 'count' ||
      body.until <= body.from
    ) {
      return json(
        {
          error: {
            code: 'explore_invalid_query',
            message: 'Explore query is invalid',
            request_id: 'explore-invalid',
          },
        },
        422,
      );
    }
    return json({
      shape: 'number',
      dataset: 'errors',
      normalized: 'v1|errors|bounded',
      cost: 198,
      items: [{ count: 17 }],
      next_cursor: null,
    });
  }
  if (path === '/api/v1/projects/42/saved-queries') {
    state.savedQueries ??= [];
    if (request.method() === 'POST') {
      state.csrfSeen = request.headers()['x-csrf-token'] === 'c'.repeat(64);
      const body = request.postDataJSON() as { name: string; query: Record<string, unknown> };
      const saved = {
        id: `${state.savedQueries.length + 1}`.padStart(32, '0'),
        project_id: '42',
        name: body.name,
        query: body.query,
        revision: 1,
        created_by: '8',
        updated_by: '8',
        created_at: Date.now(),
        updated_at: Date.now(),
      };
      state.savedQueries.push(saved);
      return json(saved, 201);
    }
    return json({ items: state.savedQueries });
  }
  const savedMatch = path.match(/^\/api\/v1\/projects\/42\/saved-queries\/([0-9a-f]+)$/);
  if (savedMatch) {
    state.savedQueries ??= [];
    const index = state.savedQueries.findIndex((item) => item.id === savedMatch[1]);
    if (request.method() === 'DELETE') {
      if (index >= 0) state.savedQueries.splice(index, 1);
      return route.fulfill({ status: 204 });
    }
    if (request.method() === 'PATCH' && index >= 0) {
      const body = request.postDataJSON() as Record<string, unknown>;
      state.savedQueries[index] = {
        ...state.savedQueries[index],
        ...body,
        revision: Number(body.revision) + 1,
      };
      return json(state.savedQueries[index]);
    }
    return index >= 0 ? json(state.savedQueries[index]) : json({}, 404);
  }
  if (path === '/api/v1/projects/42/dashboards') {
    state.dashboards ??= [];
    if (request.method() === 'POST') {
      const body = request.postDataJSON() as {
        name: string;
        widgets: Array<Record<string, unknown>>;
        refresh_interval: string;
      };
      const dashboard = {
        id: `${state.dashboards.length + 101}`.padStart(32, '0'),
        project_id: '42',
        name: body.name,
        widgets: body.widgets.map((widget, index) => ({
          ...widget,
          id: `${index + 201}`.padStart(32, '0'),
        })),
        refresh_interval: body.refresh_interval,
        revision: 1,
        created_by: '8',
        updated_by: '8',
        created_at: Date.now(),
        updated_at: Date.now(),
      };
      state.dashboards.push(dashboard);
      return json(dashboard, 201);
    }
    return json({ items: state.dashboards });
  }
  const dashboardMatch = path.match(/^\/api\/v1\/projects\/42\/dashboards\/([0-9a-f]+)$/);
  if (dashboardMatch) {
    state.dashboards ??= [];
    const index = state.dashboards.findIndex((item) => item.id === dashboardMatch[1]);
    if (request.method() === 'DELETE') {
      if (index >= 0) state.dashboards.splice(index, 1);
      return route.fulfill({ status: 204 });
    }
    if (request.method() === 'PATCH' && index >= 0) {
      const body = request.postDataJSON() as Record<string, unknown>;
      state.dashboards[index] = {
        ...state.dashboards[index],
        ...body,
        revision: Number(body.revision) + 1,
      };
      return json(state.dashboards[index]);
    }
    return index >= 0 ? json(state.dashboards[index]) : json({}, 404);
  }
  const refreshMatch = path.match(/^\/api\/v1\/projects\/42\/dashboards\/([0-9a-f]+)\/refresh$/);
  if (refreshMatch) {
    const body = request.postDataJSON() as { environment?: string; release?: string };
    state.dashboardVariableSeen = body.environment === 'production';
    const dashboard = state.dashboards?.find((item) => item.id === refreshMatch[1]);
    return json({
      dashboard_id: dashboard?.id,
      refreshed_at: Date.now(),
      total_cost: 198,
      widgets: (dashboard?.widgets ?? []).map((widget: Record<string, any>) => {
        const exists = state.savedQueries?.some((item) => item.id === widget.saved_query_id);
        return {
          widget_id: widget.id,
          cost: exists ? 198 : null,
          error_code: exists ? null : 'saved_query_missing',
          items: exists ? [{ count: 23 }] : null,
        };
      }),
    });
  }
  if (path === '/api/v1/capabilities') {
    return json({
      api_version: 'v1',
      search: {
        fields: ['event.id', 'issue', 'environment', 'release'],
        full_text: false,
        custom_tags: false,
        max_page_size: 100,
      },
      features: { native_api: true, web: true, retention: true, mcp: false },
      retention: {
        events_days: 30,
        issue_stats_hourly_days: 400,
        clock: 'received_at',
        gradual_policy_reduction: true,
      },
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
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Issues' })).toBeVisible();
  expect(state.sessionCookieSeen).toBe(true);
  expect(await page.evaluate(() => document.cookie)).not.toContain('metric_session');

  await page.getByRole('link', { name: /TypeError/ }).click();
  await expect(page.getByRole('heading', { name: 'TypeError: cannot read session' })).toBeVisible();
  await page.getByRole('button', { name: 'Resolve' }).click();
  await expect(page.getByRole('status')).toContainText('Issue marked resolved');
  expect(state.csrfSeen).toBe(true);

  await page.getByRole('link', { name: /javascript · error/ }).click();
  await expect(page.getByRole('heading', { name: '120 frames' })).toBeVisible();
  await expect(page.locator('.stack-frame')).toHaveCount(40);
  await expect(page.locator('.source-context').first()).toContainText('const state = restore();');
  await expect(page.locator('.source-context').first()).toContainText('report(state);');
  await page.getByRole('button', { name: 'Show all 120' }).click();
  await expect(page.locator('.stack-frame')).toHaveCount(120);

  await page.getByRole('link', { name: /Project settings/ }).click();
  await expect(page.getByText(/Raw Events are retained for/)).toContainText('30 days');
  await expect(page.getByText(/Hourly Issue statistics/)).toContainText('400 days');
  await page.getByRole('combobox', { name: 'IP address handling' }).click();
  await page.getByRole('option', { name: 'Remove completely', exact: true }).click();
  await page.getByRole('button', { name: 'Save policy' }).click();
  await expect(page.getByRole('status')).toContainText('Project policy saved');
  expect(state.policyRevisionSeen).toBe(true);

  await page.getByRole('button', { name: 'Sign out' }).click();
  await expect(page.getByRole('heading', { name: 'Sign in to Metric' })).toBeVisible();
  expect(state.logoutCsrfSeen).toBe(true);
});

test('first setup creates a project and reaches an actionable SDK DSN', async ({ page }) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
    noProjects: true,
  };
  await installApi(page, state);
  await page.goto('/');

  await page.getByRole('tab', { name: 'First setup' }).click();
  await page.getByLabel('Setup token').fill('a'.repeat(64));
  await page.getByLabel('Your name').fill('Owner');
  await page.getByLabel('Email').fill('owner@example.com');
  await page.getByLabel('Password').fill('correct horse battery staple');
  await page.getByLabel('Organization', { exact: true }).fill('Acme');
  await page.getByLabel('Slug').fill('acme');
  await page.getByRole('button', { name: 'Create owner and organization' }).click();

  await expect(page.getByText('Organization created. Its ID is')).toContainText('7');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Create your first project' })).toBeVisible();

  await page.getByLabel('Project name').fill('Payments API');
  await expect(page.getByLabel('Slug')).toHaveValue('payments-api');
  await page.getByRole('button', { name: 'Create project and DSN' }).click();

  await expect(page.getByRole('heading', { name: 'Connect an SDK' })).toBeVisible();
  await expect(page.getByText('Default')).toBeVisible();
  await expect(page.locator('.dsn-list code')).toContainText('e'.repeat(32));
  await expect(page.locator('.code-block')).toContainText('e'.repeat(32));
  await expect(page.locator('.code-block')).not.toContainText('PASTE_DSN_HERE');
  await page.getByRole('combobox', { name: 'SDK' }).click();
  await page.getByRole('option', { name: 'Python', exact: true }).click();
  await expect(page.locator('.code-block')).toContainText('sentry_sdk.init');
  expect(state.bootstrapSeen).toBe(true);
  expect(state.projectCreationSeen).toBe(true);
  expect(state.createdProjectBody).toMatchObject({
    display_name: 'Payments API',
    slug: 'payments-api',
    ip_policy: 'hmac',
    error_enabled: true,
    max_event_bytes: 1_048_576,
  });
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
  await page.addInitScript(() => localStorage.setItem('metric.project', '666'));
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

test('Explore submits a typed bounded query and renders a number result', async ({ page }) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
  };
  await installApi(page, state);
  await login(page);
  await page.getByRole('link', { name: 'Explore' }).click();
  await expect(page.getByRole('heading', { name: 'Unified Explore' })).toBeVisible();
  await page.getByRole('combobox', { name: 'Result' }).click();
  await page.getByRole('option', { name: /^Number/ }).click();
  await page.getByRole('button', { name: 'Run query' }).click();
  await expect(page.locator('.explore-number')).toContainText('17');
  await expect(page.getByText('Estimated cost')).toContainText('198');
});

test('Dashboard lifecycle applies variables and keeps partial widget failures visible', async ({
  page,
}) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
    savedQueries: [],
    dashboards: [],
  };
  await installApi(page, state);
  await login(page);
  await page.getByRole('link', { name: 'Dashboards' }).click();
  await expect(page.getByRole('heading', { name: 'Dashboards', exact: true })).toBeVisible();

  await page.getByPlaceholder('Production log volume').fill('Production log volume');
  await page.getByRole('button', { name: 'Save query' }).click();
  await expect(page.getByLabel('Saved query name')).toHaveValue('Production log volume');

  await page.getByPlaceholder('Service health').fill('Service health');
  await page.getByRole('combobox', { name: 'Saved query' }).click();
  await page.getByRole('option', { name: /Production log volume/ }).click();
  await page.getByRole('button', { name: 'Add' }).click();
  await page.getByRole('button', { name: 'Create dashboard' }).click();
  await expect(page.getByLabel('Dashboard name')).toHaveValue('Service health');

  await page.getByPlaceholder('All environments').fill('production');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(page.locator('.dashboard-widget__number')).toContainText('23');
  expect(state.dashboardVariableSeen).toBe(true);
  expect(state.csrfSeen).toBe(true);

  await page.locator('.saved-query-list article').getByRole('button', { name: 'Delete' }).click();
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(page.getByText('saved_query_missing')).toBeVisible();

  await page.locator('.dashboard-card').getByRole('button', { name: 'Delete' }).click();
  await expect(page.getByRole('heading', { name: 'No dashboards yet' })).toBeVisible();
});

test('all routes have no serious accessibility violations at desktop and narrow widths', async ({
  page,
}) => {
  test.setTimeout(90_000);
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
  };
  await installApi(page, state);

  async function expectAccessible(label: string): Promise<void> {
    const results = await new AxeBuilder({ page }).analyze();
    expect(
      results.violations
        .filter((item) => item.impact === 'serious' || item.impact === 'critical')
        .map((item) => ({ id: item.id, targets: item.nodes.map((node) => node.target) })),
      label,
    ).toEqual([]);
  }

  await page.goto('/');
  for (const viewport of [
    { name: 'desktop', width: 1440, height: 1000 },
    { name: 'narrow', width: 390, height: 844 },
  ]) {
    await page.setViewportSize(viewport);
    await expectAccessible(`auth/${viewport.name}`);
  }

  await page.setViewportSize({ width: 1440, height: 1000 });
  await login(page);
  const routes = [
    { name: 'issues', url: '/issues', heading: 'Issues' },
    { name: 'issue-detail', url: `/issues/${issue.id}`, heading: issue.title },
    { name: 'event-detail', url: `/events/${event.event_id}`, heading: '120 frames' },
    { name: 'explore', url: '/explore', heading: 'Unified Explore' },
    { name: 'dashboards', url: '/dashboards', heading: 'Dashboards' },
    { name: 'sdk-setup', url: '/project/setup', heading: 'Connect an SDK' },
    { name: 'project-settings', url: '/project/settings', heading: 'Project settings' },
    { name: 'system-status', url: '/system', heading: 'System status' },
  ];

  for (const route of routes) {
    await page.goto(route.url);
    await expect(page.getByRole('heading', { name: route.heading })).toBeVisible();
    for (const viewport of [
      { name: 'desktop', width: 1440, height: 1000 },
      { name: 'narrow', width: 390, height: 844 },
    ]) {
      await page.setViewportSize(viewport);
      await expectAccessible(`${route.name}/${viewport.name}`);
    }
  }
});

test('capture Phase 23 desktop and narrow route reference renders', async ({
  page,
  browserName,
}) => {
  test.skip(
    browserName !== 'chromium' || process.env.METRIC_CAPTURE_PHASE23 !== '1',
    'Reference renders are regenerated explicitly with Chromium.',
  );
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
  };
  issue.status = 'open';
  await installApi(page, state);
  const output = path.resolve(process.cwd(), '../arch-docs/phase-reports/assets/0023');
  await mkdir(output, { recursive: true });

  async function capture(name: string): Promise<void> {
    await page.setViewportSize({ width: 1440, height: 1000 });
    await page.screenshot({
      path: path.join(output, `${name}-desktop.png`),
      animations: 'disabled',
    });
    await page.setViewportSize({ width: 390, height: 844 });
    await page.screenshot({
      path: path.join(output, `${name}-narrow.png`),
      animations: 'disabled',
    });
  }

  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Sign in to Metric' })).toBeVisible();
  await capture('auth');

  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.getByLabel('Email').fill('owner@example.com');
  await page.getByLabel('Password').fill('correct horse battery staple');
  await page.getByLabel('Organization ID').fill('7');
  await page.getByRole('button', { name: 'Sign in', exact: true }).click();

  const routes = [
    { name: 'issues', url: '/issues', heading: 'Issues' },
    { name: 'issue-detail', url: `/issues/${issue.id}`, heading: issue.title },
    { name: 'event-detail', url: `/events/${event.event_id}`, heading: '120 frames' },
    { name: 'sdk-setup', url: '/project/setup', heading: 'Connect an SDK' },
    { name: 'project-settings', url: '/project/settings', heading: 'Project settings' },
    { name: 'system-status', url: '/system', heading: 'System status' },
  ];

  for (const route of routes) {
    await page.goto(route.url);
    await expect(page.getByRole('heading', { name: route.heading })).toBeVisible();
    await capture(route.name);
  }

  await page.goto('/project/settings');
  const dangerZone = page.locator('.danger-zone');
  await expect(dangerZone).toBeVisible();
  await page.setViewportSize({ width: 1440, height: 1000 });
  await dangerZone.screenshot({
    path: path.join(output, 'delete-project-desktop.png'),
    animations: 'disabled',
  });
  await page.setViewportSize({ width: 390, height: 844 });
  await dangerZone.screenshot({
    path: path.join(output, 'delete-project-narrow.png'),
    animations: 'disabled',
  });

  await page.goto('/project/setup');
  await page.getByRole('combobox', { name: 'SDK' }).click();
  await capture('sdk-select-open');

  await page.emulateMedia({ media: 'print', colorScheme: 'dark' });
  await page.setViewportSize({ width: 1440, height: 1000 });
  for (const route of [
    { name: 'issues-print', url: '/issues', heading: 'Issues' },
    { name: 'event-detail-print', url: `/events/${event.event_id}`, heading: '120 frames' },
  ]) {
    await page.goto(route.url);
    await expect(page.getByRole('heading', { name: route.heading })).toBeVisible();
    await page.screenshot({
      path: path.join(output, `${route.name}.png`),
      animations: 'disabled',
    });
  }
  await page.emulateMedia({ media: 'screen', colorScheme: 'dark' });
});
