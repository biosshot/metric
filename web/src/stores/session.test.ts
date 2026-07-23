import { createPinia, setActivePinia } from 'pinia';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api/client';
import { useSessionStore } from './session';

function memoryStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => values.delete(key),
    setItem: (key, value) => values.set(key, value),
  };
}

describe('session restoration', () => {
  beforeEach(() => {
    vi.stubGlobal('localStorage', memoryStorage());
    vi.stubGlobal('sessionStorage', memoryStorage());
    localStorage.setItem('faultkeep.organization', '7');
    setActivePinia(createPinia());
  });

  it('keeps an infrastructure failure visible instead of treating it as logout', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn().mockResolvedValue(
        new Response(
          JSON.stringify({
            error: {
              code: 'temporarily_unavailable',
              message: 'service is temporarily unavailable',
              request_id: 'restore-request-503',
            },
          }),
          { status: 503, headers: { 'content-type': 'application/json' } },
        ),
      ),
    );
    const session = useSessionStore();

    await session.restore();

    expect(session.restoreError).toBeInstanceOf(ApiError);
    expect(session.restoreError).toMatchObject({
      status: 503,
      requestId: 'restore-request-503',
    });
    expect(session.organizationId).toBe('7');
    expect(session.authenticated).toBe(false);
  });
});
