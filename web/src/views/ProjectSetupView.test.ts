import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { fireEvent, render, screen } from '@testing-library/vue';
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

function renderSetup() {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });

  return render(ProjectSetupView, {
    global: {
      plugins: [[VueQueryPlugin, { queryClient }]],
      stubs: {
        RouterLink: {
          props: ['to'],
          template: '<a :href="to"><slot /></a>',
        },
      },
    },
  });
}

describe('ProjectSetupView', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    permissions.clear();
    api.keys.mockResolvedValue({ items: [] });
  });

  it('does not request or expose DSN credentials and examples to a member', () => {
    const { container } = renderSetup();

    expect(screen.getByText('SDK setup is restricted')).toBeVisible();
    expect(api.keys).not.toHaveBeenCalled();
    expect(screen.queryByText('Available DSNs')).not.toBeInTheDocument();
    expect(screen.queryByText('Initialize the SDK')).not.toBeInTheDocument();
    expect(container.textContent).not.toContain('@localhost');
  });

  it('shows all seven supported SDKs to a project administrator', async () => {
    permissions.add('project:admin');
    api.keys.mockResolvedValue({
      items: [{ dsn_key: 'current-key', label: 'Default', state: 'active' }],
    });
    renderSetup();

    await screen.findByText('Installation');
    await fireEvent.click(screen.getByRole('combobox', { name: 'SDK' }));

    expect(screen.getAllByRole('option')).toHaveLength(7);
    for (const label of ['JavaScript Browser', 'Node.js', 'Python', 'Java', '.NET', 'Go', 'Rust']) {
      expect(screen.getByRole('option', { name: label })).toBeVisible();
    }
  });

  it('switches the installation and initialization examples with the SDK', async () => {
    permissions.add('project:admin');
    api.keys.mockResolvedValue({
      items: [{ dsn_key: 'current-key', label: 'Default', state: 'active' }],
    });
    const { container } = renderSetup();

    await screen.findByText('Installation');
    expect(container.textContent).toContain('npm install @sentry/browser@10.66.0');
    await fireEvent.click(screen.getByRole('combobox', { name: 'SDK' }));
    await fireEvent.click(screen.getByRole('option', { name: 'Rust' }));

    expect(container.textContent).toContain('cargo add sentry@0.48.5');
    expect(container.textContent).toContain('sentry::capture_message');
  });

  it('injects the current project DSN into the minimal example', async () => {
    permissions.add('project:admin');
    api.keys.mockResolvedValue({
      items: [{ dsn_key: 'current-key', label: 'Default', state: 'active' }],
    });
    const { container } = renderSetup();

    await screen.findByText('Minimal Error Event');
    const expectedDsn = `${window.location.protocol}//current-key@${window.location.host}/42`;
    expect(container.textContent).toContain(expectedDsn);
  });

  it('keeps the Browser example limited to a basic Error Event', async () => {
    permissions.add('project:admin');
    api.keys.mockResolvedValue({
      items: [{ dsn_key: 'current-key', label: 'Default', state: 'active' }],
    });
    const { container } = renderSetup();

    await screen.findByText('Minimal Error Event');
    expect(container.textContent).toContain('Sentry.captureMessage("Metric test event")');
    expect(container.textContent).not.toMatch(/replayIntegration/i);
    expect(container.textContent).not.toMatch(/tracing|tracesSampleRate|sampling/i);
  });

  it('links to Issues and the official documentation for the selected SDK', async () => {
    permissions.add('project:admin');
    api.keys.mockResolvedValue({
      items: [{ dsn_key: 'current-key', label: 'Default', state: 'active' }],
    });
    renderSetup();

    await screen.findByText('Installation');
    expect(screen.getByRole('link', { name: 'Open Issues' })).toHaveAttribute('href', '/issues');
    expect(screen.getByRole('link', { name: 'Sentry SDK documentation' })).toHaveAttribute(
      'href',
      'https://docs.sentry.io/platforms/javascript/',
    );

    await fireEvent.click(screen.getByRole('combobox', { name: 'SDK' }));
    await fireEvent.click(screen.getByRole('option', { name: 'Python' }));
    expect(screen.getByRole('link', { name: 'Sentry SDK documentation' })).toHaveAttribute(
      'href',
      'https://docs.sentry.io/platforms/python/',
    );
  });

  it('keeps examples hidden when the project has no active DSN key', async () => {
    permissions.add('project:admin');
    renderSetup();

    expect(await screen.findByText('No active DSN')).toBeVisible();
    expect(screen.queryByText('Installation')).not.toBeInTheDocument();
    expect(screen.queryByText('Minimal Error Event')).not.toBeInTheDocument();
  });
});
