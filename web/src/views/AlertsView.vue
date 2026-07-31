<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
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
import type { TelegramBot } from '../api/types';
import { useSessionStore } from '../stores/session';

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
const telegramPairingCode = ref(createPairingCode());
const telegramSyncNotice = ref('');
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
  mutationFn: () => api.checkTelegramBot(projectId.value, destination.secret),
  onSuccess: (bot) => {
    telegramBot.value = bot;
    telegramSyncNotice.value = '';
  },
});
const syncTelegram = useMutation({
  mutationFn: () =>
    api.syncTelegramSubscribers(projectId.value, destination.secret, telegramPairingCode.value),
  onSuccess: async (value) => {
    telegramBot.value = value.bot;
    selectedDestinations.value = [
      ...new Set([
        ...selectedDestinations.value,
        ...value.subscribers.map((subscriber) => subscriber.destination_id),
      ]),
    ];
    telegramSyncNotice.value = value.subscribers.length
      ? t('alerts.subscribersConnected', value.subscribers.length)
      : t('alerts.noSubscribers');
    await queryClient.invalidateQueries({
      queryKey: ['notification-destinations', projectId.value],
    });
  },
});
watch(kind, () => {
  destination.secret = '';
  telegramBot.value = null;
  telegramSyncNotice.value = '';
  saveDestination.reset();
  connectTelegram.reset();
  syncTelegram.reset();
});
watch(
  () => destination.secret,
  () => {
    if (kind.value === 'telegram' && telegramBot.value) {
      telegramBot.value = null;
      telegramSyncNotice.value = '';
      syncTelegram.reset();
    }
  },
);

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
        ruleKind.value === 'aggregate' && aggregateRule.dataset !== 'errors'
          ? aggregateRule.environment
          : null,
      release:
        ruleKind.value === 'aggregate' && aggregateRule.dataset !== 'errors'
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

function renewTelegramLink(): void {
  telegramPairingCode.value = createPairingCode();
  telegramSyncNotice.value = '';
}

function maskedDestinationEndpoint(endpoint: string): string {
  return endpoint.length > 4
    ? t('alerts.subscriberMasked', { suffix: endpoint.slice(-4) })
    : t('alerts.telegramSubscriber');
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
          saveDestination.error.value || connectTelegram.error.value || syncTelegram.error.value
        "
        :error="
          saveDestination.error.value || connectTelegram.error.value || syncTelegram.error.value
        "
        :title="
          kind !== 'telegram'
            ? $t('alerts.emailSaveFailed')
            : syncTelegram.error.value
              ? $t('alerts.subscribersSyncFailed')
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
          <section v-if="telegramBot" class="telegram-pairing">
            <div class="telegram-pairing__identity">
              <span class="section-icon section-icon--success">
                <AppIcon name="telegram" />
              </span>
              <span>
                <strong>{{ telegramBot.display_name }}</strong>
                <small>{{ $t('alerts.botReady', { username: telegramBot.username }) }}</small>
              </span>
            </div>
            <div>
              <p class="eyebrow">{{ $t('alerts.subscriberLink') }}</p>
              <h3>{{ $t('alerts.noChatId') }}</h3>
              <p>{{ $t('alerts.subscriberHelp') }}</p>
            </div>
            <CodeBlock
              :code="telegramStartUrl"
              language="text"
              :title="$t('alerts.telegramLink')"
            />
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
              <button class="button button--secondary" type="button" @click="renewTelegramLink">
                <AppIcon name="refresh" :size="16" />
                {{ $t('alerts.newLink') }}
              </button>
              <button
                class="button button--secondary"
                type="button"
                :disabled="syncTelegram.isPending.value"
                @click="syncTelegram.mutate()"
              >
                <AppIcon name="users" :size="16" />
                {{
                  syncTelegram.isPending.value ? $t('alerts.syncing') : $t('alerts.syncSubscribers')
                }}
              </button>
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
        <article v-for="item in destinations.data.value.items" :key="item.id">
          <AppIcon :name="item.kind === 'telegram' ? 'telegram' : 'email'" />
          <span>
            <strong>{{
              item.kind === 'telegram' ? $t('alerts.telegram') : $t('alerts.smtpEmail')
            }}</strong>
            <small>
              {{
                item.kind === 'telegram' ? maskedDestinationEndpoint(item.endpoint) : item.endpoint
              }}
            </small>
          </span>
          <button
            class="button button--secondary"
            type="button"
            :disabled="testDestination.isPending.value"
            @click="testDestination.mutate(item.id)"
          >
            <AppIcon name="telegram" :size="15" />
            {{ $t('alerts.sendTest') }}
          </button>
        </article>
      </div>
      <ApiErrorPanel
        v-if="testDestination.error.value"
        :error="testDestination.error.value"
        :title="$t('alerts.testFailed')"
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
        v-if="!destinations.data.value?.items.length"
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
          <div v-if="aggregateRule.dataset !== 'errors'" class="form-grid">
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
            v-for="item in destinations.data.value?.items"
            :key="item.id"
            class="destination-choice"
            :class="{ 'destination-choice--selected': selectedDestinations.includes(item.id) }"
            type="button"
            @click="toggleDestination(item.id)"
          >
            <AppIcon :name="item.kind === 'telegram' ? 'telegram' : 'email'" />
            <span>
              <strong>{{
                item.kind === 'telegram' ? $t('alerts.telegram') : $t('alerts.smtpEmail')
              }}</strong>
              <small>{{ item.endpoint }}</small>
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
