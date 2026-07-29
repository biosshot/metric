import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { render, screen } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import AlertsView from './AlertsView.vue';

const api = vi.hoisted(() => ({
  notificationDestinations: vi.fn(),
  alertRules: vi.fn(),
  monitors: vi.fn(),
  notificationDeliveries: vi.fn(),
  organizationMembers: vi.fn(),
  putNotificationDestination: vi.fn(),
  checkTelegramBot: vi.fn(),
  syncTelegramSubscribers: vi.fn(),
  putAlertRule: vi.fn(),
  testNotificationDestination: vi.fn(),
}));

vi.mock('../api/client', () => ({ api }));

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    selectedProjectId: '42',
    has: () => false,
  }),
}));

describe('AlertsView authorization boundary', () => {
  beforeEach(() => vi.clearAllMocks());

  it('does not request administrative notification data for a member', () => {
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(AlertsView, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
      },
    });

    expect(screen.getByText('Alert administration is restricted')).toBeVisible();
    expect(screen.queryByText('Alert configuration was not loaded')).not.toBeInTheDocument();
    expect(api.notificationDestinations).not.toHaveBeenCalled();
    expect(api.alertRules).not.toHaveBeenCalled();
    expect(api.monitors).not.toHaveBeenCalled();
    expect(api.notificationDeliveries).not.toHaveBeenCalled();
    expect(api.organizationMembers).not.toHaveBeenCalled();
  });
});
