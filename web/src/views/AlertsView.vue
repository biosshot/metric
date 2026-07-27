<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const queryClient = useQueryClient();
const projectId = computed(() => session.selectedProjectId ?? '');
const kind = ref('telegram');
const ruleName = ref('');
const ruleKind = ref('issue');
const selectedDestinations = ref<string[]>([]);
const triggers = reactive({ new_issue: true, regression: true, resolved: false });
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
];
const datasetOptions: SelectOption[] = [
  { value: 'errors', label: 'Errors', icon: 'bug' },
  { value: 'logs', label: 'Logs', icon: 'logs' },
  { value: 'spans', label: 'Spans', icon: 'traces' },
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
const deliveries = useQuery({
  queryKey: computed(() => ['notification-deliveries', projectId.value]),
  queryFn: () => api.notificationDeliveries(projectId.value),
  enabled: computed(() => Boolean(projectId.value)),
  refetchInterval: 5_000,
});

const saveDestination = useMutation({
  mutationFn: () =>
    api.putNotificationDestination(projectId.value, {
      kind: kind.value,
      endpoint: destination.endpoint.trim(),
      secret: destination.secret,
      enabled: destination.enabled,
      smtp_port: kind.value === 'smtp_email' ? destination.smtp_port : null,
      smtp_security: kind.value === 'smtp_email' ? destination.smtp_security : null,
      smtp_username: kind.value === 'smtp_email' ? destination.smtp_username.trim() : null,
      smtp_from: kind.value === 'smtp_email' ? destination.smtp_from.trim() : null,
      smtp_recipients:
        kind.value === 'smtp_email'
          ? destination.smtp_recipients
              .split(',')
              .map((value) => value.trim())
              .filter(Boolean)
          : null,
    }),
  onSuccess: async (value) => {
    destination.secret = '';
    destination.endpoint = '';
    destination.smtp_username = '';
    destination.smtp_from = '';
    destination.smtp_recipients = '';
    selectedDestinations.value = [...selectedDestinations.value, value.id];
    await queryClient.invalidateQueries({
      queryKey: ['notification-destinations', projectId.value],
    });
  },
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
        ruleKind.value === 'aggregate' && aggregateRule.dataset !== 'errors'
          ? aggregateRule.environment
          : null,
      release:
        ruleKind.value === 'aggregate' && aggregateRule.dataset !== 'errors'
          ? aggregateRule.release
          : null,
      notify_resolved: ruleKind.value === 'aggregate' ? aggregateRule.notify_resolved : null,
      cooldown_minutes: aggregateRule.cooldown_minutes,
      storm_limit_per_hour: aggregateRule.storm_limit_per_hour,
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
        v-if="saveDestination.error.value"
        :error="saveDestination.error.value"
        title="Destination was not saved"
      />
      <form class="settings-form" @submit.prevent="saveDestination.mutate()">
        <BaseSelect
          :model-value="kind"
          :options="kindOptions"
          label="Provider"
          @update:model-value="kind = $event"
        />
        <div class="form-grid">
          <label>
            {{ kind === 'telegram' ? 'Chat ID' : 'SMTP host' }}
            <input
              v-model="destination.endpoint"
              required
              :placeholder="kind === 'telegram' ? '-1001234567890' : 'smtp.example.com'"
            />
          </label>
          <label>
            {{ kind === 'telegram' ? 'Bot token' : 'SMTP password' }}
            <input
              v-model="destination.secret"
              required
              type="password"
              autocomplete="new-password"
              :placeholder="kind === 'telegram' ? '123456:bot-token' : 'App password'"
            />
          </label>
        </div>
        <template v-if="kind === 'smtp_email'">
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
              Recipients
              <input
                v-model="destination.smtp_recipients"
                required
                placeholder="oncall@example.com, owner@example.com"
              />
              <small>Comma-separated, up to 16 addresses.</small>
            </label>
          </div>
        </template>
        <button
          class="button button--primary"
          type="submit"
          :disabled="saveDestination.isPending.value"
        >
          <AppIcon :name="kind === 'telegram' ? 'telegram' : 'email'" :size="16" />
          {{ saveDestination.isPending.value ? 'Saving…' : 'Save destination' }}
        </button>
      </form>
      <div v-if="destinations.data.value?.items.length" class="channel-test-list">
        <article v-for="item in destinations.data.value.items" :key="item.id">
          <AppIcon :name="item.kind === 'telegram' ? 'telegram' : 'email'" />
          <span>
            <strong>{{ item.kind === 'telegram' ? 'Telegram' : 'SMTP Email' }}</strong>
            <small>{{ item.endpoint }}</small>
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
        <div v-else class="aggregate-rule-fields">
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
              !triggers.resolved)
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
                rule.aggregate
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
