import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { render, screen } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import EventDetailView from './EventDetailView.vue';

const traceId = '0123456789abcdef0123456789abcdef';
const replayId = 'fedcba9876543210fedcba9876543210';
const { api, state } = vi.hoisted(() => ({
  api: { event: vi.fn() },
  state: { replayEnabled: true },
}));

vi.mock('../api/client', () => ({ api }));
vi.mock('vue-router', () => ({
  useRoute: () => ({ params: { eventId: 'event-1' }, query: {} }),
}));
vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    selectedProjectId: '42',
    selectedProject: {
      policy: {
        items: {
          get replay() {
            return state.replayEnabled;
          },
        },
      },
    },
  }),
}));

function renderView() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(EventDetailView, {
    global: {
      plugins: [[VueQueryPlugin, { queryClient }]],
      stubs: {
        RouterLink: {
          props: ['to'],
          template:
            '<a :href="typeof to === \'string\' ? to : to.path" :data-to="JSON.stringify(to)"><slot /></a>',
        },
        StackTrace: true,
      },
    },
  });
}

describe('EventDetailView relations', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    state.replayEnabled = true;
    api.event.mockResolvedValue({
      event_id: 'event-1',
      project_id: '42',
      issue_id: 'issue-1',
      received_at: '2026-08-02T09:20:00Z',
      occurred_at: '2026-08-02T09:20:00Z',
      level: 'error',
      platform: 'javascript',
      body: {
        message: 'Metric test event',
        contexts: { trace: { trace_id: traceId }, replay: { replay_id: replayId } },
        release: 'backend@2.0',
        environment: 'production',
        user: { id: 'user-42' },
      },
    });
  });

  it('renders exact relation route objects when Replay is enabled', async () => {
    renderView();

    expect(await screen.findByRole('heading', { name: 'Related data' })).toBeVisible();
    expect(screen.getByRole('link', { name: /Open trace/ })).toHaveAttribute(
      'data-to',
      JSON.stringify({ path: `/traces/${traceId}` }),
    );
    expect(screen.getByRole('link', { name: /Open replay/ })).toHaveAttribute(
      'data-to',
      JSON.stringify({ path: `/replays/${replayId}` }),
    );
    expect(screen.getByRole('link', { name: /View release/ })).toHaveAttribute(
      'data-to',
      JSON.stringify({ path: '/releases', query: { q: 'rel:"backend@2.0"' } }),
    );
    expect(screen.getByRole('link', { name: /Other errors from user/ })).toHaveAttribute(
      'data-to',
      JSON.stringify({ path: '/explore', query: { q: 'user:"user-42"' } }),
    );
  });

  it('does not link Replay when the project capability is disabled', async () => {
    state.replayEnabled = false;
    renderView();

    await screen.findByRole('heading', { name: 'Related data' });
    expect(screen.queryByRole('link', { name: /Open replay/ })).not.toBeInTheDocument();
    expect(screen.getByText('Replay disabled')).toBeVisible();
  });
});
