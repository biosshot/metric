import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { render, screen } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import OrganizationView from './OrganizationView.vue';

const api = vi.hoisted(() => ({
  organization: vi.fn(),
  organizationMembers: vi.fn(),
  organizationAudit: vi.fn(),
  inviteOrganizationMember: vi.fn(),
  updateOrganizationMember: vi.fn(),
}));

vi.mock('../api/client', () => ({ api }));

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    identity: {
      role: 'member',
      permissions: ['project:read', 'issue:read', 'issue:write'],
    },
    projects: [{ id: '42' }],
    has: () => false,
  }),
}));

vi.mock('./ApiTokensView.vue', () => ({
  default: { template: '<div>Personal API tokens</div>' },
}));

describe('OrganizationView authorization boundary', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.organization.mockResolvedValue({
      id: '7',
      slug: 'acme',
      display_name: 'Acme',
      created_at: '2030-01-01T00:00:00Z',
    });
  });

  it('loads the member summary without requesting organization administration data', async () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });

    render(OrganizationView, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
      },
    });

    expect(await screen.findByRole('heading', { name: 'Acme' })).toBeVisible();
    expect(screen.getByText('Personal API tokens')).toBeVisible();
    expect(api.organizationMembers).not.toHaveBeenCalled();
    expect(api.organizationAudit).not.toHaveBeenCalled();
    expect(screen.queryByRole('heading', { name: 'Members' })).not.toBeInTheDocument();
  });
});
