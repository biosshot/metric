import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { render, screen } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ReleaseDetailView from './ReleaseDetailView.vue';

const { api } = vi.hoisted(() => ({
  api: {
    organization: vi.fn(),
    release: vi.fn(),
    releaseDeploys: vi.fn(),
    releaseIssues: vi.fn(),
    releaseHealth: vi.fn(),
  },
}));

vi.mock('../api/client', () => ({ api }));
vi.mock('vue-router', () => ({
  useRoute: () => ({ params: { releaseId: 'release-1' }, query: {} }),
}));
vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    selectedProjectId: '42',
    selectedProject: { slug: 'backend' },
    has: () => false,
  }),
}));

function renderView() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(ReleaseDetailView, {
    global: {
      plugins: [[VueQueryPlugin, { queryClient }]],
      stubs: {
        RouterLink: {
          props: ['to'],
          template:
            '<a :href="typeof to === \'string\' ? to : to.path" :data-to="JSON.stringify(to)"><slot /></a>',
        },
      },
    },
  });
}

describe('ReleaseDetailView signal links', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.organization.mockResolvedValue({ slug: 'acme' });
    api.release.mockResolvedValue({
      id: 'release-1',
      version: 'metric-node-signals@1.1.0',
      first_seen: '2026-08-04T09:00:00Z',
      last_seen: '2026-08-04T10:00:00Z',
      released_at: '2026-08-04T10:00:00Z',
      repositories: [],
      reference: null,
      url: null,
    });
    api.releaseDeploys.mockResolvedValue({ items: [], next_cursor: null });
    api.releaseIssues.mockResolvedValue({ items: [], next_cursor: null });
    api.releaseHealth.mockResolvedValue({
      items: [],
      sessions: 0,
      users: 0,
      crashed_users: 0,
      crash_free_users: 100,
      approximate_users: true,
      user_sketch_bytes: 256,
      user_sketch_standard_error_percent: 1,
      user_sketch_saturation_estimate: 0,
    });
  });

  it('opens exact Release errors in Explore instead of unsupported Issue search', async () => {
    renderView();

    expect(await screen.findByRole('link', { name: 'Errors' })).toHaveAttribute(
      'data-to',
      JSON.stringify({
        path: '/explore',
        query: { q: 'rel:"metric-node-signals@1.1.0"' },
      }),
    );
  });
});
