<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import type { CronMonitor, MonitorInput, MonitorRun } from '../api/types';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { sampleMonitorTimeline } from '../lib/monitorRuns';
import { suggestedSlug } from '../lib/slug';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const queryClient = useQueryClient();
const { locale, t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const selectedMonitorId = ref('');
const editorOpen = ref(false);
const historyView = ref<'list' | 'chart'>(storedHistoryView());
const historyRange = ref('all');
const appliedHistoryWindow = ref<{ from?: number; until?: number }>({});
const runCursor = ref<string | null>(null);
const runPageHistory = ref<(string | null)[]>([]);
const deleteConfirmationId = ref('');
const selectedChartRunId = ref('');
const slugWasEdited = ref(false);
const customHistoryFrom = ref(localDateTime(Date.now() - 7 * 24 * 60 * 60 * 1_000));
const customHistoryUntil = ref(localDateTime(Date.now()));
const customHistoryError = ref('');
const kindOptions = computed<SelectOption[]>(() => [
  {
    value: 'cron',
    label: t('monitors.cronCheckIn'),
    description: t('monitors.cronCheckInHelp'),
    icon: 'monitors',
  },
  {
    value: 'uptime',
    label: t('monitors.uptimeHttp'),
    description: t('monitors.uptimeHttpHelp'),
    icon: 'activity',
  },
]);
const methodOptions = computed<SelectOption[]>(() => [
  { value: 'GET', label: 'GET', description: t('monitors.getHelp'), icon: 'activity' },
  { value: 'HEAD', label: 'HEAD', description: t('monitors.headHelp'), icon: 'activity' },
]);
const scheduleTypeOptions = computed<SelectOption[]>(() => [
  {
    value: 'crontab',
    label: t('monitors.cronExpression'),
    description: t('monitors.cronExpressionHelp'),
    icon: 'monitors',
  },
  {
    value: 'interval',
    label: t('monitors.intervalMinutes'),
    description: t('monitors.intervalMinutesHelp'),
    icon: 'refresh',
  },
]);
const historyRangeOptions = computed<SelectOption[]>(() => [
  { value: 'all', label: t('timeRange.all'), icon: 'history' },
  { value: '24h', label: t('timeRange.hours24'), icon: 'history' },
  { value: '7d', label: t('timeRange.days7'), icon: 'history' },
  { value: '30d', label: t('timeRange.days30'), icon: 'history' },
  { value: 'custom', label: t('monitors.customPeriod'), icon: 'settings' },
]);
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
const monitorDefinitionOptions = computed<SelectOption[]>(() => [
  { value: '', label: t('monitors.newMonitor'), icon: 'plus' },
  ...(monitors.data.value?.items ?? []).map((monitor) => ({
    value: monitor.id,
    label: monitor.name,
    description: `${monitor.slug} · ${monitor.environment}`,
    icon: 'monitors' as const,
  })),
]);
const runs = useQuery({
  queryKey: computed(() => [
    'monitor-runs',
    projectId.value,
    selectedMonitorId.value,
    historyView.value,
    historyRange.value,
    appliedHistoryWindow.value.from,
    appliedHistoryWindow.value.until,
    runCursor.value,
  ]),
  queryFn: () =>
    api.monitorRuns(
      projectId.value,
      selectedMonitorId.value,
      appliedHistoryWindow.value,
      historyView.value === 'list' ? { cursor: runCursor.value, limit: 100 } : { limit: 100000 },
    ),
  enabled: computed(() => Boolean(projectId.value && selectedMonitorId.value)),
  refetchInterval: 10_000,
});
const listRuns = computed(() => runs.data.value?.items ?? []);
const timelineRuns = computed(() => sampleMonitorTimeline(runs.data.value?.items ?? []));
const chartRuns = computed(() => [...timelineRuns.value].reverse());
const maximumRunDuration = computed(() =>
  Math.max(1, ...timelineRuns.value.map((run) => run.duration_ms ?? 0)),
);
const selectedChartRun = computed(
  () => timelineRuns.value.find((run) => run.id === selectedChartRunId.value) ?? null,
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
watch(selectedMonitorId, () => {
  resetRunPage();
  deleteConfirmationId.value = '';
  selectedChartRunId.value = '';
});
watch(historyRange, (range) => {
  if (range !== 'custom') appliedHistoryWindow.value = historyWindow(range);
  resetRunPage();
  selectedChartRunId.value = '';
});
watch(
  () => form.name,
  (name) => {
    if (!slugWasEdited.value) form.slug = suggestedSlug(name);
  },
);
watch(historyView, (value) => {
  try {
    window.localStorage.setItem('metric.monitor-history-view', value);
  } catch {
    // The history remains usable when browser storage is blocked.
  }
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
  slugWasEdited.value = true;
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

function toggleEditor(): void {
  if (editorOpen.value) {
    editorOpen.value = false;
    return;
  }
  if (selectedMonitor.value) {
    edit(selectedMonitor.value);
  } else {
    editorOpen.value = true;
  }
}

function manageMonitor(monitorId: string): void {
  if (!monitorId) {
    selectedMonitorId.value = '';
    slugWasEdited.value = false;
    Object.assign(form, {
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
    return;
  }
  const monitor = monitors.data.value?.items.find((item) => item.id === monitorId);
  if (monitor) {
    edit(monitor);
  }
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
  if (!value) return t('monitors.notObserved');
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'medium',
  }).format(new Date(value));
}

function duration(value: number | null): string {
  if (value === null) return '—';
  if (value < 1_000) return `${value} ms`;
  return `${new Intl.NumberFormat(locale.value, { maximumFractionDigits: 2 }).format(
    value / 1_000,
  )} s`;
}

function runStatusLabel(status: string): string {
  return t(`status.${status}`);
}

function historyRangeLabel(): string {
  return (
    historyRangeOptions.value.find((option) => option.value === historyRange.value)?.label ?? ''
  );
}

function managerLabel(manager: CronMonitor['managed_by']): string {
  return t(manager === 'sdk' ? 'monitors.managedSdk' : 'monitors.managedWeb');
}

function runBarHeight(run: MonitorRun): string {
  const ratio = (run.duration_ms ?? 0) / maximumRunDuration.value;
  return `${Math.max(18, Math.round(24 + ratio * 72))}px`;
}

function historyWindow(range: string): { from?: number; until?: number } {
  if (range === 'all') return {};
  const until = Date.now();
  return {
    from: until - historyRangeMillis[range],
    until,
  };
}

function resetRunPage(): void {
  runCursor.value = null;
  runPageHistory.value = [];
}

function nextRunPage(): void {
  const next = runs.data.value?.next_cursor;
  if (!next) return;
  runPageHistory.value.push(runCursor.value);
  runCursor.value = next;
}

function previousRunPage(): void {
  runCursor.value = runPageHistory.value.pop() ?? null;
}

function applyCustomHistory(): void {
  const from = new Date(customHistoryFrom.value).getTime();
  const until = new Date(customHistoryUntil.value).getTime();
  if (!Number.isFinite(from) || !Number.isFinite(until) || from >= until) {
    customHistoryError.value = t('monitors.invalidPeriod');
    return;
  }
  customHistoryError.value = '';
  appliedHistoryWindow.value = { from, until };
  resetRunPage();
}

function localDateTime(value: number): string {
  const date = new Date(value);
  return new Date(value - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}

function storedHistoryView(): 'list' | 'chart' {
  try {
    return window.localStorage.getItem('metric.monitor-history-view') === 'chart'
      ? 'chart'
      : 'list';
  } catch {
    return 'list';
  }
}
</script>

<template>
  <section class="monitors-page">
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{ $t('monitors.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('monitors.title') }}</h1>
        <p>{{ $t('monitors.description') }}</p>
      </div>
      <button
        v-if="session.has('project:admin')"
        class="button button--secondary"
        type="button"
        :aria-pressed="editorOpen"
        @click="toggleEditor"
      >
        <AppIcon :name="editorOpen ? 'close' : 'plus'" :size="16" />
        {{ editorOpen ? $t('monitors.closeEditor') : $t('monitors.manage') }}
      </button>
    </header>

    <ApiErrorPanel
      v-if="monitors.error.value"
      class="monitor-error"
      :error="monitors.error.value"
      :title="$t('monitors.loadFailed')"
      @retry="monitors.refetch()"
    />
    <ApiErrorPanel
      v-if="saveMonitor.error.value"
      class="monitor-error"
      :error="saveMonitor.error.value"
      :title="$t('monitors.saveFailed')"
    />
    <ApiErrorPanel
      v-if="deleteMonitor.error.value"
      class="monitor-error"
      :error="deleteMonitor.error.value"
      :title="$t('monitors.deleteFailed')"
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
            <p class="eyebrow">{{ $t('monitors.definition') }}</p>
            <h2>{{ $t('monitors.createOrUpdate') }}</h2>
            <p>{{ $t('monitors.stableIdentity') }}</p>
          </div>
        </div>
      </div>
      <BaseSelect
        :model-value="selectedMonitorId"
        :options="monitorDefinitionOptions"
        :label="$t('monitors.monitorDefinition')"
        @update:model-value="manageMonitor"
      />
      <div class="form-grid form-grid--three">
        <BaseSelect
          :model-value="form.kind"
          :options="kindOptions"
          :label="$t('monitors.monitorType')"
          @update:model-value="setKind($event as MonitorInput['kind'])"
        />
        <label>
          {{ $t('monitors.name') }}
          <input v-model.trim="form.name" maxlength="128" required placeholder="Nightly backup" />
        </label>
        <label>
          {{ $t('monitors.slug') }}
          <input
            v-model.trim="form.slug"
            maxlength="64"
            required
            placeholder="nightly-backup"
            @input="slugWasEdited = true"
          />
        </label>
        <label>
          {{ $t('monitors.environment') }}
          <input v-model.trim="form.environment" maxlength="64" required placeholder="production" />
        </label>
      </div>
      <div v-if="form.kind === 'cron'" class="form-grid">
        <BaseSelect
          :model-value="form.schedule_type"
          :options="scheduleTypeOptions"
          :label="$t('monitors.scheduleType')"
          @update:model-value="form.schedule_type = $event as MonitorInput['schedule_type']"
        />
        <label>
          {{
            form.schedule_type === 'crontab'
              ? $t('monitors.cronExpression')
              : $t('monitors.intervalLabel')
          }}
          <input
            v-model.trim="form.schedule"
            required
            :placeholder="form.schedule_type === 'crontab' ? '*/5 * * * *' : '5'"
          />
        </label>
      </div>
      <div v-else class="form-grid">
        <label>
          {{ $t('monitors.endpoint') }}
          <input v-model.trim="form.endpoint" type="url" maxlength="2048" required />
          <small>{{ $t('monitors.endpointHelp') }}</small>
        </label>
        <BaseSelect
          :model-value="form.method ?? 'GET'"
          :options="methodOptions"
          :label="$t('monitors.method')"
          @update:model-value="form.method = $event as 'GET' | 'HEAD'"
        />
      </div>
      <div class="form-grid form-grid--three">
        <label v-if="form.kind === 'cron'">
          {{ $t('monitors.margin') }}
          <input v-model.number="form.checkin_margin_seconds" type="number" min="0" required />
        </label>
        <label v-if="form.kind === 'cron'">
          {{ $t('monitors.maxRuntime') }}
          <input v-model.number="form.max_runtime_seconds" type="number" min="1" required />
        </label>
        <label class="check-control monitor-form__enabled">
          <input v-model="form.enabled" type="checkbox" />
          <span
            ><strong>{{ $t('monitors.enabled') }}</strong
            ><small>{{ $t('monitors.enabledHelp') }}</small></span
          >
        </label>
      </div>
      <template v-if="form.kind === 'uptime'">
        <div class="form-grid form-grid--three">
          <label>
            {{ $t('monitors.expectedFrom')
            }}<input
              v-model.number="form.expected_status_min"
              type="number"
              min="100"
              max="599"
              required
            />
          </label>
          <label>
            {{ $t('monitors.expectedThrough')
            }}<input
              v-model.number="form.expected_status_max"
              type="number"
              min="100"
              max="599"
              required
            />
          </label>
          <label>
            {{ $t('monitors.timeoutSeconds')
            }}<input
              v-model.number="form.timeout_seconds"
              type="number"
              min="1"
              max="120"
              required
            />
          </label>
        </div>
        <div class="form-grid">
          <label>
            {{ $t('monitors.interval')
            }}<input v-model.trim="form.schedule" required placeholder="5" />
          </label>
          <label>
            {{ $t('monitors.maxRedirects')
            }}<input v-model.number="form.max_redirects" type="number" min="0" max="3" required />
          </label>
        </div>
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('monitors.customHeaders') }}</p>
            <p>{{ $t('monitors.customHeadersHelp') }}</p>
          </div>
          <button
            class="button button--secondary button--fit"
            type="button"
            :disabled="form.headers.length >= 16"
            @click="addHeader"
          >
            <AppIcon name="plus" :size="16" /> {{ $t('monitors.addHeader') }}
          </button>
        </div>
        <div
          v-for="(header, index) in form.headers"
          :key="index"
          class="form-grid form-grid--three"
        >
          <label>
            {{ $t('monitors.headerName')
            }}<input v-model.trim="header.name" maxlength="64" placeholder="authorization" />
          </label>
          <label>
            {{ $t('monitors.secretValue')
            }}<input
              v-model="header.value"
              type="password"
              maxlength="2048"
              autocomplete="new-password"
              :placeholder="$t('monitors.writeOnly')"
            />
          </label>
          <button
            class="button button--secondary button--fit"
            type="button"
            @click="removeHeader(index)"
          >
            <AppIcon name="delete" :size="16" /> {{ $t('monitors.remove') }}
          </button>
        </div>
      </template>
      <div class="compact-actions monitor-form__actions">
        <button
          class="button button--primary button--fit"
          type="submit"
          :disabled="saveMonitor.isPending.value"
        >
          <AppIcon name="save" :size="16" />
          {{ saveMonitor.isPending.value ? $t('monitors.saving') : $t('monitors.save') }}
        </button>
        <button
          v-if="selectedMonitor"
          class="button button--danger button--fit"
          type="button"
          :disabled="deleteMonitor.isPending.value"
          @click="confirmDelete(selectedMonitor.id)"
        >
          <AppIcon name="delete" :size="15" />
          {{
            deleteConfirmationId === selectedMonitor.id
              ? $t('monitors.confirmDelete')
              : $t('monitors.delete')
          }}
        </button>
      </div>
    </form>

    <LoadingPanel
      v-if="monitors.isPending.value"
      class="monitor-loading"
      :label="$t('monitors.loading')"
    />
    <EmptyState
      v-else-if="!monitors.data.value?.items.length"
      class="monitor-empty"
      icon="monitors"
      :title="$t('monitors.empty')"
      :description="$t('monitors.emptyDescription')"
    >
      <button
        v-if="session.has('project:admin')"
        class="button button--primary"
        type="button"
        @click="editorOpen = true"
      >
        <AppIcon name="plus" :size="16" />
        {{ $t('monitors.create') }}
      </button>
    </EmptyState>
    <div v-else class="monitor-layout">
      <aside class="monitor-browser">
        <section class="monitor-list" :aria-label="$t('monitors.listLabel')">
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
                >{{ monitor.kind === 'uptime' ? $t('monitors.uptime') : $t('monitors.cron') }} ·
                {{ monitor.slug }} · {{ monitor.environment }}</small
              >
            </span>
            <StatusBadge
              :status="monitor.enabled ? (monitor.last_status ?? 'waiting') : 'disabled'"
            />
            <small>{{ $t('monitors.next', { time: timestamp(monitor.next_expected_at) }) }}</small>
          </button>
        </section>
      </aside>

      <section v-if="selectedMonitor" class="panel monitor-history">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('monitors.history') }}</p>
            <h2>{{ selectedMonitor.name }}</h2>
            <p>
              {{
                $t('monitors.managed', {
                  manager: managerLabel(selectedMonitor.managed_by),
                  time: timestamp(selectedMonitor.last_check_in_at),
                })
              }}
            </p>
          </div>
          <div class="monitor-history__actions">
            <StatusBadge :status="selectedMonitor.last_status ?? 'waiting'" />
          </div>
        </div>
        <div class="monitor-history-controls">
          <div class="section-tabs" role="group" :aria-label="$t('monitors.presentation')">
            <button
              class="button"
              :class="historyView === 'list' ? 'button--primary' : 'button--secondary'"
              type="button"
              :aria-pressed="historyView === 'list'"
              @click="historyView = 'list'"
            >
              <AppIcon name="clipboard" :size="15" />
              {{ $t('monitors.list') }}
            </button>
            <button
              class="button"
              :class="historyView === 'chart' ? 'button--primary' : 'button--secondary'"
              type="button"
              :aria-pressed="historyView === 'chart'"
              @click="historyView = 'chart'"
            >
              <AppIcon name="activity" :size="15" />
              {{ $t('monitors.timeline') }}
            </button>
          </div>
          <BaseSelect
            v-model="historyRange"
            class="monitor-history-range"
            :options="historyRangeOptions"
            :aria-label="$t('monitors.historyPeriod')"
          />
        </div>
        <form
          v-if="historyRange === 'custom'"
          class="monitor-custom-range"
          @submit.prevent="applyCustomHistory"
        >
          <label>
            {{ $t('monitors.from') }}
            <input v-model="customHistoryFrom" type="datetime-local" required />
          </label>
          <label>
            {{ $t('monitors.until') }}
            <input v-model="customHistoryUntil" type="datetime-local" required />
          </label>
          <button class="button button--secondary button--fit" type="submit">
            <AppIcon name="search" :size="15" />
            {{ $t('monitors.applyPeriod') }}
          </button>
          <small v-if="customHistoryError" class="field-error" role="alert">
            {{ customHistoryError }}
          </small>
        </form>
        <LoadingPanel v-if="runs.isPending.value" :label="$t('monitors.loadingHistory')" />
        <ApiErrorPanel
          v-else-if="runs.error.value"
          :error="runs.error.value"
          :title="$t('monitors.historyFailed')"
          @retry="runs.refetch()"
        />
        <EmptyState
          v-else-if="!runs.data.value?.items.length"
          icon="history"
          :title="$t('monitors.noExecutions')"
          :description="$t('monitors.noExecutionsHelp')"
        />
        <div v-else-if="historyView === 'list'" class="monitor-run-list-view">
          <nav class="pagination" :aria-label="$t('monitors.historyPages')">
            <button
              class="button button--secondary"
              type="button"
              :disabled="runPageHistory.length === 0"
              @click="previousRunPage"
            >
              {{ $t('common.previous') }}
            </button>
            <span>{{ $t('common.page', { page: runPageHistory.length + 1 }) }}</span>
            <button
              class="button button--secondary"
              type="button"
              :disabled="!runs.data.value?.next_cursor"
              @click="nextRunPage"
            >
              {{ $t('common.next') }}
            </button>
          </nav>
          <ol class="timeline monitor-run-list">
            <li v-for="run in listRuns" :key="run.id">
              <span class="timeline__dot"></span>
              <div>
                <div class="monitor-run__title">
                  <StatusBadge :status="run.status" />
                  <strong>{{ timestamp(run.started_at) }}</strong>
                  <span>{{ duration(run.duration_ms) }}</span>
                </div>
                <p>
                  {{
                    run.source === 'sdk'
                      ? $t('monitors.reportedSdk')
                      : $t('monitors.detectedScheduler')
                  }}
                  <template v-if="run.scheduled_for">
                    · {{ $t('monitors.scheduled', { time: timestamp(run.scheduled_for) }) }}
                  </template>
                </p>
                <p v-if="run.http_status || run.uptime_failure">
                  <span v-if="run.http_status">HTTP {{ run.http_status }}</span>
                  <span v-if="run.uptime_failure"> · {{ run.uptime_failure }}</span>
                </p>
              </div>
            </li>
          </ol>
        </div>
        <div v-else class="monitor-run-chart">
          <div class="monitor-run-chart__legend" aria-hidden="true">
            <span class="monitor-run-chart__key monitor-run-chart__key--success">
              {{ $t('monitors.success') }}
            </span>
            <span class="monitor-run-chart__key monitor-run-chart__key--failure">
              {{ $t('monitors.failure') }}
            </span>
            <span class="monitor-run-chart__key monitor-run-chart__key--progress">
              {{ $t('monitors.inProgress') }}
            </span>
          </div>
          <div
            class="monitor-run-chart__plot"
            role="list"
            :aria-label="
              $t('monitors.chartLabel', {
                count: chartRuns.length,
                range: historyRangeLabel(),
              })
            "
          >
            <button
              v-for="run in chartRuns"
              :key="run.id"
              class="monitor-run-chart__column"
              :class="[
                `monitor-run-chart__column--${run.status}`,
                { 'monitor-run-chart__column--selected': selectedChartRunId === run.id },
              ]"
              :style="{ '--run-height': runBarHeight(run) }"
              role="listitem"
              :data-started-at="run.started_at"
              :aria-label="
                $t('monitors.runLabel', {
                  status: runStatusLabel(run.status),
                  time: timestamp(run.started_at),
                  duration: duration(run.duration_ms),
                })
              "
              :aria-pressed="selectedChartRunId === run.id"
              :title="
                $t('monitors.runLabel', {
                  status: runStatusLabel(run.status),
                  time: timestamp(run.started_at),
                  duration: duration(run.duration_ms),
                })
              "
              type="button"
              @click="selectedChartRunId = run.id"
            ></button>
          </div>
          <div v-if="chartRuns.length" class="monitor-run-chart__axis">
            <span>{{ timestamp(chartRuns[0].started_at) }}</span>
            <span>{{ timestamp(chartRuns[chartRuns.length - 1].started_at) }}</span>
          </div>
          <article v-if="selectedChartRun" class="monitor-run-chart__details">
            <div class="monitor-run__title">
              <StatusBadge :status="selectedChartRun.status" />
              <strong>{{ timestamp(selectedChartRun.started_at) }}</strong>
              <span>{{ duration(selectedChartRun.duration_ms) }}</span>
            </div>
            <p>
              {{
                selectedChartRun.source === 'sdk'
                  ? $t('monitors.reportedSdk')
                  : $t('monitors.detectedScheduler')
              }}
              <template v-if="selectedChartRun.scheduled_for">
                ·
                {{ $t('monitors.scheduled', { time: timestamp(selectedChartRun.scheduled_for) }) }}
              </template>
            </p>
            <p v-if="selectedChartRun.http_status || selectedChartRun.uptime_failure">
              <span v-if="selectedChartRun.http_status">
                HTTP {{ selectedChartRun.http_status }}
              </span>
              <span v-if="selectedChartRun.uptime_failure">
                · {{ selectedChartRun.uptime_failure }}
              </span>
            </p>
          </article>
        </div>
      </section>
    </div>
  </section>
</template>
