<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import type { CronMonitor, MonitorInput } from '../api/types';
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
const selectedMonitorId = ref('');
const kindOptions: SelectOption[] = [
  {
    value: 'cron',
    label: 'Cron check-in',
    description: 'SDK reports job state.',
    icon: 'monitors',
  },
  {
    value: 'uptime',
    label: 'Uptime HTTP',
    description: 'Faultkeep performs a safe GET or HEAD.',
    icon: 'activity',
  },
];
const methodOptions: SelectOption[] = [
  { value: 'GET', label: 'GET', description: 'Read a bounded response.', icon: 'activity' },
  { value: 'HEAD', label: 'HEAD', description: 'Headers only.', icon: 'activity' },
];
const scheduleTypeOptions: SelectOption[] = [
  {
    value: 'crontab',
    label: 'Cron expression (UTC)',
    description: 'Five numeric fields: minute, hour, day, month, weekday.',
    icon: 'monitors',
  },
  {
    value: 'interval',
    label: 'Interval in minutes',
    description: 'Run every fixed number of minutes.',
    icon: 'refresh',
  },
];
const form = reactive<MonitorInput>({
  kind: 'cron',
  slug: '',
  name: '',
  environment: 'production',
  enabled: true,
  schedule_type: 'crontab',
  schedule: '*/5 * * * *',
  checkin_margin_seconds: 60,
  max_runtime_seconds: 900,
  endpoint: 'https://example.com/health',
  method: 'GET',
  expected_status_min: 200,
  expected_status_max: 399,
  timeout_seconds: 10,
  max_redirects: 3,
  headers: [],
});

const monitors = useQuery({
  queryKey: computed(() => ['monitors', projectId.value]),
  queryFn: () => api.monitors(projectId.value),
  enabled: computed(() => Boolean(projectId.value)),
  refetchInterval: 15_000,
});
const selectedMonitor = computed(
  () =>
    monitors.data.value?.items.find((monitor) => monitor.id === selectedMonitorId.value) ?? null,
);
const runs = useQuery({
  queryKey: computed(() => ['monitor-runs', projectId.value, selectedMonitorId.value]),
  queryFn: () => api.monitorRuns(projectId.value, selectedMonitorId.value),
  enabled: computed(() => Boolean(projectId.value && selectedMonitorId.value)),
  refetchInterval: 10_000,
});

watch(
  () => monitors.data.value?.items,
  (items) => {
    if (items?.length && !items.some((monitor) => monitor.id === selectedMonitorId.value)) {
      selectedMonitorId.value = items[0].id;
    }
  },
  { immediate: true },
);

const saveMonitor = useMutation({
  mutationFn: () =>
    api.putMonitor(projectId.value, {
      ...form,
      slug: form.slug.trim(),
      name: form.name.trim(),
      environment: form.environment.trim(),
      schedule: form.schedule.trim(),
      schedule_type: form.kind === 'uptime' ? 'interval' : form.schedule_type,
      headers: form.headers
        .filter((header) => header.name.trim() && header.value)
        .map((header) => ({ name: header.name.trim(), value: header.value })),
    }),
  onSuccess: async (monitor) => {
    selectedMonitorId.value = monitor.id;
    await queryClient.invalidateQueries({ queryKey: ['monitors', projectId.value] });
  },
});

function edit(monitor: CronMonitor): void {
  selectedMonitorId.value = monitor.id;
  Object.assign(form, {
    slug: monitor.slug,
    name: monitor.name,
    environment: monitor.environment,
    enabled: monitor.enabled,
    kind: monitor.kind,
    schedule_type: monitor.schedule_type,
    schedule: monitor.schedule,
    checkin_margin_seconds: monitor.checkin_margin_seconds,
    max_runtime_seconds: monitor.max_runtime_seconds,
    endpoint: monitor.uptime?.endpoint ?? 'https://example.com/health',
    method: monitor.uptime?.method ?? 'GET',
    expected_status_min: monitor.uptime?.expected_status_min ?? 200,
    expected_status_max: monitor.uptime?.expected_status_max ?? 399,
    timeout_seconds: monitor.uptime?.timeout_seconds ?? 10,
    max_redirects: monitor.uptime?.max_redirects ?? 3,
    headers: monitor.uptime?.headers.map((header) => ({ name: header.name, value: '' })) ?? [],
  });
}

function addHeader(): void {
  if (form.headers.length < 16) form.headers.push({ name: '', value: '' });
}

function removeHeader(index: number): void {
  form.headers.splice(index, 1);
}

function setKind(kind: MonitorInput['kind']): void {
  form.kind = kind;
  if (kind === 'uptime' && form.schedule_type === 'crontab') {
    form.schedule_type = 'interval';
    form.schedule = '5';
  }
}

function timestamp(value: string | null): string {
  if (!value) return 'Not observed';
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'medium',
  }).format(new Date(value));
}

function duration(value: number | null): string {
  if (value === null) return '—';
  if (value < 1_000) return `${value} ms`;
  return `${(value / 1_000).toFixed(2)} s`;
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / scheduled jobs</p>
        <h1>Monitors</h1>
        <p>Track SDK cron jobs and safe server-originated HTTP uptime checks.</p>
      </div>
    </header>

    <ApiErrorPanel
      v-if="monitors.error.value"
      :error="monitors.error.value"
      title="Cron monitors could not be loaded"
      @retry="monitors.refetch()"
    />
    <ApiErrorPanel
      v-if="saveMonitor.error.value"
      :error="saveMonitor.error.value"
      title="Cron monitor was not saved"
    />

    <form
      v-if="session.has('project:admin')"
      class="panel settings-form monitor-form"
      @submit.prevent="saveMonitor.mutate()"
    >
      <div class="section-heading">
        <div class="section-heading__content">
          <span class="section-icon section-icon--info"><AppIcon name="monitors" /></span>
          <div>
            <p class="eyebrow">Definition</p>
            <h2>Create or update a monitor</h2>
            <p>The same slug and environment update one stable monitor.</p>
          </div>
        </div>
      </div>
      <div class="form-grid form-grid--three">
        <BaseSelect
          :model-value="form.kind"
          :options="kindOptions"
          label="Monitor type"
          @update:model-value="setKind($event as MonitorInput['kind'])"
        />
        <label>
          Name
          <input v-model.trim="form.name" maxlength="128" required placeholder="Nightly backup" />
        </label>
        <label>
          Slug
          <input v-model.trim="form.slug" maxlength="64" required placeholder="nightly-backup" />
        </label>
        <label>
          Environment
          <input v-model.trim="form.environment" maxlength="64" required placeholder="production" />
        </label>
      </div>
      <div v-if="form.kind === 'cron'" class="form-grid">
        <BaseSelect
          :model-value="form.schedule_type"
          :options="scheduleTypeOptions"
          label="Schedule type"
          @update:model-value="form.schedule_type = $event as MonitorInput['schedule_type']"
        />
        <label>
          {{ form.schedule_type === 'crontab' ? 'Cron expression (UTC)' : 'Interval minutes' }}
          <input
            v-model.trim="form.schedule"
            required
            :placeholder="form.schedule_type === 'crontab' ? '*/5 * * * *' : '5'"
          />
        </label>
      </div>
      <div v-else class="form-grid">
        <label>
          Public HTTP(S) endpoint
          <input v-model.trim="form.endpoint" type="url" maxlength="2048" required />
          <small
            >Private, loopback, link-local and metadata addresses are rejected on every hop.</small
          >
        </label>
        <BaseSelect
          :model-value="form.method ?? 'GET'"
          :options="methodOptions"
          label="Method"
          @update:model-value="form.method = $event as 'GET' | 'HEAD'"
        />
      </div>
      <div class="form-grid form-grid--three">
        <label v-if="form.kind === 'cron'">
          Check-in margin, seconds
          <input v-model.number="form.checkin_margin_seconds" type="number" min="0" required />
        </label>
        <label v-if="form.kind === 'cron'">
          Maximum runtime, seconds
          <input v-model.number="form.max_runtime_seconds" type="number" min="1" required />
        </label>
        <label class="check-control monitor-form__enabled">
          <input v-model="form.enabled" type="checkbox" />
          <span><strong>Enabled</strong><small>Pause without deleting history.</small></span>
        </label>
      </div>
      <template v-if="form.kind === 'uptime'">
        <div class="form-grid form-grid--three">
          <label
            >Expected from<input
              v-model.number="form.expected_status_min"
              type="number"
              min="100"
              max="599"
              required
          /></label>
          <label
            >Expected through<input
              v-model.number="form.expected_status_max"
              type="number"
              min="100"
              max="599"
              required
          /></label>
          <label
            >Timeout, seconds<input
              v-model.number="form.timeout_seconds"
              type="number"
              min="1"
              max="120"
              required
          /></label>
        </div>
        <div class="form-grid">
          <label
            >Interval, minutes<input v-model.trim="form.schedule" required placeholder="5"
          /></label>
          <label
            >Maximum redirects<input
              v-model.number="form.max_redirects"
              type="number"
              min="0"
              max="3"
              required
          /></label>
        </div>
        <div class="section-heading">
          <div>
            <p class="eyebrow">Write-only custom headers</p>
            <p>Values are sealed. A blank value removes that header when you save.</p>
          </div>
          <button
            class="button button--secondary button--fit"
            type="button"
            :disabled="form.headers.length >= 16"
            @click="addHeader"
          >
            <AppIcon name="plus" :size="16" /> Add header
          </button>
        </div>
        <div
          v-for="(header, index) in form.headers"
          :key="index"
          class="form-grid form-grid--three"
        >
          <label
            >Header name<input
              v-model.trim="header.name"
              maxlength="64"
              placeholder="authorization"
          /></label>
          <label
            >Secret value<input
              v-model="header.value"
              type="password"
              maxlength="2048"
              autocomplete="new-password"
              placeholder="Write-only"
          /></label>
          <button
            class="button button--secondary button--fit"
            type="button"
            @click="removeHeader(index)"
          >
            <AppIcon name="delete" :size="16" /> Remove
          </button>
        </div>
      </template>
      <button
        class="button button--primary button--fit"
        type="submit"
        :disabled="saveMonitor.isPending.value"
      >
        <AppIcon name="save" :size="16" />
        {{ saveMonitor.isPending.value ? 'Saving…' : 'Save monitor' }}
      </button>
    </form>

    <LoadingPanel v-if="monitors.isPending.value" label="Loading cron monitors…" />
    <EmptyState
      v-else-if="!monitors.data.value?.items.length"
      icon="monitors"
      title="No cron check-ins yet"
      description="Create a monitor here or send a check_in item with a Sentry SDK."
    />
    <div v-else class="monitor-layout">
      <section class="monitor-list" aria-label="Cron monitors">
        <button
          v-for="monitor in monitors.data.value?.items"
          :key="monitor.id"
          class="panel monitor-card"
          :class="{ 'monitor-card--selected': selectedMonitorId === monitor.id }"
          type="button"
          @click="edit(monitor)"
        >
          <span class="monitor-card__icon"><AppIcon name="monitors" /></span>
          <span class="monitor-card__copy">
            <strong>{{ monitor.name }}</strong>
            <small
              >{{ monitor.kind === 'uptime' ? 'Uptime' : 'Cron' }} · {{ monitor.slug }} ·
              {{ monitor.environment }}</small
            >
          </span>
          <StatusBadge
            :status="monitor.enabled ? (monitor.last_status ?? 'waiting') : 'disabled'"
          />
          <small>Next {{ timestamp(monitor.next_expected_at) }}</small>
        </button>
      </section>

      <section v-if="selectedMonitor" class="panel monitor-history">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Run history · TTL retained</p>
            <h2>{{ selectedMonitor.name }}</h2>
            <p>
              Managed by {{ selectedMonitor.managed_by }} · last check-in
              {{ timestamp(selectedMonitor.last_check_in_at) }}
            </p>
          </div>
          <StatusBadge :status="selectedMonitor.last_status ?? 'waiting'" />
        </div>
        <LoadingPanel v-if="runs.isPending.value" label="Loading run history…" />
        <ApiErrorPanel
          v-else-if="runs.error.value"
          :error="runs.error.value"
          title="Run history could not be loaded"
          @retry="runs.refetch()"
        />
        <EmptyState
          v-else-if="!runs.data.value?.items.length"
          icon="history"
          title="No executions recorded"
          description="The monitor definition exists, but no SDK check-in or scheduler outcome exists yet."
        />
        <ol v-else class="timeline monitor-run-list">
          <li v-for="run in runs.data.value?.items" :key="run.id">
            <span class="timeline__dot"></span>
            <div>
              <div class="monitor-run__title">
                <StatusBadge :status="run.status" />
                <strong>{{ timestamp(run.started_at) }}</strong>
                <span>{{ duration(run.duration_ms) }}</span>
              </div>
              <p>
                {{ run.source === 'sdk' ? 'Reported by SDK' : 'Detected by scheduler' }}
                <template v-if="run.scheduled_for">
                  · scheduled {{ timestamp(run.scheduled_for) }}
                </template>
              </p>
              <p v-if="run.http_status || run.uptime_failure">
                <span v-if="run.http_status">HTTP {{ run.http_status }}</span>
                <span v-if="run.uptime_failure"> · {{ run.uptime_failure }}</span>
              </p>
            </div>
          </li>
        </ol>
      </section>
    </div>
  </section>
</template>
