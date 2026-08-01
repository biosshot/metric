import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { render, screen } from '@testing-library/vue';
import { describe, expect, it, vi } from 'vitest';
import OrganizationCreateView from './OrganizationCreateView.vue';

const api = vi.hoisted(() => ({ createOrganization: vi.fn() }));

vi.mock('../api/client', () => ({ api }));
vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    has: () => false,
    refreshOrganizations: vi.fn(),
    selectOrganization: vi.fn(),
  }),
}));
vi.mock('vue-router', () => ({ useRouter: () => ({ replace: vi.fn() }) }));

describe('OrganizationCreateView authorization boundary', () => {
  it('does not expose organization creation to a member on the direct route', () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });

    render(OrganizationCreateView, {
      global: { plugins: [[VueQueryPlugin, { queryClient }]] },
    });

    expect(screen.getByText('Organization creation is restricted')).toBeVisible();
    expect(screen.queryByRole('button', { name: /Create organization/i })).not.toBeInTheDocument();
    expect(api.createOrganization).not.toHaveBeenCalled();
  });
});
