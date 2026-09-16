import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ApiTokensView from './ApiTokensView.vue';

const { createToken, permissions } = vi.hoisted(() => ({
  createToken: vi.fn(async () => ({
    id: 'token-1',
    token: 'secret-token',
    expires_at: '2030-02-01T23:59:59Z',
  })),
  permissions: new Set<string>(),
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
    has: (permission: string) => permissions.has(permission),
  }),
}));

function renderView(): void {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  render(ApiTokensView, {
    props: { embedded: true },
    global: {
      plugins: [[VueQueryPlugin, { queryClient }]],
    },
  });
}

describe('ApiTokensView role-scoped profiles', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    permissions.clear();
    for (const permission of [
      'event:read',
      'issue:read',
      'issue:write',
      'project:read',
      'debug_file:read',
      'artifact:read',
      'release:read',
    ]) {
      permissions.add(permission);
    }
  });

  it('creates a member token without requesting administrative write scopes', async () => {
    renderView();

    expect(await screen.findByText('Create Issue automation token')).toBeVisible();
    const capability = screen.getByRole('combobox', { name: 'CLI capability' });
    expect(capability).toHaveTextContent('Issue automation');
    await fireEvent.click(capability);
    expect(screen.queryByText('Releases and deploys')).not.toBeInTheDocument();
    expect(screen.queryByText('Debug files')).not.toBeInTheDocument();
    expect(screen.queryByText('Sentry CLI uploads')).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Create token' }));

    await waitFor(() => {
      expect(createToken).toHaveBeenCalledWith(
        'issue automation',
        ['event:read', 'issue:read', 'issue:write', 'project:read'],
        expect.stringMatching(/T23:59:59Z$/),
      );
    });
  });

  it('offers a least-privilege Sentry CLI upload preset with artifact write access', async () => {
    permissions.add('debug_file:write');
    permissions.add('artifact:write');
    permissions.add('release:write');
    renderView();

    const capability = await screen.findByRole('combobox', { name: 'CLI capability' });
    await fireEvent.click(capability);
    await fireEvent.click(screen.getByRole('option', { name: 'Sentry CLI uploads' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Create token' }));

    await waitFor(() => {
      expect(createToken).toHaveBeenCalledWith(
        'sentry-cli uploads',
        ['debug_file:read', 'debug_file:write', 'artifact:read', 'artifact:write'],
        expect.stringMatching(/T23:59:59Z$/),
      );
    });
  });

  it('creates custom tokens from only the current users safe token scopes', async () => {
    for (const permission of [
      'project:admin',
      'debug_file:write',
      'debug_file:delete',
      'artifact:write',
      'artifact:delete',
      'release:write',
      'incident:export',
      'organization:admin',
      'organization:owner',
      'organization:delete',
    ]) {
      permissions.add(permission);
    }
    renderView();

    const capability = await screen.findByRole('combobox', { name: 'CLI capability' });
    expect(capability).toHaveTextContent('Releases and deploys');
    await fireEvent.click(capability);
    await fireEvent.click(screen.getByRole('option', { name: 'Custom / Advanced' }));

    expect(screen.queryByText('organization:owner')).not.toBeInTheDocument();
    expect(screen.queryByText('organization:delete')).not.toBeInTheDocument();

    const artifactWrite = screen.getByText('artifact:write').closest('label');
    expect(artifactWrite).toHaveAttribute(
      'title',
      'Upload and assemble source maps and artifact bundles.',
    );

    await fireEvent.click(screen.getByRole('checkbox', { name: /artifact:write/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Create token' }));

    await waitFor(() => {
      expect(createToken).toHaveBeenCalledWith(
        'custom API token',
        ['release:read', 'release:write', 'artifact:write'],
        expect.stringMatching(/T23:59:59Z$/),
      );
    });
  });
});
