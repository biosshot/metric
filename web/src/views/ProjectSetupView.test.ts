import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { render, screen } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ProjectSetupView from './ProjectSetupView.vue';

const { api, permissions } = vi.hoisted(() => ({
  api: { keys: vi.fn() },
  permissions: new Set<string>(),
}));

vi.mock('../api/client', () => ({ api }));

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    selectedProjectId: '42',
    selectedProject: { display_name: 'Backend' },
    has: (permission: string) => permissions.has(permission),
  }),
}));

describe('ProjectSetupView authorization boundary', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    permissions.clear();
    api.keys.mockResolvedValue({ items: [] });
  });

  it('does not request DSN credentials for a member', () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });

    render(ProjectSetupView, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
        stubs: { RouterLink: { template: '<a><slot /></a>' } },
      },
    });

    expect(screen.getByText('SDK setup is restricted')).toBeVisible();
    expect(api.keys).not.toHaveBeenCalled();
    expect(screen.queryByText('Available DSNs')).not.toBeInTheDocument();
  });

  it('loads DSN credentials for a project administrator', async () => {
    permissions.add('project:admin');
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });

    render(ProjectSetupView, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
        stubs: { RouterLink: { template: '<a><slot /></a>' } },
      },
    });

    expect(await screen.findByText('Available DSNs')).toBeVisible();
    expect(api.keys).toHaveBeenCalledWith('42');
    expect(screen.queryByText('SDK setup is restricted')).not.toBeInTheDocument();
  });
});
