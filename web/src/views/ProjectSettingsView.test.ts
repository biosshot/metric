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

const { api, permissions } = vi.hoisted(() => ({
  api: {
    project: vi.fn(),
    keys: vi.fn(),
    capabilities: vi.fn(),
    projectDeletionStatus: vi.fn(),
    updatePolicy: vi.fn(),
    createKey: vi.fn(),
    disableKey: vi.fn(),
    requestProjectDeletion: vi.fn(),
    cancelProjectDeletion: vi.fn(),
  },
  permissions: new Set<string>(),
}));

vi.mock('../api/client', () => ({ api }));

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    selectedProjectId: '42',
    selectedProject: project,
    has: (permission: string) => permissions.has(permission),
    refreshProjects: vi.fn(),
  }),
}));

vi.mock('vue-router', () => ({
  useRoute: () => ({ hash: '' }),
}));

describe('ProjectSettingsView inbound filters', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    permissions.clear();
    permissions.add('project:admin');
    api.project.mockResolvedValue(project);
    api.keys.mockResolvedValue({ items: [] });
    api.capabilities.mockResolvedValue({ retention: null });
  });

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

  it('keeps member project settings read-only without requesting administrative resources', async () => {
    permissions.delete('project:admin');
    const memberProject = {
      ...project,
      state: 'pending_delete' as const,
    };
    api.project.mockResolvedValue(memberProject);
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });

    render(ProjectSettingsView, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
      },
    });

    expect(await screen.findByText('DSN key access is restricted')).toBeVisible();
    expect(screen.getByText(/only a project administrator can change them/i)).toBeVisible();
    expect(api.keys).not.toHaveBeenCalled();
    expect(api.projectDeletionStatus).not.toHaveBeenCalled();
    expect(screen.queryByRole('button', { name: 'Save policy' })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Create key' })).not.toBeInTheDocument();
  });
});
