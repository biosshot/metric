import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { render, screen, waitFor } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ExploreView from './ExploreView.vue';

const eventId = '11'.repeat(16);
const { api, route } = vi.hoisted(() => ({
  api: { query: vi.fn() },
  route: { query: { q: 'rel:"metric-node-signals@1.1.0"' } as Record<string, string> },
}));

vi.mock('../api/client', () => ({ api }));
vi.mock('vue-router', () => ({
  useRoute: () => route,
  useRouter: () => ({ replace: vi.fn() }),
}));
vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    selectedProjectId: '42',
    selectedProject: { slug: 'backend' },
  }),
}));

function renderView() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(ExploreView, {
    global: {
      plugins: [[VueQueryPlugin, { queryClient }]],
      stubs: {
        RouterLink: {
          props: ['to'],
          template: '<a :href="to.path" :data-to="JSON.stringify(to)"><slot /></a>',
        },
      },
    },
  });
}

describe('ExploreView record navigation', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    route.query = { q: 'rel:"metric-node-signals@1.1.0"' };
    api.query.mockResolvedValue({
      source: 'errors',
      kind: 'records',
      items: [
        {
          event_id: eventId,
          issue_id: '22'.repeat(16),
          level: 'error',
          platform: 'javascript',
        },
      ],
      next_cursor: null,
      normalized_query: 'rel:"metric-node-signals@1.1.0"',
      cost: 1,
    });
  });

  it('runs a deep-linked query and opens its Error event', async () => {
    renderView();

    await waitFor(() =>
      expect(api.query).toHaveBeenCalledWith(
        '42',
        expect.objectContaining({
          source: 'errors',
          query: 'rel:"metric-node-signals@1.1.0"',
          result: { kind: 'records' },
        }),
      ),
    );
    expect(await screen.findByRole('link', { name: 'Open result 1' })).toHaveAttribute(
      'data-to',
      JSON.stringify({ path: `/events/${eventId}` }),
    );
  });
});
