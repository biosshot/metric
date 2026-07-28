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

const replayRecord = {
  id: 'a1477a22ee174888834b000d10a284f7',
  project_id: '42',
  started_at: '2026-07-23T09:00:00Z',
  ended_at: '2026-07-23T09:00:09Z',
  received_at: '2026-07-23T09:00:10Z',
  duration_ms: 9_000,
  environment: 'manual-replay-demo',
  release: 'metric-browser-replay-demo@1.0.0',
  url: 'https://example.test/replay',
  error_ids: [],
  trace_ids: [],
  segments: [
    {
      segment_id: 0,
      size: 512,
      decompressed_bytes: 1_024,
      event_count: 8,
      checksum: 'a'.repeat(64),
    },
    {
      segment_id: 1,
      size: 256,
      decompressed_bytes: 512,
      event_count: 4,
      checksum: 'b'.repeat(64),
    },
  ],
  partial: false,
  expires_at: null,
};

const logRecord = {
  id: '31'.repeat(16),
  project_id: '42',
  received_at: '2026-07-23T09:00:01Z',
  timestamp: '2026-07-23T09:00:00Z',
  timestamp_ns: '1784797200000000000',
  level: 'info',
  message: 'Worker accepted the scheduled job',
  trace_id: '41'.repeat(16),
  span_id: '51'.repeat(8),
  environment: 'production',
  release: 'backend@1.1',
  service: 'worker',
  body: {},
};

const transactionRecord = {
  id: '61'.repeat(16),
  project_id: '42',
  received_at: '2026-07-23T09:00:01Z',
  started_at: '2026-07-23T09:00:00Z',
  started_at_ns: '1784797200000000000',
  ended_at: '2026-07-23T09:00:00.125Z',
  duration_ns: '125000000',
  duration_ms: 125,
  trace_id: '41'.repeat(16),
  span_id: '51'.repeat(8),
  parent_span_id: null,
  is_segment: true,
  operation_class: 'http',
  operation: 'http.server',
  status: 'ok',
  name: 'GET /api/jobs',
  environment: 'production',
  release: 'backend@1.1',
  service: 'api',
  insight_flags: 0,
  body: {},
};

const feedbackRecord = {
  id: '71'.repeat(16),
  project_id: '42',
  received_at: '2026-07-23T09:00:00Z',
  status: 'open',
  status_changed_at: '2026-07-23T09:00:00Z',
  message: 'The save action needs a clearer confirmation.',
  name: 'Ada',
  contact_email: 'ada@example.com',
  url: 'https://example.test/settings',
  associated_event_id: null,
  issue_id: null,
  trace_id: null,
  replay_id: null,
  attachments: [],
  expires_at: '2026-08-23T09:00:00Z',
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
  monitors?: Array<Record<string, any>>;
  monitorRuns?: Array<Record<string, any>>;
  alertRules?: Array<Record<string, any>>;
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
  if (path === '/api/v1/projects/42/logs') {
    return json({ items: [logRecord], next_cursor: null });
  }
  if (path === '/api/v1/projects/42/transactions') {
    return json({ items: [transactionRecord], next_cursor: null });
  }
  if (path === '/api/v1/projects/42/feedback') {
    return json({ items: [feedbackRecord], next_cursor: null });
  }
  if (path === `/api/v1/projects/42/replays/${replayRecord.id}`) {
    return json(replayRecord);
  }
  if (path === '/api/v1/projects/42/replays') {
    return json({ items: [replayRecord], next_cursor: null });
  }
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
  if (path === '/api/v1/projects/42/monitors') {
    state.monitors ??= [];
    if (request.method() === 'POST') {
      state.csrfSeen = request.headers()['x-csrf-token'] === 'c'.repeat(64);
      const body = request.postDataJSON() as Record<string, any>;
      const monitor = {
        id: '36'.repeat(16),
        project_id: '42',
        ...body,
        managed_by: 'web',
        revision: 1,
        kind: body.kind,
        uptime:
          body.kind === 'uptime'
            ? {
                endpoint: body.endpoint,
                method: body.method,
                expected_status_min: body.expected_status_min,
                expected_status_max: body.expected_status_max,
                timeout_seconds: body.timeout_seconds,
                max_redirects: body.max_redirects,
                headers: body.headers.map((header: { name: string }) => ({
                  name: header.name,
                  sensitive: header.name === 'authorization',
                  has_value: true,
                })),
              }
            : null,
        next_expected_at: '2026-07-27T15:10:00Z',
        last_run_id: null,
        last_status: 'success',
        last_check_in_at: '2026-07-27T15:05:00Z',
        created_at: '2026-07-27T15:00:00Z',
        updated_at: '2026-07-27T15:00:00Z',
      };
      state.monitors = [monitor];
      state.monitorRuns = [
        {
          id: '37'.repeat(16),
          monitor_id: monitor.id,
          status: 'error',
          source: 'scheduler',
          scheduled_for: '2026-07-27T15:00:00Z',
          started_at: '2026-07-27T15:00:00Z',
          finished_at: '2026-07-27T15:00:01Z',
          duration_ms: 812,
          received_at: '2026-07-27T15:00:01Z',
          release_id: null,
          http_status: 503,
          uptime_failure: 'unexpected_status',
        },
        {
          id: '38'.repeat(16),
          monitor_id: monitor.id,
          status: 'success',
          source: 'scheduler',
          scheduled_for: '2026-07-27T15:05:00Z',
          started_at: '2026-07-27T15:05:00Z',
          finished_at: '2026-07-27T15:05:00Z',
          duration_ms: 93,
          received_at: '2026-07-27T15:05:00Z',
          release_id: null,
          http_status: 200,
          uptime_failure: null,
        },
      ];
      return json(monitor);
    }
    return json({ items: state.monitors });
  }
  if (path === `/api/v1/projects/42/monitors/${'36'.repeat(16)}/runs`) {
    return json({ items: state.monitorRuns ?? [] });
  }
  if (path === '/api/v1/projects/42/notification-destinations') {
    return json({
      items: [
        {
          id: '39'.repeat(16),
          project_id: '42',
          kind: 'telegram',
          endpoint: 'metric-alerts',
          enabled: true,
          smtp: null,
          created_at: Date.now(),
          updated_at: Date.now(),
        },
      ],
    });
  }
  if (path === '/api/v1/projects/42/alert-rules') {
    state.alertRules ??= [];
    if (request.method() === 'POST') {
      const body = request.postDataJSON() as Record<string, any>;
      const rule = {
        id: '40'.repeat(16),
        project_id: '42',
        ...body,
        aggregate: null,
        monitor: {
          monitor_id: body.monitor_id,
          outcomes: body.monitor_outcomes,
          notify_resolved: body.notify_resolved,
        },
        threshold_met: false,
        created_at: Date.now(),
        updated_at: Date.now(),
      };
      state.alertRules = [rule];
      return json(rule, 201);
    }
    return json({ items: state.alertRules });
  }
  if (path === '/api/v1/projects/42/notification-deliveries') {
    return json({ items: [] });
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
  await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toBeVisible();
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
  await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toBeVisible();
  await page.getByRole('link', { name: 'Issues', exact: true }).click();
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

  await page.goto('/settings/project');
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

test('uptime monitor lifecycle shows history and configures recovery alerts', async ({ page }) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
    monitors: [],
    monitorRuns: [],
    alertRules: [],
  };
  await installApi(page, state);
  await login(page);

  await page.getByRole('link', { name: 'Monitors', exact: true }).click();
  await page.getByRole('combobox', { name: 'Monitor type' }).click();
  await page.getByRole('option', { name: /^Uptime HTTP/ }).click();
  await page.getByLabel('Name', { exact: true }).fill('Public API');
  await page.getByLabel('Slug', { exact: true }).fill('public-api');
  await page.getByLabel('Public HTTP(S) endpoint').fill('https://status.example.com/health');
  await page.getByRole('button', { name: 'Add header' }).click();
  await page.getByLabel('Header name').fill('authorization');
  await page.getByLabel('Secret value').fill('Bearer secret');
  await page.getByRole('button', { name: 'Save monitor' }).click();

  await expect(page.getByText('Uptime · public-api · production')).toBeVisible();
  await expect(page.getByText('HTTP 503')).toBeVisible();
  await expect(page.getByText('unexpected_status')).toBeVisible();
  await expect(page.getByText('HTTP 200')).toBeVisible();
  expect(state.csrfSeen).toBe(true);

  await page.goto('/settings/notifications');
  await page.getByRole('combobox', { name: 'Rule type' }).click();
  await page.getByRole('option', { name: /^Monitor outcome/ }).click();
  await page.getByLabel('Rule name').fill('Public API availability');
  await page.getByRole('combobox', { name: 'Monitor' }).click();
  await page.getByRole('option', { name: /^Public API/ }).click();
  await page.getByRole('button', { name: /Telegram/ }).click();
  await page.getByRole('button', { name: 'Create rule' }).click();
  await expect(page.getByText('Public API availability')).toBeVisible();
  expect(state.alertRules?.[0]?.monitor.notify_resolved).toBe(true);
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
  await page.getByRole('link', { name: 'Issues', exact: true }).click();
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
  await page.getByRole('link', { name: 'Issues', exact: true }).click();

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
  await page.getByRole('link', { name: 'Issues', exact: true }).click();

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
  await page.goto('/explore');
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
  await page.getByRole('link', { name: 'Dashboard', exact: true }).click();
  await expect(page.getByRole('heading', { name: 'Dashboard', exact: true })).toBeVisible();
  await page.getByRole('button', { name: 'Edit dashboard' }).click();
  const dashboardViewBox = await page.locator('.dashboard-section--views').boundingBox();
  const dashboardBuildersBox = await page.locator('.dashboard-builders').boundingBox();
  expect(dashboardViewBox).not.toBeNull();
  expect(dashboardBuildersBox).not.toBeNull();
  expect(
    dashboardBuildersBox!.y - (dashboardViewBox!.y + dashboardViewBox!.height),
  ).toBeGreaterThanOrEqual(20);

  await page.getByPlaceholder('Production log volume').fill('Production log volume');
  await page.getByRole('button', { name: 'Add widget' }).click();
  await expect(page.getByLabel('Saved query name')).toHaveValue('Production log volume');

  await page.getByPlaceholder('Service health').fill('Service health');
  const dashboardForm = page
    .locator('form')
    .filter({ has: page.getByPlaceholder('Service health') });
  await dashboardForm.getByRole('button', { name: 'Create dashboard' }).click();
  await expect(
    page.getByRole('heading', { name: 'Service health', exact: true, level: 2 }),
  ).toBeVisible();

  await page.getByPlaceholder('All environments').fill('production');
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(page.locator('.dashboard-widget__number')).toContainText('23');
  expect(state.dashboardVariableSeen).toBe(true);
  expect(state.csrfSeen).toBe(true);

  await page.getByRole('button', { name: 'Edit dashboard' }).click();
  await page.locator('.saved-query-list article').getByRole('button', { name: 'Delete' }).click();
  await page.getByRole('button', { name: 'Refresh' }).click();
  await expect(page.getByText('saved_query_missing')).toBeVisible();

  await page.locator('.dashboard-card').getByRole('button', { name: 'Delete' }).click();
  await expect(page.getByRole('heading', { name: 'No dashboard yet' })).toBeVisible();
});

test('mobile pagination stays outside horizontal data scrolling and dashboard cards do not overlap', async ({
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
  await page.setViewportSize({ width: 390, height: 844 });
  await login(page);
  await page.goto('/issues');

  const issueScroller = page.locator('.issue-table-scroll');
  const issuePagination = page.getByRole('navigation', { name: 'Results pages' });
  await expect(issuePagination).toBeVisible();
  const scrollRange = await issueScroller.evaluate(
    (element) => element.scrollWidth - element.clientWidth,
  );
  expect(scrollRange).toBeGreaterThan(0);
  const paginationBefore = await issuePagination.boundingBox();
  await issueScroller.evaluate((element) => {
    element.scrollLeft = element.scrollWidth;
  });
  const paginationAfter = await issuePagination.boundingBox();
  expect(paginationBefore).not.toBeNull();
  expect(paginationAfter).not.toBeNull();
  expect(paginationAfter!.x).toBeCloseTo(paginationBefore!.x, 0);

  for (const target of [
    { url: '/logs', label: 'Log result pages' },
    { url: '/traces', label: 'Transaction pages' },
    { url: '/feedback', label: 'Feedback result pages' },
  ]) {
    await page.goto(target.url);
    await expect(page.getByRole('navigation', { name: target.label })).toBeVisible();
    const widths = await page.evaluate(() => ({
      client: document.documentElement.clientWidth,
      scroll: document.documentElement.scrollWidth,
    }));
    expect(widths.scroll).toBe(widths.client);
  }

  await page.setViewportSize({ width: 1050, height: 900 });
  await page.goto('/dashboard?edit=1');
  const builders = page.locator('.dashboard-builder');
  await expect(builders).toHaveCount(2);
  const firstBuilder = await builders.nth(0).boundingBox();
  const secondBuilder = await builders.nth(1).boundingBox();
  expect(firstBuilder).not.toBeNull();
  expect(secondBuilder).not.toBeNull();
  expect(secondBuilder!.y).toBeGreaterThanOrEqual(firstBuilder!.y + firstBuilder!.height);

  const dashboardForm = builders.nth(1);
  const picker = await dashboardForm.getByRole('combobox').first().boundingBox();
  const addButton = await dashboardForm.getByRole('button', { name: 'Add' }).boundingBox();
  expect(picker).not.toBeNull();
  expect(addButton).not.toBeNull();
  const overlaps =
    picker!.x < addButton!.x + addButton!.width &&
    picker!.x + picker!.width > addButton!.x &&
    picker!.y < addButton!.y + addButton!.height &&
    picker!.y + picker!.height > addButton!.y;
  expect(overlaps).toBe(false);
});

test('Replay search and detail keep controls and metadata in their content flow', async ({
  page,
}) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
  };
  await installApi(page, state);
  await login(page);
  await page.goto('/replays');

  await page.getByLabel('Search loaded Replays').fill('manual-replay-demo');
  await page.getByRole('button', { name: 'Search', exact: true }).click();
  await expect(page.getByText('1 matching Replay for')).toBeVisible();
  await page.getByRole('link', { name: /example\.test\/replay/ }).click();

  await expect(page.getByRole('heading', { name: 'Session Replay', exact: true })).toBeVisible();
  await expect(page.locator('.replay-metadata-grid article')).toHaveCount(4);
  await expect(page.locator('.replay-metadata-grid')).toContainText('Complete');
  await expect(page.locator('.replay-metadata-grid')).toContainText('manual-replay-demo');
  const placeholder = page.locator('.replay-player-placeholder');
  await expect(placeholder).toContainText('Recording is not downloaded automatically');
  await expect(placeholder.getByRole('button', { name: 'Load recording' })).toBeVisible();
});

test('settings anchors, capability grid and project creation stay discoverable', async ({
  page,
}) => {
  const state: ApiState = {
    role: 'owner',
    csrfSeen: false,
    sessionCookieSeen: false,
    failIssues: false,
  };
  await installApi(page, state);
  await login(page);

  await page.getByRole('combobox', { name: 'Project' }).click();
  const createProject = page.getByRole('option', { name: /New project/ });
  await expect(createProject).toHaveClass(/base-select__option--action/);
  await createProject.click();
  await expect(page).toHaveURL(/\/projects\/new$/);
  await page.goto('/project/setup');
  await page.getByRole('link', { name: 'Manage keys' }).click();
  await expect(page).toHaveURL(/\/settings\/project#dsn-keys$/);
  await expect(page.locator('#dsn-keys')).toBeInViewport();

  await page.goto('/settings/system');
  const capabilities = page.locator('.capability-list article');
  await expect(capabilities).toHaveCount(4);
  const firstCapability = await capabilities.nth(0).boundingBox();
  const secondCapability = await capabilities.nth(1).boundingBox();
  expect(firstCapability).not.toBeNull();
  expect(secondCapability).not.toBeNull();
  expect(Math.abs(firstCapability!.y - secondCapability!.y)).toBeLessThan(2);
  expect(secondCapability!.x).toBeGreaterThan(firstCapability!.x);
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
    { name: 'replays', url: '/replays', heading: 'Session Replays' },
    { name: 'replay-detail', url: `/replays/${replayRecord.id}`, heading: 'Session Replay' },
    { name: 'explore', url: '/explore', heading: 'Unified Explore' },
    { name: 'dashboard', url: '/dashboard', heading: 'Dashboard' },
    { name: 'project-new', url: '/projects/new', heading: 'Create a new project' },
    { name: 'sdk-setup', url: '/project/setup', heading: 'Connect an SDK' },
    { name: 'project-settings', url: '/settings/project', heading: 'Project settings' },
    { name: 'notifications', url: '/settings/notifications', heading: 'Alerts' },
    { name: 'system-status', url: '/settings/system', heading: 'System status' },
  ];

  for (const route of routes) {
    await page.goto(route.url);
    await expect(page.getByRole('heading', { name: route.heading, exact: true })).toBeVisible();
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
