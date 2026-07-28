<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import type { CronMonitor, MonitorInput, MonitorRun } from '../api/types';
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
const editorOpen = ref(false);
const historyView = ref<'list' | 'chart'>('list');
const historyRange = ref('24h');
const deleteConfirmationId = ref('');
const customHistoryFrom = ref(localDateTime(Date.now() - 7 * 24 * 60 * 60 * 1_000));
const customHistoryUntil = ref(localDateTime(Date.now()));
const appliedCustomFrom = ref(Date.now() - 7 * 24 * 60 * 60 * 1_000);
const appliedCustomUntil = ref(Date.now());
const customHistoryError = ref('');
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
const historyRangeOptions: SelectOption[] = [
  { value: '24h', label: 'Last 24 hours', icon: 'history' },
  { value: '7d', label: 'Last 7 days', icon: 'history' },
  { value: '30d', label: 'Last 30 days', icon: 'history' },
  { value: 'custom', label: 'Custom period', icon: 'settings' },
];
const historyRangeMillis: Record<string, number> = {
  '24h': 24 * 60 * 60 * 1_000,
  '7d': 7 * 24 * 60 * 60 * 1_000,
  '30d': 30 * 24 * 60 * 60 * 1_000,
};
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
  queryKey: computed(() => [
    'monitor-runs',
    projectId.value,
    selectedMonitorId.value,
    historyRange.value,
    appliedCustomFrom.value,
    appliedCustomUntil.value,
  ]),
  queryFn: () => api.monitorRuns(projectId.value, selectedMonitorId.value, currentHistoryWindow()),
  enabled: computed(() => Boolean(projectId.value && selectedMonitorId.value)),
  refetchInterval: 10_000,
});
const chartRuns = computed(() => [...(runs.data.value?.items ?? [])].reverse());
const maximumRunDuration = computed(() =>
  Math.max(1, ...chartRuns.value.map((run) => run.duration_ms ?? 0)),
);

watch(
  () => monitors.data.value?.items,
  (items) => {
    if (items?.length && !items.some((monitor) => monitor.id === selectedMonitorId.value)) {
      selectedMonitorId.value = items[0].id;
    }
  },
  { immediate: true },
);
watch([selectedMonitorId, historyRange], () => {
  deleteConfirmationId.value = '';
});

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
    editorOpen.value = false;
    await queryClient.invalidateQueries({ queryKey: ['monitors', projectId.value] });
  },
});
const deleteMonitor = useMutation({
  mutationFn: (monitorId: string) => api.deleteMonitor(projectId.value, monitorId),
  onSuccess: async (_, monitorId) => {
    if (selectedMonitorId.value === monitorId) selectedMonitorId.value = '';
    deleteConfirmationId.value = '';
    await queryClient.invalidateQueries({ queryKey: ['monitors', projectId.value] });
    await queryClient.removeQueries({ queryKey: ['monitor-runs', projectId.value, monitorId] });
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
  editorOpen.value = true;
}

function confirmDelete(monitorId: string): void {
  if (deleteConfirmationId.value === monitorId) {
    deleteMonitor.mutate(monitorId);
    return;
  }
  deleteConfirmationId.value = monitorId;
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

function runBarHeight(run: MonitorRun): string {
  const ratio = (run.duration_ms ?? 0) / maximumRunDuration.value;
  return `${Math.max(18, Math.round(24 + ratio * 72))}px`;
}

function currentHistoryWindow(): { from: number; until: number } {
  if (historyRange.value === 'custom') {
    return { from: appliedCustomFrom.value, until: appliedCustomUntil.value };
  }
  const until = Date.now();
  return {
    from: until - historyRangeMillis[historyRange.value],
    until,
  };
}

function applyCustomHistory(): void {
  const from = new Date(customHistoryFrom.value).getTime();
  const until = new Date(customHistoryUntil.value).getTime();
  if (!Number.isFinite(from) || !Number.isFinite(until) || from >= until) {
    customHistoryError.value = 'Choose a valid start before the end of the period.';
    return;
  }
  customHistoryError.value = '';
  appliedCustomFrom.value = from;
  appliedCustomUntil.value = until;
}

function localDateTime(value: number): string {
  const date = new Date(value);
  return new Date(value - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}
</script>

<template>
  <section class="monitors-page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / scheduled jobs</p>
        <h1>Monitors</h1>
        <p>Track SDK cron jobs and safe server-originated HTTP uptime checks.</p>
      </div>
      <button
        v-if="session.has('project:admin')"
        class="button button--secondary"
        type="button"
        :aria-pressed="editorOpen"
        @click="editorOpen = !editorOpen"
      >
        <AppIcon :name="editorOpen ? 'close' : 'plus'" :size="16" />
        {{ editorOpen ? 'Close editor' : 'Create monitor' }}
      </button>
    </header>

    <ApiErrorPanel
      v-if="monitors.error.value"
      class="monitor-error"
      :error="monitors.error.value"
      title="Cron monitors could not be loaded"
      @retry="monitors.refetch()"
    />
    <ApiErrorPanel
      v-if="saveMonitor.error.value"
      class="monitor-error"
      :error="saveMonitor.error.value"
      title="Cron monitor was not saved"
    />
    <ApiErrorPanel
      v-if="deleteMonitor.error.value"
      class="monitor-error"
      :error="deleteMonitor.error.value"
      title="Monitor deletion failed"
    />

    <form
      v-if="session.has('project:admin') && editorOpen"
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

    <LoadingPanel
      v-if="monitors.isPending.value"
      class="monitor-loading"
      label="Loading cron monitors…"
    />
    <EmptyState
      v-else-if="!monitors.data.value?.items.length"
      class="monitor-empty"
      icon="monitors"
      title="No monitors yet"
      description="Create an uptime monitor here or send a check_in item with a Sentry SDK."
    >
      <button
        v-if="session.has('project:admin')"
        class="button button--primary"
        type="button"
        @click="editorOpen = true"
      >
        <AppIcon name="plus" :size="16" />
        Create monitor
      </button>
    </EmptyState>
    <div v-else class="monitor-layout">
      <section class="monitor-list" aria-label="Cron monitors">
        <button
          v-for="monitor in monitors.data.value?.items"
          :key="monitor.id"
          class="panel monitor-card"
          :class="{ 'monitor-card--selected': selectedMonitorId === monitor.id }"
          type="button"
          @click="selectedMonitorId = monitor.id"
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
          <div class="monitor-history__actions">
            <StatusBadge :status="selectedMonitor.last_status ?? 'waiting'" />
            <button
              v-if="session.has('project:admin')"
              class="button button--secondary button--fit"
              type="button"
              @click="edit(selectedMonitor)"
            >
              <AppIcon name="settings" :size="15" />
              Edit
            </button>
            <button
              v-if="session.has('project:admin')"
              class="button button--danger button--fit"
              type="button"
              :disabled="deleteMonitor.isPending.value"
              @click="confirmDelete(selectedMonitor.id)"
            >
              <AppIcon name="delete" :size="15" />
              {{
                deleteConfirmationId === selectedMonitor.id ? 'Confirm delete' : 'Delete monitor'
              }}
            </button>
          </div>
        </div>
        <div class="monitor-history-controls">
          <div class="section-tabs" role="group" aria-label="Run history presentation">
            <button
              class="button"
              :class="historyView === 'list' ? 'button--primary' : 'button--secondary'"
              type="button"
              :aria-pressed="historyView === 'list'"
              @click="historyView = 'list'"
            >
              <AppIcon name="clipboard" :size="15" />
              List
            </button>
            <button
              class="button"
              :class="historyView === 'chart' ? 'button--primary' : 'button--secondary'"
              type="button"
              :aria-pressed="historyView === 'chart'"
              @click="historyView = 'chart'"
            >
              <AppIcon name="activity" :size="15" />
              Timeline
            </button>
          </div>
          <BaseSelect
            v-model="historyRange"
            class="monitor-history-range"
            :options="historyRangeOptions"
            aria-label="Run history period"
          />
        </div>
        <form
          v-if="historyRange === 'custom'"
          class="monitor-custom-range"
          @submit.prevent="applyCustomHistory"
        >
          <label>
            From
            <input v-model="customHistoryFrom" type="datetime-local" required />
          </label>
          <label>
            Until
            <input v-model="customHistoryUntil" type="datetime-local" required />
          </label>
          <button class="button button--secondary button--fit" type="submit">
            <AppIcon name="search" :size="15" />
            Apply period
          </button>
          <small v-if="customHistoryError" class="field-error" role="alert">
            {{ customHistoryError }}
          </small>
        </form>
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
        <ol v-else-if="historyView === 'list'" class="timeline monitor-run-list">
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
        <div v-else class="monitor-run-chart">
          <div class="monitor-run-chart__legend" aria-hidden="true">
            <span class="monitor-run-chart__key monitor-run-chart__key--success">Success</span>
            <span class="monitor-run-chart__key monitor-run-chart__key--failure">Failure</span>
            <span class="monitor-run-chart__key monitor-run-chart__key--progress">In progress</span>
          </div>
          <div
            class="monitor-run-chart__plot"
            role="list"
            :aria-label="`${chartRuns.length} monitor runs over ${historyRange}`"
          >
            <span
              v-for="run in chartRuns"
              :key="run.id"
              class="monitor-run-chart__column"
              :class="`monitor-run-chart__column--${run.status}`"
              :style="{ '--run-height': runBarHeight(run) }"
              role="listitem"
              :aria-label="`${run.status}, ${timestamp(run.started_at)}, ${duration(run.duration_ms)}`"
              :title="`${timestamp(run.started_at)} · ${run.status} · ${duration(run.duration_ms)}`"
            ></span>
          </div>
          <div v-if="chartRuns.length" class="monitor-run-chart__axis">
            <span>{{ timestamp(chartRuns[0].started_at) }}</span>
            <span>{{ timestamp(chartRuns[chartRuns.length - 1].started_at) }}</span>
          </div>
        </div>
      </section>
    </div>
  </section>
</template>
