import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
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
  disableNotificationDestination: vi.fn(),
  restoreNotificationDestination: vi.fn(),
  putAlertRule: vi.fn(),
  testNotificationDestination: vi.fn(),
}));
const session = vi.hoisted(() => ({ canAdminister: false }));

vi.mock('../api/client', () => ({ api }));

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    selectedProjectId: '42',
    has: () => session.canAdminister,
  }),
}));

describe('AlertsView', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    session.canAdminister = false;
    api.notificationDestinations.mockResolvedValue({ items: [] });
    api.alertRules.mockResolvedValue({ items: [] });
    api.monitors.mockResolvedValue({ items: [] });
    api.notificationDeliveries.mockResolvedValue({ items: [] });
    api.organizationMembers.mockResolvedValue({ items: [] });
  });

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

  it('adds many Telegram targets through manual input or one-click discovery', async () => {
    session.canAdminister = true;
    api.checkTelegramBot.mockResolvedValue({
      id: '123',
      username: 'metric_alerts_bot',
      display_name: 'Metric',
      api_base: 'http://telegram.test/proxy',
    });
    api.putNotificationDestination.mockResolvedValue({
      id: 'd'.repeat(32),
      project_id: '42',
      kind: 'telegram',
      endpoint: '-1001234567890',
      has_secret: true,
      telegram: {
        api_base: 'http://telegram.test/proxy',
        message_thread_id: 73,
        bot_id: '123',
        bot_username: 'metric_alerts_bot',
        bot_display_name: 'Metric',
        chat_type: 'supergroup',
        chat_username: 'on_call',
        chat_display_name: 'On-call',
      },
      smtp: null,
      enabled: true,
      created_at: 1,
      updated_at: 1,
    });
    api.syncTelegramSubscribers.mockResolvedValue({
      bot: {
        id: '123',
        username: 'metric_alerts_bot',
        display_name: 'Metric',
        api_base: 'http://telegram.test/proxy',
      },
      next_offset: 124,
      subscribers: [
        {
          destination_id: 'e'.repeat(32),
          display_name: 'On-call topic',
          chat_id: '-1001234567890',
          message_thread_id: 73,
        },
      ],
    });
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(AlertsView, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
      },
    });

    await screen.findByText('Add a delivery channel');
    await fireEvent.update(
      screen.getByPlaceholderText('https://api.telegram.org'),
      'http://telegram.test/proxy',
    );
    await fireEvent.update(
      screen.getByPlaceholderText('123456:bot-token'),
      '123:abcdefghijklmnopqrstuvwxyz',
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Connect bot' }));
    await waitFor(() =>
      expect(api.checkTelegramBot).toHaveBeenCalledWith(
        '42',
        '123:abcdefghijklmnopqrstuvwxyz',
        'http://telegram.test/proxy',
        null,
      ),
    );

    await fireEvent.update(
      await screen.findByPlaceholderText('-1001234567890 or @channel'),
      '@on_call',
    );
    await fireEvent.update(screen.getByPlaceholderText('42'), '73');
    await fireEvent.click(screen.getByRole('button', { name: 'Save Telegram destination' }));
    await waitFor(() =>
      expect(api.putNotificationDestination).toHaveBeenCalledWith('42', {
        kind: 'telegram',
        endpoint: '@on_call',
        secret: '123:abcdefghijklmnopqrstuvwxyz',
        enabled: true,
        telegram_api_base: 'http://telegram.test/proxy',
        telegram_source_destination_id: null,
        telegram_message_thread_id: 73,
      }),
    );
    expect(screen.getByText(/same bot can serve any number of destinations/i)).toBeVisible();

    await fireEvent.click(screen.getByRole('button', { name: 'Find automatically' }));
    await waitFor(() => expect(api.syncTelegramSubscribers).toHaveBeenCalledTimes(1));
    const [, , apiBase, pairingCode, offset, sourceDestinationId] =
      api.syncTelegramSubscribers.mock.calls[0];
    expect(apiBase).toBe('http://telegram.test/proxy');
    expect(pairingCode).toMatch(/^[a-f0-9]{24}$/);
    expect(offset).toBeNull();
    expect(sourceDestinationId).toBeNull();
  });

  it.each([
    { label: 'missing configuration', telegram: undefined },
    { label: 'null configuration', telegram: null },
    {
      label: 'migrated configuration without identity snapshots',
      telegram: {
        api_base: 'https://api.telegram.org',
        message_thread_id: null,
        bot_id: null,
        bot_username: null,
        bot_display_name: null,
        chat_type: null,
        chat_username: null,
        chat_display_name: null,
      },
    },
  ])('labels a Telegram recipient by chat ID with $label', async ({ telegram }) => {
    session.canAdminister = true;
    api.notificationDestinations.mockResolvedValue({
      items: [
        {
          id: 'd'.repeat(32),
          project_id: '42',
          kind: 'telegram',
          endpoint: '-1001234567890',
          has_secret: true,
          telegram,
          smtp: null,
          enabled: true,
          created_at: 1,
          updated_at: 1,
        },
      ],
    });
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(AlertsView, {
      global: { plugins: [[VueQueryPlugin, { queryClient }]] },
    });

    expect(await screen.findByRole('button', { name: /^-1001234567890/ })).toBeVisible();
    expect(screen.queryByRole('button', { name: /SMTP/ })).not.toBeInTheDocument();
  });

  it('restores a saved bot after reload and lets an operator disable its recipient', async () => {
    session.canAdminister = true;
    const destinationId = 'd'.repeat(32);
    api.notificationDestinations.mockResolvedValue({
      items: [
        {
          id: destinationId,
          project_id: '42',
          kind: 'telegram',
          endpoint: '5663',
          has_secret: true,
          telegram: {
            api_base: 'https://api.telegram.org',
            message_thread_id: null,
            bot_id: '123',
            bot_username: 'metric_alerts_bot',
            bot_display_name: 'Metric',
            chat_type: 'private',
            chat_username: 'kirill_kosenko',
            chat_display_name: 'Kirill Kosenko',
          },
          smtp: null,
          enabled: true,
          created_at: 1,
          updated_at: 2,
        },
      ],
    });
    api.disableNotificationDestination.mockResolvedValue(undefined);
    api.putNotificationDestination.mockResolvedValue({
      id: 'e'.repeat(32),
      project_id: '42',
      kind: 'telegram',
      endpoint: '-1001234567890',
      has_secret: true,
      telegram: {
        api_base: 'https://api.telegram.org',
        message_thread_id: null,
        bot_id: '123',
        bot_username: 'metric_alerts_bot',
        bot_display_name: 'Metric',
        chat_type: 'supergroup',
        chat_username: 'on_call',
        chat_display_name: 'On-call',
      },
      smtp: null,
      enabled: true,
      created_at: 3,
      updated_at: 3,
    });
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(AlertsView, {
      global: {
        plugins: [[VueQueryPlugin, { queryClient }]],
      },
    });

    expect((await screen.findAllByText('@kirill_kosenko')).length).toBeGreaterThan(0);
    expect(screen.getByText('@metric_alerts_bot')).toBeVisible();
    expect(screen.queryByPlaceholderText('123456:bot-token')).not.toBeInTheDocument();

    await fireEvent.update(screen.getByPlaceholderText('-1001234567890 or @channel'), '@on_call');
    await fireEvent.click(screen.getByRole('button', { name: 'Save Telegram destination' }));
    await waitFor(() =>
      expect(api.putNotificationDestination).toHaveBeenCalledWith('42', {
        kind: 'telegram',
        endpoint: '@on_call',
        secret: null,
        enabled: true,
        telegram_api_base: null,
        telegram_source_destination_id: destinationId,
        telegram_message_thread_id: null,
      }),
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Disable' }));
    await waitFor(() =>
      expect(api.disableNotificationDestination).toHaveBeenCalledWith('42', destinationId),
    );
  });
});
