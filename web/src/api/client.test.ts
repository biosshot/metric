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
    expect(headers.get('x-faultkeep-organization-id')).toBe('7');
    expect(headers.get('x-csrf-token')).toBe('a'.repeat(64));
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

  it('refuses a mutation when this tab lost its CSRF token', async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    configureSession(() => ({ organizationId: '7', csrfToken: null }));

    await expect(api.logout()).rejects.toMatchObject({
      code: 'csrf_missing',
      status: 403,
    });
    expect(fetchMock).not.toHaveBeenCalled();
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
      message: 'Faultkeep returned HTTP 502 without a recognized error.',
    });
  });
});
