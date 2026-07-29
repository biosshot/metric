import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ApiTokensView from './ApiTokensView.vue';

const { createToken } = vi.hoisted(() => ({
  createToken: vi.fn(async () => ({
    id: 'token-1',
    token: 'secret-token',
    expires_at: '2030-02-01T23:59:59Z',
  })),
}));

vi.mock('../api/client', () => ({
  api: {
    tokens: vi.fn(async () => ({ items: [] })),
    createToken,
    revokeToken: vi.fn(),
  },
}));

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    has: (permission: string) =>
      new Set([
        'event:read',
        'issue:read',
        'issue:write',
        'project:read',
        'debug_file:read',
        'artifact:read',
        'release:read',
      ]).has(permission),
  }),
}));

describe('ApiTokensView role-scoped profiles', () => {
  beforeEach(() => vi.clearAllMocks());

  it('creates a member token without requesting administrative write scopes', async () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(ApiTokensView, {
      props: { embedded: true },
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
      },
    });

    expect(await screen.findByText('Create Issue automation token')).toBeVisible();
    const capability = screen.getByRole('combobox', { name: 'CLI capability' });
    expect(capability).toHaveTextContent('Issue automation');
    await fireEvent.click(capability);
    expect(screen.queryByText('Releases and deploys')).not.toBeInTheDocument();
    expect(screen.queryByText('Debug files')).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Create token' }));

    await waitFor(() => {
      expect(createToken).toHaveBeenCalledWith(
        'issue automation',
        ['event:read', 'issue:read', 'issue:write', 'project:read'],
        expect.stringMatching(/T23:59:59Z$/),
      );
    });
  });
});
