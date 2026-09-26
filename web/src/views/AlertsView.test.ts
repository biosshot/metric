import { QueryClient, VueQueryPlugin } from '@tanstack/vue-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import AlertsView from './AlertsView.vue';
import type { AlertRule, NotificationDestination } from '../api/types';

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
  deleteNotificationDestination: vi.fn(),
  deleteAlertRule: vi.fn(),
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
    vi.restoreAllMocks();
    vi.clearAllMocks();
    session.canAdminister = false;
    api.notificationDestinations.mockResolvedValue({ items: [] });
    api.alertRules.mockResolvedValue({ items: [] });
    api.monitors.mockResolvedValue({ items: [] });
    api.notificationDeliveries.mockResolvedValue({ items: [] });
    api.organizationMembers.mockResolvedValue({ items: [] });
    api.putAlertRule.mockResolvedValue({});
    api.deleteAlertRule.mockResolvedValue(undefined);
    api.deleteNotificationDestination.mockResolvedValue(undefined);
    api.putNotificationDestination.mockResolvedValue({});
  });

  function renderAdmin(): void {
    session.canAdminister = true;
    render(AlertsView, {
      global: {
        plugins: [
          [
            VueQueryPlugin,
            {
              queryClient: new QueryClient({
                defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
              }),
            },
          ],
        ],
      },
    });
  }

  function emailChannel(): NotificationDestination {
    return {
      id: 'a'.repeat(32),
      project_id: '42',
      kind: 'smtp_email',
      endpoint: 'smtp.gmail.com',
      has_secret: true,
      enabled: true,
      telegram: null,
      created_at: 1,
      updated_at: 2,
      smtp: {
        port: 587,
        security: 'starttls',
        username: 'sender@example.com',
        from: 'sender@example.com',
        recipients: ['first@example.com', 'second@example.com'],
      },
    };
  }

  function savedRule(): AlertRule {
    return {
      id: 'b'.repeat(32),
      project_id: '42',
      name: 'Existing rule',
      enabled: true,
      triggers: ['new_issue'],
      aggregate: null,
      monitor: null,
      destination_ids: ['a'.repeat(32)],
      cooldown_minutes: 7,
      storm_limit_per_hour: 23,
      created_at: 1,
      updated_at: 2,
    };
  }

  it('edits the existing rule and keeps its id, state and delivery limits', async () => {
    const rule = { ...savedRule(), enabled: false };
    api.notificationDestinations.mockResolvedValue({ items: [emailChannel()] });
    api.alertRules.mockResolvedValue({ items: [rule] });
    renderAdmin();
    await fireEvent.click(await screen.findByRole('button', { name: 'Edit rule' }));
    await fireEvent.update(screen.getByDisplayValue('Existing rule'), 'Updated rule');
    await fireEvent.click(screen.getByRole('button', { name: 'Save rule' }));
    await waitFor(() =>
      expect(api.putAlertRule).toHaveBeenCalledWith(
        '42',
        expect.objectContaining({
          id: rule.id,
          name: 'Updated rule',
          enabled: false,
          triggers: ['new_issue'],
          destination_ids: rule.destination_ids,
          cooldown_minutes: 7,
          storm_limit_per_hour: 23,
        }),
      ),
    );
  });

  it('toggles a rule without losing aggregate conditions and confirms deletion', async () => {
    const rule: AlertRule = {
      ...savedRule(),
      triggers: [],
      aggregate: {
        dataset: 'logs',
        lookback_minutes: 15,
        evaluation_interval_minutes: 5,
        threshold: 10,
        environment: 'production',
        release: 'v1',
        notify_resolved: false,
      },
    };
    api.notificationDestinations.mockResolvedValue({ items: [emailChannel()] });
    api.alertRules.mockResolvedValue({ items: [rule] });
    renderAdmin();
    await fireEvent.click(await screen.findByRole('button', { name: 'Disable rule' }));
    await waitFor(() =>
      expect(api.putAlertRule).toHaveBeenCalledWith(
        '42',
        expect.objectContaining({
          id: rule.id,
          enabled: false,
          aggregate_dataset: 'logs',
          environment: 'production',
          release: 'v1',
          notify_resolved: false,
          threshold: 10,
        }),
      ),
    );
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    await fireEvent.click(screen.getByRole('button', { name: 'Delete rule' }));
    expect(api.deleteAlertRule).not.toHaveBeenCalled();
    confirm.mockReturnValue(true);
    await fireEvent.click(screen.getByRole('button', { name: 'Delete rule' }));
    await waitFor(() => expect(api.deleteAlertRule).toHaveBeenCalledWith('42', rule.id));
  });

  it('removes an email address and saves the same channel without its SMTP password', async () => {
    const item = emailChannel();
    api.notificationDestinations.mockResolvedValue({ items: [item] });
    renderAdmin();
    await fireEvent.click(await screen.findByRole('button', { name: 'Edit email addresses' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Remove first@example.com' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Save addresses' }));
    await waitFor(() =>
      expect(api.putNotificationDestination).toHaveBeenCalledWith('42', {
        id: item.id,
        kind: 'smtp_email',
        endpoint: 'smtp.gmail.com',
        enabled: true,
        secret: null,
        smtp_port: 587,
        smtp_security: 'starttls',
        smtp_username: 'sender@example.com',
        smtp_from: 'sender@example.com',
        smtp_recipients: ['second@example.com'],
      }),
    );
  });

  it('deletes all recipients of the selected bot, including disabled ones, and leaves other bots', async () => {
    const telegram = (id: string, botId: string, enabled: boolean): NotificationDestination => ({
      ...emailChannel(),
      id,
      kind: 'telegram',
      enabled,
      smtp: null,
      endpoint: id,
      telegram: {
        api_base: 'https://api.telegram.org',
        message_thread_id: null,
        bot_id: botId,
        bot_username: `bot_${botId}`,
        bot_display_name: botId,
        chat_type: 'private',
        chat_username: null,
        chat_display_name: id,
      },
    });
    const items = [
      telegram('1'.repeat(32), '123', true),
      telegram('2'.repeat(32), '123', false),
      telegram('3'.repeat(32), '456', true),
    ];
    api.notificationDestinations.mockResolvedValue({ items });
    renderAdmin();
    const confirm = vi.spyOn(window, 'confirm').mockReturnValue(false);
    const botButton = (await screen.findAllByRole('button', { name: 'Delete bot' }))[0];
    await fireEvent.click(botButton);
    expect(api.deleteNotificationDestination).not.toHaveBeenCalled();
    confirm.mockReturnValue(true);
    await fireEvent.click(botButton);
    await waitFor(() => expect(api.deleteNotificationDestination).toHaveBeenCalledTimes(2));
    expect(api.deleteNotificationDestination).toHaveBeenNthCalledWith(1, '42', items[0].id);
    expect(api.deleteNotificationDestination).toHaveBeenNthCalledWith(2, '42', items[1].id);
    expect(confirm).toHaveBeenLastCalledWith(expect.stringContaining('2 recipients'));
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
