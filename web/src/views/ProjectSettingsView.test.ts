import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import ProjectSettingsView from './ProjectSettingsView.vue';

const project = {
  id: '42',
  organization_id: '7',
  slug: 'backend',
  display_name: 'Backend',
  state: 'active' as const,
  policy: {
    revision: 3,
    ip_policy: 'hmac' as const,
    items: {
      error: true,
      client_report: true,
      log: true,
      transaction: true,
      span: true,
    },
    limits: {
      max_event_bytes: 1_048_576,
      max_events_per_second: null,
      burst: null,
    },
    inbound_filters: [],
  },
  grouping_revision: 1,
  created_at: '2030-01-01T00:00:00Z',
};

vi.mock('../api/client', () => ({
  api: {
    project: vi.fn(async () => project),
    keys: vi.fn(async () => ({ items: [] })),
    capabilities: vi.fn(async () => ({ retention: null })),
    projectDeletionStatus: vi.fn(),
    updatePolicy: vi.fn(),
    createKey: vi.fn(),
    disableKey: vi.fn(),
    requestProjectDeletion: vi.fn(),
    cancelProjectDeletion: vi.fn(),
  },
}));

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    selectedProjectId: '42',
    selectedProject: project,
    has: (permission: string) => permission === 'project:admin',
    refreshProjects: vi.fn(),
  }),
}));

vi.mock('vue-router', () => ({
  useRoute: () => ({ hash: '' }),
}));

describe('ProjectSettingsView inbound filters', () => {
  beforeEach(() => vi.clearAllMocks());

  it('renders the durable-storage warning and adds a bounded rule editor', async () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(ProjectSettingsView, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
      },
    });

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Inbound filters' })).toBeVisible();
    });
    expect(
      screen.getByText(/discarded before MongoDB, attachments, or BlobStore writes/i),
    ).toBeVisible();
    expect(screen.getByText(/No inbound filters/i)).toBeVisible();

    await fireEvent.click(screen.getByRole('button', { name: 'Add filter' }));

    expect(screen.getByLabelText('Pattern')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Remove inbound filter' })).toBeVisible();
    expect(screen.getByText(/Up to 32 rules, 256 bytes per pattern/i)).toBeVisible();
  });
});
