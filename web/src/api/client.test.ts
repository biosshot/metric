import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError, api, configureSession } from './client';

describe('native API client', () => {
  beforeEach(() => {
    configureSession(() => ({ organizationId: '7', csrfToken: 'a'.repeat(64) }));
  });

  it('sends organization, cookie and CSRF context for mutations', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal('fetch', fetchMock);

    await api.logout();

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = init.headers as Headers;
    expect(init.credentials).toBe('include');
    expect(headers.get('x-metric-organization-id')).toBe('7');
    expect(headers.get('x-csrf-token')).toBe('a'.repeat(64));
  });

  it('creates a scoped personal token through the authenticated CSRF contract', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          id: '11',
          token: 'f'.repeat(64),
          expires_at: '2030-02-01T23:59:59Z',
        }),
        { status: 201, headers: { 'content-type': 'application/json' } },
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    await api.createToken(
      'sentry-cli debug files',
      ['debug_file:read', 'debug_file:write'],
      '2030-02-01T23:59:59Z',
    );

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = init.headers as Headers;
    expect(path).toBe('/api/v1/auth/tokens');
    expect(init.method).toBe('POST');
    expect(headers.get('x-csrf-token')).toBe('a'.repeat(64));
    expect(JSON.parse(String(init.body))).toEqual({
      name: 'sentry-cli debug files',
      scopes: ['debug_file:read', 'debug_file:write'],
      expires_at: '2030-02-01T23:59:59Z',
    });
  });

  it('loads organization context with the authoritative organization header', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          id: '7',
          slug: 'acme',
          display_name: 'Acme',
          created_at: '2030-01-01T00:00:00Z',
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    await api.organization();

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/v1/organization');
    expect((init.headers as Headers).get('x-metric-organization-id')).toBe('7');
    expect((init.headers as Headers).get('x-csrf-token')).toBeNull();
  });

  it('updates one organization member action under CSRF protection', async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
    vi.stubGlobal('fetch', fetchMock);

    await api.updateOrganizationMember('42', 'change_role', 'admin');

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/v1/organization/members/42');
    expect(init.method).toBe('PATCH');
    expect((init.headers as Headers).get('x-csrf-token')).toBe('a'.repeat(64));
    expect(JSON.parse(String(init.body))).toEqual({
      action: 'change_role',
      role: 'admin',
    });
  });

  it('sends large organization IDs without JavaScript precision loss', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          csrf_token: 'b'.repeat(64),
          expires_at: '2030-01-01T00:00:00Z',
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    const organizationId = '9007199254740993';
    await api.login('owner@example.com', 'correct horse battery staple', organizationId);

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toMatchObject({
      organization_id: organizationId,
    });
  });

  it('allows sign-in without exposing an organization identifier', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          csrf_token: 'b'.repeat(64),
          expires_at: '2030-01-01T00:00:00Z',
          organization_id: '7',
        }),
        { status: 200, headers: { 'content-type': 'application/json' } },
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    await api.login('owner@example.com', 'correct horse battery staple');

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(JSON.parse(String(init.body))).toEqual({
      email: 'owner@example.com',
      password: 'correct horse battery staple',
    });
  });

  it('creates a project through the authenticated CSRF contract', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          project_id: '9007199254740993',
          dsn_key: 'd'.repeat(32),
        }),
        { status: 201, headers: { 'content-type': 'application/json' } },
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    await api.createProject({
      display_name: 'Payments API',
      slug: 'payments-api',
      ip_policy: 'hmac',
      error_enabled: true,
      client_report_enabled: true,
      log_enabled: true,
      transaction_enabled: true,
      span_enabled: true,
      feedback_enabled: true,
      check_in_enabled: true,
      metric_enabled: true,
      replay_enabled: false,
      max_event_bytes: 1_048_576,
      max_events_per_second: null,
      burst: null,
    });

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = init.headers as Headers;
    expect(path).toBe('/api/v1/projects');
    expect(init.method).toBe('POST');
    expect(headers.get('x-metric-organization-id')).toBe('7');
    expect(headers.get('x-csrf-token')).toBe('a'.repeat(64));
    expect(JSON.parse(String(init.body))).toMatchObject({
      display_name: 'Payments API',
      slug: 'payments-api',
      max_event_bytes: 1_048_576,
    });
  });

  it('can list projects for another authorized organization without changing global state', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ items: [] }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    await api.projects('99');

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect((init.headers as Headers).get('x-metric-organization-id')).toBe('99');
  });

  it('updates bounded inbound filters with the project policy revision', async () => {
    const policy = {
      revision: 3,
      ip_policy: 'hmac' as const,
      items: {
        error: true,
        client_report: true,
        log: true,
        transaction: true,
        span: true,
        feedback: true,
        check_in: true,
        metric: true,
        replay: false,
      },
      limits: {
        max_event_bytes: 1_048_576,
        max_events_per_second: null,
        burst: null,
      },
      inbound_filters: [
        {
          signal: 'error' as const,
          field: 'message' as const,
          operation: 'contains' as const,
          pattern: 'healthcheck',
        },
      ],
    };
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ ...policy, revision: 4 }), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    await api.updatePolicy('42', policy);

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe('/api/v1/projects/42/policy');
    expect(init.method).toBe('PATCH');
    expect(JSON.parse(String(init.body))).toMatchObject({
      expected_revision: 3,
      inbound_filters: policy.inbound_filters,
    });
  });

  it('connects Telegram subscribers without exposing chat IDs in the form', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({ id: '123', username: 'metric_alerts_bot', display_name: 'Metric' }),
          { status: 200, headers: { 'content-type': 'application/json' } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            bot: { id: '123', username: 'metric_alerts_bot', display_name: 'Metric' },
            subscribers: [{ destination_id: 'd'.repeat(32), display_name: 'On-call' }],
          }),
          { status: 200, headers: { 'content-type': 'application/json' } },
        ),
      );
    vi.stubGlobal('fetch', fetchMock);

    await api.checkTelegramBot('42', '123:bot-token');
    await api.syncTelegramSubscribers('42', '123:bot-token', 'pairing-code-1234');

    const [checkPath, checkInit] = fetchMock.mock.calls[0] as [string, RequestInit];
    const [syncPath, syncInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(checkPath).toBe('/api/v1/projects/42/notification-destinations/telegram/check');
    expect(syncPath).toBe('/api/v1/projects/42/notification-destinations/telegram/sync');
    expect((checkInit.headers as Headers).get('x-csrf-token')).toBe('a'.repeat(64));
    expect(JSON.parse(String(checkInit.body))).toEqual({ token: '123:bot-token' });
    expect(JSON.parse(String(syncInit.body))).toEqual({
      token: '123:bot-token',
      pairing_code: 'pairing-code-1234',
    });
  });

  it('scopes monitor history by time and protects monitor deletion with CSRF', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(JSON.stringify({ items: [] }), {
          status: 200,
          headers: { 'content-type': 'application/json' },
        }),
      )
      .mockResolvedValueOnce(new Response(null, { status: 204 }));
    vi.stubGlobal('fetch', fetchMock);

    await api.monitorRuns(
      '42',
      'b'.repeat(32),
      { from: 1_000, until: 2_000 },
      { cursor: 'next-page', limit: 100 },
    );
    await api.deleteMonitor('42', 'b'.repeat(32));

    const [historyPath] = fetchMock.mock.calls[0] as [string, RequestInit];
    const [deletePath, deleteInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    const historyUrl = new URL(historyPath, 'http://metric.test');
    expect(historyUrl.pathname).toBe(`/api/v1/projects/42/monitors/${'b'.repeat(32)}/runs`);
    expect(historyUrl.searchParams.get('from')).toBe('1970-01-01T00:00:01.000Z');
    expect(historyUrl.searchParams.get('until')).toBe('1970-01-01T00:00:02.000Z');
    expect(historyUrl.searchParams.get('cursor')).toBe('next-page');
    expect(historyUrl.searchParams.get('limit')).toBe('100');
    expect(deletePath).toBe(`/api/v1/projects/42/monitors/${'b'.repeat(32)}`);
    expect(deleteInit.method).toBe('DELETE');
    expect((deleteInit.headers as Headers).get('x-csrf-token')).toBe('a'.repeat(64));
  });

  it('refuses a mutation when this tab lost its CSRF token', async () => {
    const fetchMock = vi.fn();
    const invalidate = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    configureSession(() => ({ organizationId: '7', csrfToken: null }), invalidate);

    await expect(api.logout()).rejects.toMatchObject({
      code: 'csrf_missing',
      status: 403,
    });
    expect(fetchMock).not.toHaveBeenCalled();
    expect(invalidate).toHaveBeenCalledOnce();
  });

  it('invalidates expired sessions without treating an ordinary forbidden response as logout', async () => {
    const invalidate = vi.fn();
    configureSession(() => ({ organizationId: '7', csrfToken: 'a'.repeat(64) }), invalidate);
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            error: { code: 'invalid_credentials', message: 'expired', request_id: 'expired-1' },
          }),
          { status: 401, headers: { 'content-type': 'application/json' } },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            error: { code: 'forbidden', message: 'forbidden', request_id: 'forbidden-1' },
          }),
          { status: 403, headers: { 'content-type': 'application/json' } },
        ),
      );
    vi.stubGlobal('fetch', fetchMock);

    await expect(api.projects()).rejects.toMatchObject({ status: 401 });
    expect(invalidate).toHaveBeenCalledOnce();
    await expect(api.projects()).rejects.toMatchObject({ status: 403, code: 'forbidden' });
    expect(invalidate).toHaveBeenCalledOnce();
  });

  it('downloads Feedback attachments with cookie and organization context', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response('attachment body', {
        status: 200,
        headers: { 'content-type': 'text/plain' },
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    const blob = await api.feedbackAttachment('42', 'f'.repeat(32), 'a'.repeat(32));

    expect(blob.size).toBe(15);
    expect(blob.type).toBe('text/plain');
    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(path).toBe(
      `/api/v1/projects/42/feedback/${'f'.repeat(32)}/attachments/${'a'.repeat(32)}`,
    );
    expect(init.credentials).toBe('include');
    expect((init.headers as Headers).get('x-metric-organization-id')).toBe('7');
    expect((init.headers as Headers).get('accept')).toBe('application/octet-stream');
  });

  it('sends an explicit confirmation and idempotency key for project deletion', async () => {
    const response = {
      operation_id: 'b'.repeat(32),
      project_id: '42',
      organization_id: '7',
      phase: 'pending_grace',
      dataset_code: 10,
      reconciliation_pass: false,
      requested_at: '2030-01-01T00:00:00Z',
      purge_after: '2030-01-02T00:00:00Z',
      completed_at: null,
      next_attempt_at: '2030-01-02T00:00:00Z',
      attempts: 0,
      last_error: null,
      status_url: '/api/v1/projects/42/deletion',
    };
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(JSON.stringify(response), {
        status: 202,
        headers: { 'content-type': 'application/json' },
      }),
    );
    vi.stubGlobal('fetch', fetchMock);

    await api.requestProjectDeletion('42', 'backend', 'b'.repeat(32));

    const [path, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const headers = init.headers as Headers;
    expect(path).toBe('/api/v1/projects/42');
    expect(init.method).toBe('DELETE');
    expect(headers.get('idempotency-key')).toBe('b'.repeat(32));
    expect(headers.get('x-csrf-token')).toBe('a'.repeat(64));
    expect(JSON.parse(String(init.body))).toEqual({ confirm_slug: 'backend' });
  });

  it('keeps status, stable code and request ID visible', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: {
              code: 'temporarily_unavailable',
              message: 'service is temporarily unavailable',
              request_id: 'request-42',
            },
          }),
          { status: 503, headers: { 'content-type': 'application/json' } },
        ),
      ),
    );

    const error = await api.status().catch((cause: unknown) => cause);
    expect(error).toBeInstanceOf(ApiError);
    expect(error).toMatchObject({
      status: 503,
      code: 'temporarily_unavailable',
      requestId: 'request-42',
      retryable: true,
    });
  });

  it('reports a non-JSON proxy failure without hiding its HTTP status', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(new Response('<h1>Bad gateway</h1>', { status: 502 })),
    );

    await expect(api.status()).rejects.toMatchObject({
      status: 502,
      code: 'http_502',
      message: 'Metric returned HTTP 502 without a recognized error.',
    });
  });
});
