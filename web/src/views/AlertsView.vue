<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
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
const projectId = computed(() => session.selectedProjectId ?? '');
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

const kindOptions: SelectOption[] = [
  {
    value: 'telegram',
    label: 'Telegram',
    description: 'Send a concise alert through your bot.',
    icon: 'telegram',
  },
  {
    value: 'smtp_email',
    label: 'Email via SMTP',
    description: 'Works with any TLS-capable SMTP provider.',
    icon: 'email',
  },
];
const securityOptions: SelectOption[] = [
  { value: 'starttls', label: 'STARTTLS', description: 'Usually port 587.' },
  { value: 'tls', label: 'Implicit TLS', description: 'Usually port 465.' },
];
const ruleKindOptions: SelectOption[] = [
  { value: 'issue', label: 'Issue transition', icon: 'bug' },
  { value: 'aggregate', label: 'Explore threshold', icon: 'gauge' },
  { value: 'monitor', label: 'Monitor outcome', icon: 'monitors' },
];
const datasetOptions: SelectOption[] = [
  { value: 'errors', label: 'Errors', icon: 'bug' },
  { value: 'logs', label: 'Logs', icon: 'logs' },
  { value: 'spans', label: 'Spans', icon: 'traces' },
  { value: 'metrics', label: 'Metrics', icon: 'gauge' },
];

const destinations = useQuery({
  queryKey: computed(() => ['notification-destinations', projectId.value]),
  queryFn: () => api.notificationDestinations(projectId.value),
  enabled: computed(() => Boolean(projectId.value)),
});
const rules = useQuery({
  queryKey: computed(() => ['alert-rules', projectId.value]),
  queryFn: () => api.alertRules(projectId.value),
  enabled: computed(() => Boolean(projectId.value)),
});
const monitors = useQuery({
  queryKey: computed(() => ['monitors', projectId.value]),
  queryFn: () => api.monitors(projectId.value),
  enabled: computed(() => Boolean(projectId.value)),
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
  enabled: computed(() => Boolean(projectId.value)),
  refetchInterval: 5_000,
});
const organizationMembers = useQuery({
  queryKey: ['organization-members'],
  queryFn: api.organizationMembers,
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
      ? `${value.subscribers.length} Telegram subscriber${value.subscribers.length === 1 ? '' : 's'} connected and selected for the next rule.`
      : 'No new subscribers found. Open the Telegram link, press Start, then sync again.';
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
  return endpoint.length > 4 ? `Subscriber ••••${endpoint.slice(-4)}` : 'Telegram subscriber';
}
</script>

<template>
  <section class="page-heading">
    <div>
      <p class="eyebrow">Reliable delivery</p>
      <h1>Alerts</h1>
      <p>Route Issue alerts through Telegram or your own SMTP server.</p>
    </div>
    <StatusBadge status="durable_outbox" />
  </section>

  <ApiErrorPanel
    v-if="destinations.error.value || rules.error.value"
    :error="destinations.error.value || rules.error.value"
    title="Alert configuration was not loaded"
    @retry="
      destinations.refetch();
      rules.refetch();
    "
  />
  <LoadingPanel
    v-else-if="destinations.isLoading.value || rules.isLoading.value"
    label="Loading alert configuration…"
  />
  <template v-else>
    <section class="panel">
      <div class="section-heading">
        <div class="section-heading__content">
          <span class="section-icon section-icon--info"><AppIcon name="alerts" /></span>
          <div>
            <p class="eyebrow">Destination</p>
            <h2>Add a delivery channel</h2>
            <p>Credentials are encrypted before MongoDB storage and never returned by the API.</p>
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
            ? 'Email destination was not saved'
            : syncTelegram.error.value
              ? 'Telegram subscribers were not synced'
              : 'Telegram bot could not be connected'
        "
      />
      <form
        class="settings-form"
        @submit.prevent="kind === 'telegram' ? connectTelegram.mutate() : saveDestination.mutate()"
      >
        <BaseSelect
          :model-value="kind"
          :options="kindOptions"
          label="Provider"
          @update:model-value="kind = $event"
        />
        <template v-if="kind === 'telegram'">
          <label>
            Bot token
            <input
              v-model="destination.secret"
              required
              type="password"
              autocomplete="new-password"
              placeholder="123456:bot-token"
            />
            <small>
              Create a bot with @BotFather and paste its token. The token is never returned by the
              API.
            </small>
          </label>
          <button
            class="button button--primary"
            type="submit"
            :disabled="connectTelegram.isPending.value"
          >
            <AppIcon name="connect" :size="16" />
            {{ connectTelegram.isPending.value ? 'Checking bot…' : 'Connect bot' }}
          </button>
          <section v-if="telegramBot" class="telegram-pairing">
            <div class="telegram-pairing__identity">
              <span class="section-icon section-icon--success">
                <AppIcon name="telegram" />
              </span>
              <span>
                <strong>{{ telegramBot.display_name }}</strong>
                <small>@{{ telegramBot.username }} is ready to accept subscribers.</small>
              </span>
            </div>
            <div>
              <p class="eyebrow">Subscriber link</p>
              <h3>No chat ID required</h3>
              <p>
                Share this link with the people who should receive alerts. Each person opens it and
                presses Start; Metric discovers only subscribers using this exact link.
              </p>
            </div>
            <CodeBlock :code="telegramStartUrl" language="text" title="Telegram start link" />
            <div class="button-row">
              <a
                class="button button--primary"
                :href="telegramStartUrl"
                target="_blank"
                rel="noreferrer"
              >
                <AppIcon name="telegram" :size="16" />
                Open in Telegram
              </a>
              <button class="button button--secondary" type="button" @click="renewTelegramLink">
                <AppIcon name="refresh" :size="16" />
                New link
              </button>
              <button
                class="button button--secondary"
                type="button"
                :disabled="syncTelegram.isPending.value"
                @click="syncTelegram.mutate()"
              >
                <AppIcon name="users" :size="16" />
                {{ syncTelegram.isPending.value ? 'Syncing…' : 'Sync subscribers' }}
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
              SMTP host
              <input v-model="destination.endpoint" required placeholder="smtp.example.com" />
            </label>
            <label>
              SMTP password
              <input
                v-model="destination.secret"
                required
                type="password"
                autocomplete="new-password"
                placeholder="App password"
              />
            </label>
          </div>
          <div class="form-grid form-grid--three">
            <label>
              Port
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
              label="Transport security"
              @update:model-value="destination.smtp_security = $event"
            />
            <label>
              Username
              <input v-model="destination.smtp_username" required autocomplete="username" />
            </label>
          </div>
          <div class="form-grid">
            <label>
              From
              <input
                v-model="destination.smtp_from"
                required
                type="email"
                placeholder="alerts@example.com"
              />
            </label>
            <label>
              Additional recipients
              <input
                v-model="destination.smtp_recipients"
                placeholder="external-oncall@example.com"
              />
              <small>Optional comma-separated addresses outside the organization.</small>
            </label>
          </div>
          <div class="notification-audience">
            <div class="section-heading">
              <div>
                <p class="eyebrow">Organization audience</p>
                <h3>Email participants</h3>
                <p>Select organization members instead of copying their email addresses.</p>
              </div>
              <button class="button button--secondary" type="button" @click="toggleAllMembers">
                <AppIcon name="organization" :size="16" />
                {{
                  selectedMemberIds.length === activeMembers.length ? 'Clear members' : 'Select all'
                }}
              </button>
            </div>
            <LoadingPanel
              v-if="organizationMembers.isPending.value"
              label="Loading organization members…"
            />
            <ApiErrorPanel
              v-else-if="organizationMembers.error.value"
              :error="organizationMembers.error.value"
              title="Organization members were not loaded"
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
                  <small>{{ member.email }} · {{ member.role }}</small>
                </span>
              </label>
            </div>
            <p class="field-help">
              {{ smtpRecipients.length }} of 16 recipient addresses selected.
            </p>
          </div>
          <button
            class="button button--primary"
            type="submit"
            :disabled="saveDestination.isPending.value || smtpRecipients.length === 0"
          >
            <AppIcon name="email" :size="16" />
            {{ saveDestination.isPending.value ? 'Saving…' : 'Save email destination' }}
          </button>
        </template>
      </form>
      <div v-if="destinations.data.value?.items.length" class="channel-test-list">
        <article v-for="item in destinations.data.value.items" :key="item.id">
          <AppIcon :name="item.kind === 'telegram' ? 'telegram' : 'email'" />
          <span>
            <strong>{{ item.kind === 'telegram' ? 'Telegram' : 'SMTP Email' }}</strong>
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
            Send test
          </button>
        </article>
      </div>
      <ApiErrorPanel
        v-if="testDestination.error.value"
        :error="testDestination.error.value"
        title="Test notification was not queued"
      />
    </section>

    <section class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">Issue rule</p>
          <h2>Choose when to notify</h2>
          <p>One rule can fan out to several provider types without duplicate Issue logic.</p>
        </div>
      </div>
      <ApiErrorPanel
        v-if="saveRule.error.value"
        :error="saveRule.error.value"
        title="Alert rule was not saved"
      />
      <EmptyState
        v-if="!destinations.data.value?.items.length"
        icon="alerts"
        title="Add a destination first"
        description="Rules remain separate from credentials, so destinations can be reused."
      />
      <form v-else class="settings-form" @submit.prevent="saveRule.mutate()">
        <label>
          Rule name
          <input v-model="ruleName" required placeholder="Production Issue alerts" />
        </label>
        <BaseSelect
          :model-value="ruleKind"
          :options="ruleKindOptions"
          label="Rule type"
          @update:model-value="ruleKind = $event"
        />
        <div v-if="ruleKind === 'issue'" class="alert-trigger-grid">
          <label class="choice-card">
            <input v-model="triggers.new_issue" type="checkbox" />
            <span><strong>New Issue</strong><small>Notify on the first occurrence.</small></span>
          </label>
          <label class="choice-card">
            <input v-model="triggers.resolved" type="checkbox" />
            <span><strong>Resolved</strong><small>Notify when an Issue is closed.</small></span>
          </label>
          <label class="choice-card">
            <input v-model="triggers.regression" type="checkbox" />
            <span
              ><strong>Regression</strong><small>Notify when a resolved Issue returns.</small></span
            >
          </label>
        </div>
        <div v-else-if="ruleKind === 'aggregate'" class="aggregate-rule-fields">
          <div class="form-grid form-grid--three">
            <BaseSelect
              :model-value="aggregateRule.dataset"
              :options="datasetOptions"
              label="Dataset"
              @update:model-value="aggregateRule.dataset = $event"
            />
            <label>
              Count threshold
              <input v-model.number="aggregateRule.threshold" type="number" min="1" required />
            </label>
            <label>
              Lookback (minutes)
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
              Evaluate every (minutes)
              <input
                v-model.number="aggregateRule.evaluation_interval_minutes"
                type="number"
                min="1"
                max="1440"
                required
              />
            </label>
            <label>
              Cooldown (minutes)
              <input
                v-model.number="aggregateRule.cooldown_minutes"
                type="number"
                min="0"
                max="43200"
                required
              />
            </label>
            <label>
              Storm limit / hour
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
              Environment predicate
              <input v-model="aggregateRule.environment" placeholder="production (optional)" />
            </label>
            <label>
              Release predicate
              <input v-model="aggregateRule.release" placeholder="2026.07.27 (optional)" />
            </label>
          </div>
          <label class="choice-card">
            <input v-model="aggregateRule.notify_resolved" type="checkbox" />
            <span
              ><strong>Recovery notification</strong
              ><small>Notify after the count drops below threshold.</small></span
            >
          </label>
        </div>
        <div v-else class="aggregate-rule-fields">
          <EmptyState
            v-if="!monitorOptions.length"
            icon="monitors"
            title="Create a cron monitor first"
            description="Monitor alerts reference a stable monitor definition."
          />
          <template v-else>
            <BaseSelect
              :model-value="monitorRule.monitor_id"
              :options="monitorOptions"
              label="Monitor"
              @update:model-value="monitorRule.monitor_id = $event"
            />
            <div class="alert-trigger-grid">
              <label class="choice-card">
                <input v-model="monitorRule.error" type="checkbox" />
                <span><strong>Error</strong><small>The job explicitly failed.</small></span>
              </label>
              <label class="choice-card">
                <input v-model="monitorRule.timeout" type="checkbox" />
                <span><strong>Timeout</strong><small>An active run exceeded runtime.</small></span>
              </label>
              <label class="choice-card">
                <input v-model="monitorRule.missed" type="checkbox" />
                <span
                  ><strong>Missed</strong
                  ><small>No check-in arrived within the margin.</small></span
                >
              </label>
            </div>
            <label class="choice-card">
              <input v-model="monitorRule.notify_resolved" type="checkbox" />
              <span
                ><strong>Recovery notification</strong
                ><small>Notify when a failing Uptime monitor becomes healthy.</small></span
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
              <strong>{{ item.kind === 'telegram' ? 'Telegram' : 'SMTP Email' }}</strong>
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
          {{ saveRule.isPending.value ? 'Saving…' : 'Create rule' }}
        </button>
      </form>
    </section>

    <section class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">Active configuration</p>
          <h2>Rules</h2>
        </div>
      </div>
      <EmptyState
        v-if="!rules.data.value?.items.length"
        icon="alerts"
        title="No alert rules yet"
        description="Create a rule above; delivery history will use the existing durable outbox."
      />
      <div v-else class="alert-rule-list">
        <article v-for="rule in rules.data.value?.items" :key="rule.id" class="alert-rule-card">
          <span class="section-icon section-icon--warning"><AppIcon name="alerts" /></span>
          <div>
            <strong>{{ rule.name }}</strong>
            <p>
              {{
                rule.monitor
                  ? `cron outcomes: ${rule.monitor.outcomes.join(' / ')}`
                  : rule.aggregate
                    ? `${rule.aggregate.dataset} count ≥ ${rule.aggregate.threshold} / ${rule.aggregate.lookback_minutes}m`
                    : rule.triggers.join(' · ').replaceAll('_', ' ')
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
          <p class="eyebrow">Durable history</p>
          <h2>Recent deliveries</h2>
          <p>Attempts and terminal provider errors remain visible across restarts.</p>
        </div>
      </div>
      <EmptyState
        v-if="!deliveries.data.value?.items.length"
        icon="history"
        title="No deliveries yet"
        description="Send a test or wait for a matching alert."
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
              {{ delivery.attempts }} attempt{{ delivery.attempts === 1 ? '' : 's' }}
              <template v-if="delivery.last_error"> В· {{ delivery.last_error }}</template>
            </p>
          </div>
          <StatusBadge :status="delivery.status" />
        </article>
      </div>
    </section>
  </template>
</template>
