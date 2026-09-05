<script setup lang="ts">
import { computed, onBeforeUnmount, reactive, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import type { NotificationDestination, TelegramBot } from '../api/types';
import { useSessionStore } from '../stores/session';

interface AvailableTelegramBot {
  key: string;
  source_destination_id: string;
  id: string | null;
  username: string | null;
  display_name: string;
  api_base: string;
  active_destinations: number;
  total_destinations: number;
  updated_at: number;
}

const session = useSessionStore();
const queryClient = useQueryClient();
const { t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const canAdminister = computed(() => session.has('project:admin'));
const kind = ref('telegram');
const ruleName = ref('');
const ruleKind = ref('issue');
const selectedDestinations = ref<string[]>([]);
const selectedMemberIds = ref<string[]>([]);
const memberSelectionTouched = ref(false);
const telegramBot = ref<TelegramBot | null>(null);
const telegramSourceDestinationId = ref('');
const telegramPairingCode = ref(createPairingCode());
const telegramSyncNotice = ref('');
const telegramDiscoveryActive = ref(false);
const telegramDiscoveryOffset = ref<number | null>(null);
let telegramDiscoveryDeadline = 0;
let telegramDiscoveryTimer: ReturnType<typeof setTimeout> | undefined;
let telegramDiscoveryGeneration = 0;
const triggers = reactive({ new_issue: true, regression: true, resolved: false });
const monitorRule = reactive({
  monitor_id: '',
  error: true,
  timeout: true,
  missed: true,
  notify_resolved: true,
});
const aggregateRule = reactive({
  dataset: 'errors',
  lookback_minutes: 15,
  evaluation_interval_minutes: 5,
  threshold: 100,
  environment: '',
  release: '',
  notify_resolved: true,
  cooldown_minutes: 10,
  storm_limit_per_hour: 12,
});
const destination = reactive({
  endpoint: '',
  secret: '',
  enabled: true,
  telegram_api_base: 'https://api.telegram.org',
  telegram_chat_id: '',
  telegram_message_thread_id: '' as string | number,
  smtp_port: 587,
  smtp_security: 'starttls',
  smtp_username: '',
  smtp_from: '',
  smtp_recipients: '',
});

const kindOptions = computed<SelectOption[]>(() => [
  {
    value: 'telegram',
    label: t('alerts.telegram'),
    description: t('alerts.telegramHelp'),
    icon: 'telegram',
  },
  {
    value: 'smtp_email',
    label: t('alerts.email'),
    description: t('alerts.emailHelp'),
    icon: 'email',
  },
]);
const securityOptions = computed<SelectOption[]>(() => [
  { value: 'starttls', label: 'STARTTLS', description: t('alerts.starttlsHelp') },
  { value: 'tls', label: t('alerts.implicitTls'), description: t('alerts.tlsHelp') },
]);
const ruleKindOptions = computed<SelectOption[]>(() => [
  { value: 'issue', label: t('alerts.issueTransition'), icon: 'bug' },
  { value: 'aggregate', label: t('alerts.exploreThreshold'), icon: 'gauge' },
  { value: 'monitor', label: t('alerts.monitorOutcome'), icon: 'monitors' },
]);
const datasetOptions = computed<SelectOption[]>(() => [
  { value: 'errors', label: t('alerts.errors'), icon: 'bug' },
  { value: 'logs', label: t('alerts.logs'), icon: 'logs' },
  { value: 'spans', label: t('alerts.spans'), icon: 'traces' },
  { value: 'metrics', label: t('alerts.metrics'), icon: 'gauge' },
]);

const destinations = useQuery({
  queryKey: computed(() => ['notification-destinations', projectId.value]),
  queryFn: () => api.notificationDestinations(projectId.value),
  enabled: computed(() => canAdminister.value && Boolean(projectId.value)),
});
const availableTelegramBots = computed<AvailableTelegramBot[]>(() => {
  const bots = new Map<string, AvailableTelegramBot>();
  for (const item of destinations.data.value?.items ?? []) {
    if (item.kind !== 'telegram' || !item.telegram) continue;
    const key = item.telegram.bot_id
      ? `${item.telegram.bot_id}\u0000${item.telegram.api_base}`
      : `legacy:${item.id}`;
    const current = bots.get(key);
    const candidate: AvailableTelegramBot = current ?? {
      key,
      source_destination_id: item.id,
      id: item.telegram.bot_id,
      username: item.telegram.bot_username,
      display_name:
        item.telegram.bot_display_name ??
        item.telegram.bot_username ??
        t('alerts.savedTelegramBot'),
      api_base: item.telegram.api_base,
      active_destinations: 0,
      total_destinations: 0,
      updated_at: item.updated_at,
    };
    candidate.total_destinations += 1;
    if (item.enabled) candidate.active_destinations += 1;
    if (item.updated_at > candidate.updated_at) {
      candidate.source_destination_id = item.id;
      candidate.updated_at = item.updated_at;
    }
    bots.set(key, candidate);
  }
  return [...bots.values()].sort((left, right) => right.updated_at - left.updated_at);
});
const activeNotificationDestinations = computed(
  () => destinations.data.value?.items.filter((item) => item.enabled) ?? [],
);
const rules = useQuery({
  queryKey: computed(() => ['alert-rules', projectId.value]),
  queryFn: () => api.alertRules(projectId.value),
  enabled: computed(() => canAdminister.value && Boolean(projectId.value)),
});
const monitors = useQuery({
  queryKey: computed(() => ['monitors', projectId.value]),
  queryFn: () => api.monitors(projectId.value),
  enabled: computed(() => canAdminister.value && Boolean(projectId.value)),
});
const monitorOptions = computed<SelectOption[]>(() =>
  (monitors.data.value?.items ?? []).map((monitor) => ({
    value: monitor.id,
    label: monitor.name,
    description: `${monitor.slug} · ${monitor.environment}`,
    icon: 'monitors',
  })),
);
const deliveries = useQuery({
  queryKey: computed(() => ['notification-deliveries', projectId.value]),
  queryFn: () => api.notificationDeliveries(projectId.value),
  enabled: computed(() => canAdminister.value && Boolean(projectId.value)),
  refetchInterval: 5_000,
});
const organizationMembers = useQuery({
  queryKey: ['organization-members'],
  queryFn: api.organizationMembers,
  enabled: canAdminister,
});
const activeMembers = computed(
  () => organizationMembers.data.value?.items.filter((member) => !member.disabled_at) ?? [],
);
const smtpRecipients = computed(() => {
  const selected = new Set(selectedMemberIds.value);
  const recipients = [
    ...activeMembers.value
      .filter((member) => selected.has(member.user_id))
      .map((member) => member.email),
    ...destination.smtp_recipients
      .split(',')
      .map((value) => value.trim())
      .filter(Boolean),
  ];
  return [...new Map(recipients.map((value) => [value.toLowerCase(), value])).values()].slice(
    0,
    16,
  );
});
const telegramStartUrl = computed(() =>
  telegramBot.value
    ? `https://t.me/${telegramBot.value.username}?start=${telegramPairingCode.value}`
    : '',
);
const telegramPairingCommand = computed(() => `/start ${telegramPairingCode.value}`);
const telegramUsesHttp = computed(() =>
  destination.telegram_api_base.trim().toLowerCase().startsWith('http://'),
);
watch(
  activeMembers,
  (members) => {
    if (!memberSelectionTouched.value && !selectedMemberIds.value.length) {
      selectedMemberIds.value = members.slice(0, 16).map((member) => member.user_id);
    }
  },
  { immediate: true },
);

const saveDestination = useMutation({
  mutationFn: () =>
    api.putNotificationDestination(projectId.value, {
      kind: 'smtp_email',
      endpoint: destination.endpoint.trim(),
      secret: destination.secret,
      enabled: destination.enabled,
      smtp_port: destination.smtp_port,
      smtp_security: destination.smtp_security,
      smtp_username: destination.smtp_username.trim(),
      smtp_from: destination.smtp_from.trim(),
      smtp_recipients: smtpRecipients.value,
    }),
  onSuccess: async (value) => {
    destination.secret = '';
    destination.endpoint = '';
    destination.smtp_username = '';
    destination.smtp_from = '';
    destination.smtp_recipients = '';
    memberSelectionTouched.value = false;
    selectedMemberIds.value = activeMembers.value.slice(0, 16).map((member) => member.user_id);
    selectedDestinations.value = [...selectedDestinations.value, value.id];
    await queryClient.invalidateQueries({
      queryKey: ['notification-destinations', projectId.value],
    });
  },
});
const connectTelegram = useMutation({
  mutationFn: () =>
    api.checkTelegramBot(
      projectId.value,
      telegramSourceDestinationId.value ? null : destination.secret,
      telegramSourceDestinationId.value ? null : destination.telegram_api_base.trim(),
      telegramSourceDestinationId.value || null,
    ),
  onSuccess: (bot) => {
    telegramBot.value = bot;
    destination.telegram_api_base = bot.api_base;
    telegramSyncNotice.value = '';
  },
});
const saveTelegramDestination = useMutation({
  mutationFn: () =>
    api.putNotificationDestination(projectId.value, {
      kind: 'telegram',
      endpoint: destination.telegram_chat_id.trim(),
      secret: telegramSourceDestinationId.value ? null : destination.secret,
      enabled: destination.enabled,
      telegram_api_base: telegramSourceDestinationId.value
        ? null
        : destination.telegram_api_base.trim(),
      telegram_source_destination_id: telegramSourceDestinationId.value || null,
      telegram_message_thread_id:
        String(destination.telegram_message_thread_id).trim() === ''
          ? null
          : Number(destination.telegram_message_thread_id),
    }),
  onSuccess: async (value) => {
    destination.telegram_chat_id = '';
    destination.telegram_message_thread_id = '';
    selectedDestinations.value = [...new Set([...selectedDestinations.value, value.id])];
    telegramSyncNotice.value = t('alerts.telegramDestinationSaved');
    await queryClient.invalidateQueries({
      queryKey: ['notification-destinations', projectId.value],
    });
  },
});
const syncTelegram = useMutation({
  mutationFn: (generation: number) => {
    // The generation is carried through Vue Query so stale polling responses
    // can be ignored without sending this client-only value to the server.
    void generation;
    return api.syncTelegramSubscribers(
      projectId.value,
      telegramSourceDestinationId.value ? null : destination.secret,
      telegramSourceDestinationId.value ? null : destination.telegram_api_base.trim(),
      telegramPairingCode.value,
      telegramDiscoveryOffset.value,
      telegramSourceDestinationId.value || null,
    );
  },
  onSuccess: async (value, generation) => {
    if (!telegramDiscoveryActive.value || generation !== telegramDiscoveryGeneration) {
      return;
    }
    telegramBot.value = value.bot;
    telegramDiscoveryOffset.value = value.next_offset;
    if (value.subscribers.length) {
      stopTelegramDiscovery();
      selectedDestinations.value = [
        ...new Set([
          ...selectedDestinations.value,
          ...value.subscribers.map((subscriber) => subscriber.destination_id),
        ]),
      ];
      telegramSyncNotice.value = t('alerts.subscribersConnected', value.subscribers.length);
      await queryClient.invalidateQueries({
        queryKey: ['notification-destinations', projectId.value],
      });
    } else if (telegramDiscoveryActive.value && Date.now() < telegramDiscoveryDeadline) {
      telegramSyncNotice.value = t('alerts.waitingForTelegram');
      telegramDiscoveryTimer = setTimeout(() => syncTelegram.mutate(generation), 500);
    } else {
      stopTelegramDiscovery();
      telegramSyncNotice.value = t('alerts.discoveryTimedOut');
    }
  },
  onError: (_error, generation) => {
    if (generation === telegramDiscoveryGeneration) {
      stopTelegramDiscovery();
    }
  },
});
watch(kind, () => {
  stopTelegramDiscovery();
  destination.secret = '';
  telegramSourceDestinationId.value = '';
  telegramBot.value = null;
  telegramSyncNotice.value = '';
  saveDestination.reset();
  saveTelegramDestination.reset();
  connectTelegram.reset();
  syncTelegram.reset();
});
watch([() => destination.secret, () => destination.telegram_api_base], () => {
  if (kind.value === 'telegram' && telegramBot.value && !telegramSourceDestinationId.value) {
    stopTelegramDiscovery();
    telegramBot.value = null;
    telegramSyncNotice.value = '';
    syncTelegram.reset();
  }
});

const saveRule = useMutation({
  mutationFn: () =>
    api.putAlertRule(projectId.value, {
      name: ruleName.value.trim(),
      enabled: true,
      triggers: [
        ...(ruleKind.value === 'issue' && triggers.new_issue ? ['new_issue'] : []),
        ...(ruleKind.value === 'issue' && triggers.regression ? ['regression'] : []),
        ...(ruleKind.value === 'issue' && triggers.resolved ? ['resolved'] : []),
      ],
      destination_ids: selectedDestinations.value,
      aggregate_dataset: ruleKind.value === 'aggregate' ? aggregateRule.dataset : null,
      lookback_minutes: ruleKind.value === 'aggregate' ? aggregateRule.lookback_minutes : null,
      evaluation_interval_minutes:
        ruleKind.value === 'aggregate' ? aggregateRule.evaluation_interval_minutes : null,
      threshold: ruleKind.value === 'aggregate' ? aggregateRule.threshold : null,
      environment:
        ruleKind.value === 'aggregate' && !['errors', 'metrics'].includes(aggregateRule.dataset)
          ? aggregateRule.environment
          : null,
      release:
        ruleKind.value === 'aggregate' && !['errors', 'metrics'].includes(aggregateRule.dataset)
          ? aggregateRule.release
          : null,
      notify_resolved:
        ruleKind.value === 'aggregate'
          ? aggregateRule.notify_resolved
          : ruleKind.value === 'monitor'
            ? monitorRule.notify_resolved
            : null,
      cooldown_minutes: aggregateRule.cooldown_minutes,
      storm_limit_per_hour: aggregateRule.storm_limit_per_hour,
      monitor_id: ruleKind.value === 'monitor' ? monitorRule.monitor_id : null,
      monitor_outcomes:
        ruleKind.value === 'monitor'
          ? [
              ...(monitorRule.error ? ['error'] : []),
              ...(monitorRule.timeout ? ['timeout'] : []),
              ...(monitorRule.missed ? ['missed'] : []),
            ]
          : [],
    }),
  onSuccess: async () => {
    ruleName.value = '';
    await queryClient.invalidateQueries({ queryKey: ['alert-rules', projectId.value] });
  },
});
const testDestination = useMutation({
  mutationFn: (destinationId: string) =>
    api.testNotificationDestination(projectId.value, destinationId),
  onSuccess: async () => {
    await queryClient.invalidateQueries({
      queryKey: ['notification-deliveries', projectId.value],
    });
  },
});
const setDestinationEnabled = useMutation({
  mutationFn: async ({ id, enabled }: { id: string; enabled: boolean }) => {
    if (enabled) {
      await api.restoreNotificationDestination(projectId.value, id);
      return;
    }
    await api.disableNotificationDestination(projectId.value, id);
  },
  onSuccess: async (_value, input) => {
    if (!input.enabled) {
      selectedDestinations.value = selectedDestinations.value.filter((id) => id !== input.id);
    }
    await queryClient.invalidateQueries({
      queryKey: ['notification-destinations', projectId.value],
    });
  },
});

function toggleDestination(id: string): void {
  selectedDestinations.value = selectedDestinations.value.includes(id)
    ? selectedDestinations.value.filter((value) => value !== id)
    : [...selectedDestinations.value, id];
}

function toggleAllMembers(): void {
  memberSelectionTouched.value = true;
  selectedMemberIds.value =
    selectedMemberIds.value.length === activeMembers.value.length
      ? []
      : activeMembers.value.slice(0, 16).map((member) => member.user_id);
}

function createPairingCode(): string {
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  return [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

function useTelegramBot(bot: AvailableTelegramBot): void {
  stopTelegramDiscovery();
  telegramSourceDestinationId.value = bot.source_destination_id;
  destination.secret = '';
  destination.telegram_api_base = bot.api_base;
  telegramSyncNotice.value = '';
  connectTelegram.reset();
  if (bot.id && bot.username) {
    telegramBot.value = {
      id: bot.id,
      username: bot.username,
      display_name: bot.display_name,
      api_base: bot.api_base,
    };
  } else {
    telegramBot.value = null;
    connectTelegram.mutate();
  }
}

function useNewTelegramBot(): void {
  stopTelegramDiscovery();
  telegramSourceDestinationId.value = '';
  telegramBot.value = null;
  destination.secret = '';
  destination.telegram_api_base = 'https://api.telegram.org';
  telegramSyncNotice.value = '';
  connectTelegram.reset();
  syncTelegram.reset();
}

watch(
  [availableTelegramBots, kind],
  ([bots, selectedKind]) => {
    if (
      selectedKind === 'telegram' &&
      bots.length === 1 &&
      !telegramSourceDestinationId.value &&
      !destination.secret &&
      !telegramBot.value
    ) {
      useTelegramBot(bots[0]);
    }
  },
  { immediate: true },
);

function startTelegramDiscovery(): void {
  stopTelegramDiscovery();
  telegramPairingCode.value = createPairingCode();
  telegramDiscoveryOffset.value = null;
  telegramDiscoveryDeadline = Date.now() + 90_000;
  telegramDiscoveryActive.value = true;
  telegramSyncNotice.value = t('alerts.waitingForTelegram');
  syncTelegram.reset();
  syncTelegram.mutate(telegramDiscoveryGeneration);
}

function stopTelegramDiscovery(): void {
  telegramDiscoveryActive.value = false;
  telegramDiscoveryGeneration += 1;
  if (telegramDiscoveryTimer !== undefined) {
    clearTimeout(telegramDiscoveryTimer);
    telegramDiscoveryTimer = undefined;
  }
}

onBeforeUnmount(stopTelegramDiscovery);

function destinationDisplayName(item: NotificationDestination): string {
  if (item.kind !== 'telegram') return t('alerts.smtpEmail');
  if (!item.telegram) return item.endpoint;
  if (item.telegram.chat_type === 'private' && item.telegram.chat_username) {
    return `@${item.telegram.chat_username}`;
  }
  return (
    item.telegram.chat_display_name ??
    (item.telegram.chat_username ? `@${item.telegram.chat_username}` : item.endpoint)
  );
}

function destinationEndpointLabel(item: NotificationDestination): string {
  if (item.kind !== 'telegram') {
    return item.endpoint;
  }
  const parts = [
    item.telegram?.chat_type ? t(`alerts.telegramChatType.${item.telegram.chat_type}`) : null,
    t('alerts.telegramChatIdValue', { id: item.endpoint }),
    item.telegram?.message_thread_id
      ? t('alerts.telegramTopic', { id: item.telegram.message_thread_id })
      : null,
  ];
  return parts.filter(Boolean).join(' · ');
}

function triggerLabel(value: string): string {
  const keys: Record<string, string> = {
    new_issue: 'alerts.newIssue',
    regression: 'alerts.regression',
    resolved: 'alerts.resolved',
  };
  return keys[value] ? t(keys[value]) : value.replaceAll('_', ' ');
}

function outcomeLabel(value: string): string {
  const keys: Record<string, string> = {
    error: 'alerts.error',
    timeout: 'alerts.timeout',
    missed: 'alerts.missed',
  };
  return keys[value] ? t(keys[value]) : value.replaceAll('_', ' ');
}

function datasetLabel(value: string): string {
  const key = `alerts.${value}`;
  return t(key);
}
</script>

<template>
  <section class="page-heading">
    <div>
      <p class="eyebrow">{{ $t('alerts.eyebrow') }}</p>
      <h1>{{ $t('alerts.title') }}</h1>
      <p>{{ $t('alerts.description') }}</p>
    </div>
    <StatusBadge status="durable_outbox" />
  </section>

  <EmptyState
    v-if="!canAdminister"
    icon="shield"
    :title="$t('alerts.restricted')"
    :description="$t('alerts.restrictedDescription')"
  />
  <ApiErrorPanel
    v-else-if="destinations.error.value || rules.error.value"
    :error="destinations.error.value || rules.error.value"
    :title="$t('alerts.loadFailed')"
    @retry="
      destinations.refetch();
      rules.refetch();
    "
  />
  <LoadingPanel
    v-else-if="destinations.isLoading.value || rules.isLoading.value"
    :label="$t('alerts.loading')"
  />
  <template v-else>
    <section class="panel">
      <div class="section-heading">
        <div class="section-heading__content">
          <span class="section-icon section-icon--info"><AppIcon name="alerts" /></span>
          <div>
            <p class="eyebrow">{{ $t('alerts.destination') }}</p>
            <h2>{{ $t('alerts.addChannel') }}</h2>
            <p>{{ $t('alerts.credentialsHelp') }}</p>
          </div>
        </div>
      </div>

      <ApiErrorPanel
        v-if="
          saveDestination.error.value ||
          saveTelegramDestination.error.value ||
          connectTelegram.error.value ||
          syncTelegram.error.value
        "
        :error="
          saveDestination.error.value ||
          saveTelegramDestination.error.value ||
          connectTelegram.error.value ||
          syncTelegram.error.value
        "
        :title="
          kind !== 'telegram'
            ? $t('alerts.emailSaveFailed')
            : saveTelegramDestination.error.value
              ? $t('alerts.telegramSaveFailed')
              : syncTelegram.error.value
                ? $t('alerts.discoveryFailed')
                : $t('alerts.botConnectFailed')
        "
      />
      <form
        class="settings-form"
        @submit.prevent="kind === 'telegram' ? connectTelegram.mutate() : saveDestination.mutate()"
      >
        <BaseSelect
          :model-value="kind"
          :options="kindOptions"
          :label="$t('alerts.provider')"
          @update:model-value="kind = $event"
        />
        <template v-if="kind === 'telegram'">
          <section v-if="availableTelegramBots.length" class="telegram-bot-list">
            <div>
              <p class="eyebrow">{{ $t('alerts.availableTelegramBots') }}</p>
              <h3>{{ $t('alerts.useSavedTelegramBot') }}</h3>
              <p>{{ $t('alerts.useSavedTelegramBotHelp') }}</p>
            </div>
            <article v-for="bot in availableTelegramBots" :key="bot.key">
              <span class="section-icon section-icon--success">
                <AppIcon name="telegram" />
              </span>
              <span>
                <strong>{{ bot.username ? `@${bot.username}` : bot.display_name }}</strong>
                <small>
                  {{ bot.api_base }} ·
                  {{
                    $t('alerts.telegramRecipientCount', {
                      count: bot.active_destinations,
                    })
                  }}
                </small>
              </span>
              <button
                class="button button--secondary"
                type="button"
                :disabled="telegramSourceDestinationId === bot.source_destination_id"
                @click="useTelegramBot(bot)"
              >
                <AppIcon name="connect" :size="15" />
                {{
                  telegramSourceDestinationId === bot.source_destination_id
                    ? $t('alerts.telegramBotSelected')
                    : $t('alerts.useTelegramBot')
                }}
              </button>
            </article>
          </section>
          <template v-if="!telegramSourceDestinationId">
            <label>
              {{ $t('alerts.telegramApiBase') }}
              <input
                v-model="destination.telegram_api_base"
                required
                type="url"
                placeholder="https://api.telegram.org"
              />
              <small>{{ $t('alerts.telegramApiBaseHelp') }}</small>
            </label>
            <p v-if="telegramUsesHttp" class="permission-note" role="status">
              <AppIcon name="info" :size="16" />
              {{ $t('alerts.telegramHttpWarning') }}
            </p>
            <label>
              {{ $t('alerts.botToken') }}
              <input
                v-model="destination.secret"
                required
                type="password"
                autocomplete="new-password"
                placeholder="123456:bot-token"
              />
              <small>{{ $t('alerts.botTokenHelp') }}</small>
            </label>
            <button
              class="button button--primary"
              type="submit"
              :disabled="connectTelegram.isPending.value"
            >
              <AppIcon name="connect" :size="16" />
              {{
                connectTelegram.isPending.value ? $t('alerts.checkingBot') : $t('alerts.connectBot')
              }}
            </button>
          </template>
          <section v-if="telegramBot" class="telegram-pairing">
            <div class="telegram-pairing__identity">
              <span class="section-icon section-icon--success">
                <AppIcon name="telegram" />
              </span>
              <span>
                <strong>{{ telegramBot.display_name }}</strong>
                <small>{{ $t('alerts.botReady', { username: telegramBot.username }) }}</small>
              </span>
              <div v-if="telegramSourceDestinationId" class="button-row">
                <button
                  class="button button--secondary"
                  type="button"
                  :disabled="connectTelegram.isPending.value"
                  @click="connectTelegram.mutate()"
                >
                  <AppIcon name="refresh" :size="15" />
                  {{ $t('alerts.checkTelegramBot') }}
                </button>
                <button class="button button--secondary" type="button" @click="useNewTelegramBot">
                  <AppIcon name="plus" :size="15" />
                  {{ $t('alerts.connectAnotherTelegramBot') }}
                </button>
              </div>
            </div>
            <div>
              <p class="eyebrow">{{ $t('alerts.telegramTarget') }}</p>
              <h3>{{ $t('alerts.addTelegramTarget') }}</h3>
              <p>{{ $t('alerts.oneBotManyTargets') }}</p>
            </div>
            <div class="form-grid">
              <label>
                {{ $t('alerts.telegramChatId') }}
                <input
                  v-model="destination.telegram_chat_id"
                  placeholder="-1001234567890 or @channel"
                />
                <small>{{ $t('alerts.telegramChatIdHelp') }}</small>
              </label>
              <label>
                {{ $t('alerts.telegramThreadId') }}
                <input
                  v-model="destination.telegram_message_thread_id"
                  type="number"
                  min="1"
                  step="1"
                  placeholder="42"
                />
                <small>{{ $t('alerts.telegramThreadIdHelp') }}</small>
              </label>
            </div>
            <button
              class="button button--primary"
              type="button"
              :disabled="
                saveTelegramDestination.isPending.value || !destination.telegram_chat_id.trim()
              "
              @click="saveTelegramDestination.mutate()"
            >
              <AppIcon name="telegram" :size="16" />
              {{
                saveTelegramDestination.isPending.value
                  ? $t('alerts.saving')
                  : $t('alerts.saveTelegramTarget')
              }}
            </button>
            <div class="telegram-discovery">
              <div>
                <p class="eyebrow">{{ $t('alerts.telegramDiscovery') }}</p>
                <h3>{{ $t('alerts.findTelegramTarget') }}</h3>
                <p>{{ $t('alerts.telegramDiscoveryHelp') }}</p>
              </div>
              <button
                v-if="!telegramDiscoveryActive"
                class="button button--secondary"
                type="button"
                @click="startTelegramDiscovery"
              >
                <AppIcon name="users" :size="16" />
                {{ $t('alerts.findAutomatically') }}
              </button>
              <template v-else>
                <CodeBlock
                  :code="telegramPairingCommand"
                  language="text"
                  :title="$t('alerts.telegramPairingCommand')"
                />
                <p class="field-help">{{ $t('alerts.telegramGroupTopicHelp') }}</p>
                <div class="button-row">
                  <a
                    class="button button--primary"
                    :href="telegramStartUrl"
                    target="_blank"
                    rel="noreferrer"
                  >
                    <AppIcon name="telegram" :size="16" />
                    {{ $t('alerts.openTelegram') }}
                  </a>
                  <button
                    class="button button--secondary"
                    type="button"
                    @click="stopTelegramDiscovery"
                  >
                    {{ $t('common.cancel') }}
                  </button>
                </div>
              </template>
            </div>
            <p v-if="telegramSyncNotice" class="success-notice" role="status">
              <AppIcon name="info" :size="16" />
              {{ telegramSyncNotice }}
            </p>
          </section>
        </template>
        <template v-else>
          <div class="form-grid">
            <label>
              {{ $t('alerts.smtpHost') }}
              <input v-model="destination.endpoint" required placeholder="smtp.example.com" />
            </label>
            <label>
              {{ $t('alerts.smtpPassword') }}
              <input
                v-model="destination.secret"
                required
                type="password"
                autocomplete="new-password"
                :placeholder="$t('alerts.appPassword')"
              />
            </label>
          </div>
          <div class="form-grid form-grid--three">
            <label>
              {{ $t('alerts.port') }}
              <input
                v-model.number="destination.smtp_port"
                required
                type="number"
                min="1"
                max="65535"
              />
            </label>
            <BaseSelect
              :model-value="destination.smtp_security"
              :options="securityOptions"
              :label="$t('alerts.transportSecurity')"
              @update:model-value="destination.smtp_security = $event"
            />
            <label>
              {{ $t('alerts.username') }}
              <input v-model="destination.smtp_username" required autocomplete="username" />
            </label>
          </div>
          <div class="form-grid">
            <label>
              {{ $t('alerts.from') }}
              <input
                v-model="destination.smtp_from"
                required
                type="email"
                placeholder="alerts@example.com"
              />
            </label>
            <label>
              {{ $t('alerts.recipients') }}
              <input
                v-model="destination.smtp_recipients"
                placeholder="external-oncall@example.com"
              />
              <small>{{ $t('alerts.recipientsHelp') }}</small>
            </label>
          </div>
          <div class="notification-audience">
            <div class="section-heading">
              <div>
                <p class="eyebrow">{{ $t('alerts.audience') }}</p>
                <h3>{{ $t('alerts.participants') }}</h3>
                <p>{{ $t('alerts.participantsHelp') }}</p>
              </div>
              <button class="button button--secondary" type="button" @click="toggleAllMembers">
                <AppIcon name="organization" :size="16" />
                {{
                  selectedMemberIds.length === activeMembers.length
                    ? $t('alerts.clearMembers')
                    : $t('alerts.selectAll')
                }}
              </button>
            </div>
            <LoadingPanel
              v-if="organizationMembers.isPending.value"
              :label="$t('alerts.loadingMembers')"
            />
            <ApiErrorPanel
              v-else-if="organizationMembers.error.value"
              :error="organizationMembers.error.value"
              :title="$t('alerts.membersFailed')"
              @retry="organizationMembers.refetch()"
            />
            <div v-else class="notification-member-grid">
              <label v-for="member in activeMembers" :key="member.user_id" class="choice-card">
                <input
                  v-model="selectedMemberIds"
                  type="checkbox"
                  :value="member.user_id"
                  @change="memberSelectionTouched = true"
                />
                <span>
                  <strong>{{ member.display_name }}</strong>
                  <small>{{ member.email }} · {{ $t(`organization.${member.role}`) }}</small>
                </span>
              </label>
            </div>
            <p class="field-help">
              {{ $t('alerts.recipientsSelected', { count: smtpRecipients.length }) }}
            </p>
          </div>
          <button
            class="button button--primary"
            type="submit"
            :disabled="saveDestination.isPending.value || smtpRecipients.length === 0"
          >
            <AppIcon name="email" :size="16" />
            {{ saveDestination.isPending.value ? $t('alerts.saving') : $t('alerts.saveEmail') }}
          </button>
        </template>
      </form>
      <div v-if="destinations.data.value?.items.length" class="channel-test-list">
        <article
          v-for="item in destinations.data.value.items"
          :key="item.id"
          :class="{ 'notification-destination--disabled': !item.enabled }"
        >
          <AppIcon :name="item.kind === 'telegram' ? 'telegram' : 'email'" />
          <span>
            <strong>{{ destinationDisplayName(item) }}</strong>
            <small>{{ destinationEndpointLabel(item) }}</small>
          </span>
          <button
            v-if="item.enabled"
            class="button button--secondary"
            type="button"
            :disabled="testDestination.isPending.value"
            @click="testDestination.mutate(item.id)"
          >
            <AppIcon name="telegram" :size="15" />
            {{ $t('alerts.sendTest') }}
          </button>
          <button
            v-if="item.enabled"
            class="button button--danger"
            type="button"
            :disabled="setDestinationEnabled.isPending.value"
            @click="setDestinationEnabled.mutate({ id: item.id, enabled: false })"
          >
            <AppIcon name="delete" :size="15" />
            {{ $t('alerts.disableRecipient') }}
          </button>
          <button
            v-else
            class="button button--secondary"
            type="button"
            :disabled="setDestinationEnabled.isPending.value"
            @click="setDestinationEnabled.mutate({ id: item.id, enabled: true })"
          >
            <AppIcon name="refresh" :size="15" />
            {{ $t('alerts.restoreRecipient') }}
          </button>
        </article>
      </div>
      <ApiErrorPanel
        v-if="testDestination.error.value || setDestinationEnabled.error.value"
        :error="testDestination.error.value || setDestinationEnabled.error.value"
        :title="
          setDestinationEnabled.error.value
            ? $t('alerts.recipientStateFailed')
            : $t('alerts.testFailed')
        "
      />
    </section>

    <section class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">{{ $t('alerts.issueRule') }}</p>
          <h2>{{ $t('alerts.chooseWhen') }}</h2>
          <p>{{ $t('alerts.ruleHelp') }}</p>
        </div>
      </div>
      <ApiErrorPanel
        v-if="saveRule.error.value"
        :error="saveRule.error.value"
        :title="$t('alerts.ruleSaveFailed')"
      />
      <EmptyState
        v-if="!activeNotificationDestinations.length"
        icon="alerts"
        :title="$t('alerts.addDestination')"
        :description="$t('alerts.addDestinationHelp')"
      />
      <form v-else class="settings-form" @submit.prevent="saveRule.mutate()">
        <label>
          {{ $t('alerts.ruleName') }}
          <input v-model="ruleName" required :placeholder="$t('alerts.rulePlaceholder')" />
        </label>
        <BaseSelect
          :model-value="ruleKind"
          :options="ruleKindOptions"
          :label="$t('alerts.ruleType')"
          @update:model-value="ruleKind = $event"
        />
        <div v-if="ruleKind === 'issue'" class="alert-trigger-grid">
          <label class="choice-card">
            <input v-model="triggers.new_issue" type="checkbox" />
            <span
              ><strong>{{ $t('alerts.newIssue') }}</strong
              ><small>{{ $t('alerts.newIssueHelp') }}</small></span
            >
          </label>
          <label class="choice-card">
            <input v-model="triggers.resolved" type="checkbox" />
            <span
              ><strong>{{ $t('alerts.resolved') }}</strong
              ><small>{{ $t('alerts.resolvedHelp') }}</small></span
            >
          </label>
          <label class="choice-card">
            <input v-model="triggers.regression" type="checkbox" />
            <span
              ><strong>{{ $t('alerts.regression') }}</strong
              ><small>{{ $t('alerts.regressionHelp') }}</small></span
            >
          </label>
        </div>
        <div v-else-if="ruleKind === 'aggregate'" class="aggregate-rule-fields">
          <div class="form-grid form-grid--three">
            <BaseSelect
              :model-value="aggregateRule.dataset"
              :options="datasetOptions"
              :label="$t('alerts.dataset')"
              @update:model-value="aggregateRule.dataset = $event"
            />
            <label>
              {{ $t('alerts.threshold') }}
              <input v-model.number="aggregateRule.threshold" type="number" min="1" required />
            </label>
            <label>
              {{ $t('alerts.lookback') }}
              <input
                v-model.number="aggregateRule.lookback_minutes"
                type="number"
                min="1"
                max="43200"
                required
              />
            </label>
          </div>
          <div class="form-grid form-grid--three">
            <label>
              {{ $t('alerts.evaluateEvery') }}
              <input
                v-model.number="aggregateRule.evaluation_interval_minutes"
                type="number"
                min="1"
                max="1440"
                required
              />
            </label>
            <label>
              {{ $t('alerts.cooldown') }}
              <input
                v-model.number="aggregateRule.cooldown_minutes"
                type="number"
                min="0"
                max="43200"
                required
              />
            </label>
            <label>
              {{ $t('alerts.stormLimit') }}
              <input
                v-model.number="aggregateRule.storm_limit_per_hour"
                type="number"
                min="1"
                max="10000"
                required
              />
            </label>
          </div>
          <div v-if="!['errors', 'metrics'].includes(aggregateRule.dataset)" class="form-grid">
            <label>
              {{ $t('alerts.environmentPredicate') }}
              <input
                v-model="aggregateRule.environment"
                :placeholder="$t('alerts.optionalProduction')"
              />
            </label>
            <label>
              {{ $t('alerts.releasePredicate') }}
              <input v-model="aggregateRule.release" :placeholder="$t('alerts.optionalRelease')" />
            </label>
          </div>
          <label class="choice-card">
            <input v-model="aggregateRule.notify_resolved" type="checkbox" />
            <span
              ><strong>{{ $t('alerts.recovery') }}</strong
              ><small>{{ $t('alerts.recoveryHelp') }}</small></span
            >
          </label>
        </div>
        <div v-else class="aggregate-rule-fields">
          <EmptyState
            v-if="!monitorOptions.length"
            icon="monitors"
            :title="$t('alerts.createMonitorFirst')"
            :description="$t('alerts.createMonitorFirstHelp')"
          />
          <template v-else>
            <BaseSelect
              :model-value="monitorRule.monitor_id"
              :options="monitorOptions"
              :label="$t('alerts.monitor')"
              @update:model-value="monitorRule.monitor_id = $event"
            />
            <div class="alert-trigger-grid">
              <label class="choice-card">
                <input v-model="monitorRule.error" type="checkbox" />
                <span
                  ><strong>{{ $t('alerts.error') }}</strong
                  ><small>{{ $t('alerts.errorHelp') }}</small></span
                >
              </label>
              <label class="choice-card">
                <input v-model="monitorRule.timeout" type="checkbox" />
                <span
                  ><strong>{{ $t('alerts.timeout') }}</strong
                  ><small>{{ $t('alerts.timeoutHelp') }}</small></span
                >
              </label>
              <label class="choice-card">
                <input v-model="monitorRule.missed" type="checkbox" />
                <span
                  ><strong>{{ $t('alerts.missed') }}</strong
                  ><small>{{ $t('alerts.missedHelp') }}</small></span
                >
              </label>
            </div>
            <label class="choice-card">
              <input v-model="monitorRule.notify_resolved" type="checkbox" />
              <span
                ><strong>{{ $t('alerts.recovery') }}</strong
                ><small>{{ $t('alerts.monitorRecoveryHelp') }}</small></span
              >
            </label>
          </template>
        </div>
        <div class="destination-choice-list">
          <button
            v-for="item in activeNotificationDestinations"
            :key="item.id"
            class="destination-choice"
            :class="{ 'destination-choice--selected': selectedDestinations.includes(item.id) }"
            type="button"
            @click="toggleDestination(item.id)"
          >
            <AppIcon :name="item.kind === 'telegram' ? 'telegram' : 'email'" />
            <span>
              <strong>{{ destinationDisplayName(item) }}</strong>
              <small>{{ destinationEndpointLabel(item) }}</small>
            </span>
            <AppIcon v-if="selectedDestinations.includes(item.id)" name="check" />
          </button>
        </div>
        <button
          class="button button--primary"
          type="submit"
          :disabled="
            saveRule.isPending.value ||
            !selectedDestinations.length ||
            (ruleKind === 'issue' &&
              !triggers.new_issue &&
              !triggers.regression &&
              !triggers.resolved) ||
            (ruleKind === 'monitor' &&
              (!monitorRule.monitor_id ||
                (!monitorRule.error && !monitorRule.timeout && !monitorRule.missed)))
          "
        >
          <AppIcon name="save" :size="16" />
          {{ saveRule.isPending.value ? $t('alerts.saving') : $t('alerts.createRule') }}
        </button>
      </form>
    </section>

    <section class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">{{ $t('alerts.activeConfiguration') }}</p>
          <h2>{{ $t('alerts.rules') }}</h2>
        </div>
      </div>
      <EmptyState
        v-if="!rules.data.value?.items.length"
        icon="alerts"
        :title="$t('alerts.noRules')"
        :description="$t('alerts.noRulesHelp')"
      />
      <div v-else class="alert-rule-list">
        <article v-for="rule in rules.data.value?.items" :key="rule.id" class="alert-rule-card">
          <span class="section-icon section-icon--warning"><AppIcon name="alerts" /></span>
          <div>
            <strong>{{ rule.name }}</strong>
            <p>
              {{
                rule.monitor
                  ? $t('alerts.cronOutcomes', {
                      outcomes: rule.monitor.outcomes.map(outcomeLabel).join(' / '),
                    })
                  : rule.aggregate
                    ? $t('alerts.aggregateSummary', {
                        dataset: datasetLabel(rule.aggregate.dataset),
                        threshold: rule.aggregate.threshold,
                        minutes: rule.aggregate.lookback_minutes,
                      })
                    : rule.triggers.map(triggerLabel).join(' · ')
              }}
            </p>
          </div>
          <StatusBadge :status="rule.enabled ? 'active' : 'disabled'" />
        </article>
      </div>
    </section>

    <section class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">{{ $t('alerts.durableHistory') }}</p>
          <h2>{{ $t('alerts.deliveries') }}</h2>
          <p>{{ $t('alerts.deliveriesHelp') }}</p>
        </div>
      </div>
      <EmptyState
        v-if="!deliveries.data.value?.items.length"
        icon="history"
        :title="$t('alerts.noDeliveries')"
        :description="$t('alerts.noDeliveriesHelp')"
      />
      <div v-else class="alert-rule-list">
        <article
          v-for="delivery in deliveries.data.value?.items"
          :key="delivery.id"
          class="alert-rule-card"
        >
          <span class="section-icon"><AppIcon name="telegram" /></span>
          <div>
            <strong>{{ delivery.id.slice(0, 12) }}</strong>
            <p>
              {{ $t('alerts.attempts', delivery.attempts) }}
              <template v-if="delivery.last_error"> В· {{ delivery.last_error }}</template>
            </p>
          </div>
          <StatusBadge :status="delivery.status" />
        </article>
      </div>
    </section>
  </template>
</template>
