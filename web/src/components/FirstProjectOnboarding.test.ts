import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { render, screen } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import FirstProjectOnboarding from './FirstProjectOnboarding.vue';

const api = vi.hoisted(() => ({
  createProject: vi.fn(),
}));

vi.mock('../api/client', () => ({ api }));

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    projects: [{ id: '42' }],
    has: () => false,
    refreshProjects: vi.fn(),
    selectProject: vi.fn(),
  }),
}));

vi.mock('vue-router', () => ({
  useRouter: () => ({ push: vi.fn() }),
}));

describe('FirstProjectOnboarding authorization boundary', () => {
  beforeEach(() => vi.clearAllMocks());

  it('does not expose project creation to a member on the direct route', () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });

    render(FirstProjectOnboarding, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
        stubs: { RouterLink: { template: '<a><slot /></a>' } },
      },
    });

    expect(screen.getByText('Project creation is restricted')).toBeVisible();
    expect(screen.queryByRole('button', { name: /Create project/i })).not.toBeInTheDocument();
    expect(api.createProject).not.toHaveBeenCalled();
  });
});
