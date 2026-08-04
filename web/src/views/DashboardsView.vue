<script setup lang="ts">
import { computed, onUnmounted, reactive, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import type {
  Dashboard,
  DashboardRefresh,
  ExploreDataset,
  ExploreRequest,
  ExploreShape,
  QuerySource,
  SavedQuery,
} from '../api/types';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import UnifiedQueryBar from '../components/UnifiedQueryBar.vue';
import { timeWindow, type TimeWindow } from '../lib/timeRange';
import { randomHexId } from '../lib/randomId';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const queryClient = useQueryClient();
const { locale, t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const canWrite = computed(() => session.has('issue:write'));

const savedName = ref('');
const savedDataset = ref<ExploreDataset>('logs');
const savedShape = ref<ExploreShape>('number');
const savedQueryText = ref('');
const savedRange = ref('all');
const savedWindow = ref<TimeWindow>(timeWindow('all'));
const savedMetricMeasure = ref<'value' | 'samples'>('value');
const editingSavedId = ref('');
const dashboardName = ref('');
const editingDashboardId = ref('');
const selectedSavedQuery = ref('');
const selectedDashboardId = ref('');
const refreshInterval = ref('manual');
const draftWidgets = ref<SavedQuery[]>([]);
const editing = ref(route.query.edit === '1');
const refreshResults = reactive<Record<string, DashboardRefresh>>({});
const lastAutoRefresh = reactive<Record<string, number>>({});

const datasetOptions = computed<SelectOption[]>(() => [
  { value: 'errors', label: t('queryBuilder.errors'), icon: 'bug' },
  { value: 'logs', label: t('queryBuilder.logs'), icon: 'logs' },
  { value: 'spans', label: t('queryBuilder.spans'), icon: 'traces' },
  { value: 'metrics', label: t('queryBuilder.metrics'), icon: 'gauge' },
]);
const shapeOptions = computed<SelectOption[]>(() => [
  { value: 'number', label: t('queryBuilder.number'), icon: 'gauge' },
  { value: 'table', label: t('queryBuilder.table'), icon: 'clipboard' },
  { value: 'timeseries', label: t('queryBuilder.timeseries'), icon: 'activity' },
]);
const refreshOptions = computed<SelectOption[]>(() => [
  { value: 'manual', label: t('dashboards.manualRefresh') },
  { value: '30s', label: t('dashboards.every30Seconds') },
  { value: '1m', label: t('dashboards.everyMinute') },
  { value: '5m', label: t('dashboards.every5Minutes') },
]);
const rangeMillis: Record<string, number> = {
  '24h': 24 * 60 * 60 * 1_000,
  '7d': 7 * 24 * 60 * 60 * 1_000,
  '30d': 30 * 24 * 60 * 60 * 1_000,
};
const metricMeasureOptions = computed<SelectOption[]>(() => [
  {
    value: 'value',
    label: t('queryBuilder.metricValue'),
    icon: 'gauge',
    description: t('queryBuilder.metricValueHelp'),
  },
  {
    value: 'samples',
    label: t('queryBuilder.sampleCount'),
    icon: 'activity',
    description: t('queryBuilder.sampleCountHelp'),
  },
]);
const savedQuerySource = computed<QuerySource>(() =>
  savedDataset.value === 'spans' ? 'traces' : savedDataset.value,
);

const savedQueries = useQuery({
  queryKey: computed(() => ['saved-queries', projectId.value]),
  queryFn: () => api.savedQueries(projectId.value),
  enabled: computed(() => Boolean(projectId.value)),
});
const dashboards = useQuery({
  queryKey: computed(() => ['dashboards', projectId.value]),
  queryFn: () => api.dashboards(projectId.value),
  enabled: computed(() => Boolean(projectId.value)),
});
const savedOptions = computed<SelectOption[]>(() => [
  { value: '', label: t('dashboards.chooseWidget') },
  ...(savedQueries.data.value?.items ?? []).map((query) => ({
    value: query.id,
    label: query.name,
    description: `${datasetLabel(query.query.dataset)} · ${shapeLabel(shapeFor(query.query))}`,
  })),
]);
const dashboardOptions = computed<SelectOption[]>(() =>
  (dashboards.data.value?.items ?? []).map((dashboard) => ({
    value: dashboard.id,
    label: dashboard.name,
    description: t('dashboards.widgets', dashboard.widgets.length),
    icon: 'dashboard',
  })),
);
const visibleDashboards = computed(() => {
  const items = dashboards.data.value?.items ?? [];
  const selected = items.find((dashboard) => dashboard.id === selectedDashboardId.value);
  return selected ? [selected] : items.slice(0, 1);
});

watch(projectId, () => {
  draftWidgets.value = [];
  selectedSavedQuery.value = '';
  selectedDashboardId.value = '';
  editingSavedId.value = '';
  editingDashboardId.value = '';
  savedName.value = '';
  savedQueryText.value = '';
  dashboardName.value = '';
  editing.value = false;
  Object.keys(refreshResults).forEach((key) => delete refreshResults[key]);
});
watch(
  () => route.query.edit,
  (value) => {
    if (value === '1') editing.value = true;
  },
);

const createSaved = useMutation({
  mutationFn: () => {
    const existing = savedQueries.data.value?.items.find(
      (saved) => saved.id === editingSavedId.value,
    );
    const query = buildQuery(savedDataset.value, savedShape.value);
    return existing
      ? api.updateSavedQuery(projectId.value, {
          ...existing,
          name: savedName.value.trim(),
          query,
        })
      : api.createSavedQuery(projectId.value, savedName.value.trim(), query);
  },
  onSuccess: async (saved) => {
    const draftIndex = draftWidgets.value.findIndex((item) => item.id === saved.id);
    if (draftIndex >= 0) {
      draftWidgets.value[draftIndex] = saved;
    } else {
      draftWidgets.value.push(saved);
    }
    savedName.value = '';
    savedQueryText.value = '';
    editingSavedId.value = '';
    await queryClient.invalidateQueries({ queryKey: ['saved-queries', projectId.value] });
  },
});
const createDashboard = useMutation({
  mutationFn: () => {
    const input = {
      name: dashboardName.value.trim(),
      widgets: draftWidgets.value.map((saved) => ({
        title: saved.name,
        saved_query_id: saved.id,
        shape: shapeFor(saved.query),
      })),
      refresh_interval: refreshInterval.value,
    };
    const existing = dashboards.data.value?.items.find(
      (dashboard) => dashboard.id === editingDashboardId.value,
    );
    if (!existing) return api.createDashboard(projectId.value, input);
    return api.updateDashboard(projectId.value, {
      ...existing,
      name: input.name,
      refresh_interval: input.refresh_interval as Dashboard['refresh_interval'],
      widgets: input.widgets.map((widget) => ({
        id:
          existing.widgets.find((item) => item.saved_query_id === widget.saved_query_id)?.id ??
          randomHexId(),
        ...widget,
        shape: widget.shape,
      })),
    });
  },
  onSuccess: async (dashboard) => {
    dashboardName.value = '';
    draftWidgets.value = [];
    selectedDashboardId.value = dashboard.id;
    editingDashboardId.value = '';
    editing.value = false;
    delete refreshResults[dashboard.id];
    await refresh(dashboard);
    await queryClient.invalidateQueries({ queryKey: ['dashboards', projectId.value] });
  },
});
const mutation = useMutation({
  mutationFn: (work: () => Promise<unknown>) => work(),
});

function buildQuery(dataset: ExploreDataset, shape: ExploreShape): ExploreRequest {
  const window = savedRange.value === 'custom' ? savedWindow.value : timeWindow(savedRange.value);
  return {
    dataset,
    from: window.from,
    until: window.until,
    query: savedQueryText.value.trim(),
    predicates: [],
    aggregates:
      shape === 'table'
        ? []
        : dataset === 'metrics'
          ? [
              savedMetricMeasure.value === 'value'
                ? { function: 'sum', field: 'metric_sum', alias: 'value' }
                : { function: 'sum', field: 'metric_count', alias: 'samples' },
            ]
          : [{ function: 'count', alias: 'count' }],
    group_by: [],
    interval: shape === 'timeseries' ? '1h' : undefined,
    limit: 50,
  };
}

function shapeFor(query: ExploreRequest): ExploreShape {
  if (query.aggregates.length === 1 && !query.group_by.length && !query.interval) return 'number';
  return query.interval ? 'timeseries' : 'table';
}

function datasetLabel(value: ExploreDataset): string {
  return t(`queryBuilder.${value}`);
}

function shapeLabel(value: ExploreShape): string {
  return t(`queryBuilder.${value}`);
}

function refreshLabel(value: Dashboard['refresh_interval']): string {
  const keys: Record<Dashboard['refresh_interval'], string> = {
    manual: 'dashboards.manualRefresh',
    '30s': 'dashboards.every30Seconds',
    '1m': 'dashboards.everyMinute',
    '5m': 'dashboards.every5Minutes',
  };
  return t(keys[value]);
}

function fieldLabel(value: string): string {
  const keys: Record<string, string> = {
    count: 'queryBuilder.count',
    duration_ms: 'queryBuilder.duration',
    environment: 'queryBuilder.environment',
    id: 'queryBuilder.id',
    issue_id: 'queryBuilder.issueId',
    level: 'queryBuilder.level',
    message: 'queryBuilder.message',
    metric_kind: 'queryBuilder.metricKind',
    name: 'queryBuilder.name',
    operation: 'queryBuilder.operation',
    platform: 'queryBuilder.platform',
    received_at: 'queryBuilder.receivedAt',
    release: 'queryBuilder.release',
    samples: 'queryBuilder.samples',
    service: 'queryBuilder.service',
    status: 'queryBuilder.status',
    timestamp: 'queryBuilder.timestamp',
    trace_id: 'queryBuilder.traceId',
    unit: 'queryBuilder.unit',
    value: 'queryBuilder.value',
  };
  return keys[value] ? t(keys[value]) : value.replaceAll('_', ' ');
}

function formatNumber(value: number): string {
  return new Intl.NumberFormat(locale.value).format(value);
}

function addWidget(): void {
  const saved = savedQueries.data.value?.items.find((item) => item.id === selectedSavedQuery.value);
  if (
    !saved ||
    draftWidgets.value.some((item) => item.id === saved.id) ||
    draftWidgets.value.length >= 8
  )
    return;
  draftWidgets.value.push(saved);
  selectedSavedQuery.value = '';
}

async function refresh(dashboard: Dashboard): Promise<void> {
  const result = await mutation.mutateAsync(() =>
    api.refreshDashboard(projectId.value, dashboard.id, {}),
  );
  refreshResults[dashboard.id] = result as DashboardRefresh;
  lastAutoRefresh[dashboard.id] = Date.now();
}

function editSaved(saved: SavedQuery): void {
  editing.value = true;
  editingSavedId.value = saved.id;
  savedName.value = saved.name;
  savedDataset.value = saved.query.dataset;
  savedShape.value = shapeFor(saved.query);
  savedQueryText.value = saved.query.query ?? '';
  const age = saved.query.until - saved.query.from;
  savedRange.value =
    saved.query.from === 0
      ? 'all'
      : (Object.entries(rangeMillis).find(([, duration]) => duration === age)?.[0] ?? 'custom');
  savedWindow.value = { from: saved.query.from, until: saved.query.until };
  savedMetricMeasure.value =
    saved.query.aggregates[0]?.field === 'metric_count' ? 'samples' : 'value';
}

async function removeSaved(saved: SavedQuery): Promise<void> {
  await mutation.mutateAsync(() => api.deleteSavedQuery(projectId.value, saved.id));
  await queryClient.invalidateQueries({ queryKey: ['saved-queries', projectId.value] });
}

async function removeDashboard(dashboard: Dashboard): Promise<void> {
  await mutation.mutateAsync(() => api.deleteDashboard(projectId.value, dashboard.id));
  delete refreshResults[dashboard.id];
  await queryClient.invalidateQueries({ queryKey: ['dashboards', projectId.value] });
}

function editDashboard(dashboard: Dashboard): void {
  editing.value = true;
  editingDashboardId.value = dashboard.id;
  dashboardName.value = dashboard.name;
  refreshInterval.value = dashboard.refresh_interval;
  draftWidgets.value = dashboard.widgets
    .map((widget) =>
      savedQueries.data.value?.items.find((saved) => saved.id === widget.saved_query_id),
    )
    .filter((saved): saved is SavedQuery => Boolean(saved));
}

function widgetResult(dashboardId: string, widgetId: string) {
  return refreshResults[dashboardId]?.widgets.find((item) => item.widget_id === widgetId);
}

function columns(items: Array<Record<string, unknown>> | null | undefined): string[] {
  return items?.[0] ? Object.keys(items[0]) : [];
}

const refreshMillis: Record<Dashboard['refresh_interval'], number | null> = {
  manual: null,
  '30s': 30_000,
  '1m': 60_000,
  '5m': 300_000,
};
watch(
  () => dashboards.data.value?.items,
  (items) => {
    if (items?.length && !items.some((dashboard) => dashboard.id === selectedDashboardId.value)) {
      selectedDashboardId.value = items[0].id;
    }
  },
  { immediate: true },
);
watch(
  visibleDashboards,
  (items) => {
    for (const dashboard of items) {
      if (!refreshResults[dashboard.id] && !mutation.isPending.value) {
        void refresh(dashboard).catch(() => undefined);
      }
    }
  },
  { immediate: true },
);
const autoRefreshTimer = window.setInterval(() => {
  if (mutation.isPending.value) return;
  const now = Date.now();
  for (const dashboard of visibleDashboards.value) {
    const interval = refreshMillis[dashboard.refresh_interval];
    if (interval && now - (lastAutoRefresh[dashboard.id] ?? now) >= interval) {
      void refresh(dashboard).catch(() => undefined);
    }
  }
}, 5_000);
onUnmounted(() => window.clearInterval(autoRefreshTimer));
</script>

<template>
  <section class="dashboards-page">
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{ $t('dashboards.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('dashboards.title') }}</h1>
        <p>{{ $t('dashboards.description') }}</p>
      </div>
    </header>

    <ApiErrorPanel
      v-if="createSaved.error.value || createDashboard.error.value || mutation.error.value"
      :error="createSaved.error.value || createDashboard.error.value || mutation.error.value"
    />

    <div v-if="canWrite && editing" class="dashboard-builders">
      <form class="panel dashboard-builder" @submit.prevent="createSaved.mutate()">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('dashboards.stepWidget') }}</p>
            <h2>
              {{ editingSavedId ? $t('dashboards.editWidget') : $t('dashboards.addWidget') }}
            </h2>
            <p>{{ $t('dashboards.widgetHelp') }}</p>
          </div>
          <AppIcon name="explore" :size="20" />
        </div>
        <label>
          <span>{{ $t('dashboards.widgetTitle') }}</span>
          <input
            v-model="savedName"
            maxlength="120"
            required
            :placeholder="$t('dashboards.widgetTitlePlaceholder')"
          />
        </label>
        <div class="dashboard-builder__row">
          <BaseSelect
            v-model="savedDataset"
            :options="datasetOptions"
            :label="$t('queryBuilder.dataset')"
          />
          <BaseSelect
            v-model="savedShape"
            :options="shapeOptions"
            :label="$t('queryBuilder.result')"
          />
        </div>
        <TimeRangeSelect
          v-model="savedRange"
          :window-value="savedWindow"
          :label="$t('queryBuilder.timeWindow')"
          @update:window-value="savedWindow = $event"
        />
        <BaseSelect
          v-if="savedDataset === 'metrics' && savedShape !== 'table'"
          v-model="savedMetricMeasure"
          :options="metricMeasureOptions"
          :label="$t('queryBuilder.metricMeasure')"
        />
        <UnifiedQueryBar
          v-model="savedQueryText"
          :source="savedQuerySource"
          :show-submit="false"
          :show-reset="Boolean(savedQueryText)"
          :sync-url="false"
          :disabled="createSaved.isPending.value"
          @reset="savedQueryText = ''"
        />
        <button
          class="button button--primary"
          :disabled="createSaved.isPending.value"
          type="submit"
        >
          <AppIcon name="save" :size="17" />
          {{ editingSavedId ? $t('dashboards.saveWidget') : $t('dashboards.addWidgetAction') }}
        </button>
      </form>

      <form class="panel dashboard-builder" @submit.prevent="createDashboard.mutate()">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('dashboards.stepDashboard') }}</p>
            <h2>
              {{
                editingDashboardId ? $t('dashboards.editDashboard') : $t('dashboards.arrangeShared')
              }}
            </h2>
            <p>{{ $t('dashboards.dashboardHelp') }}</p>
          </div>
          <AppIcon name="dashboard" :size="20" />
        </div>
        <label>
          <span>{{ $t('dashboards.name') }}</span>
          <input
            v-model="dashboardName"
            maxlength="120"
            required
            :placeholder="$t('dashboards.namePlaceholder')"
          />
        </label>
        <div class="dashboard-widget-picker">
          <BaseSelect
            v-model="selectedSavedQuery"
            :options="savedOptions"
            :label="$t('dashboards.addExisting')"
          />
          <button
            class="button button--secondary"
            type="button"
            :disabled="!selectedSavedQuery || draftWidgets.length >= 8"
            @click="addWidget"
          >
            <AppIcon name="plus" :size="16" />
            {{ $t('dashboards.add') }}
          </button>
        </div>
        <div v-if="draftWidgets.length" class="dashboard-draft-widgets">
          <button
            v-for="saved in draftWidgets"
            :key="saved.id"
            type="button"
            @click="draftWidgets = draftWidgets.filter((item) => item.id !== saved.id)"
          >
            {{ saved.name }} <AppIcon name="close" :size="14" />
          </button>
        </div>
        <BaseSelect
          v-model="refreshInterval"
          :options="refreshOptions"
          :label="$t('dashboards.refreshPreference')"
        />
        <button
          class="button button--primary"
          type="submit"
          :disabled="createDashboard.isPending.value || !draftWidgets.length"
        >
          <AppIcon name="plus" :size="17" />
          {{
            editingDashboardId ? $t('dashboards.saveDashboard') : $t('dashboards.createDashboard')
          }}
        </button>
      </form>
    </div>

    <LoadingPanel
      v-if="savedQueries.isPending.value || dashboards.isPending.value"
      :label="$t('dashboards.loading')"
    />
    <ApiErrorPanel
      v-else-if="savedQueries.error.value || dashboards.error.value"
      :error="savedQueries.error.value || dashboards.error.value"
      @retry="
        savedQueries.refetch();
        dashboards.refetch();
      "
    />
    <template v-else>
      <section v-if="canWrite && editing" class="dashboard-section dashboard-section--definitions">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('dashboards.library') }}</p>
            <h2>{{ $t('dashboards.savedQueries') }}</h2>
          </div>
          <span>
            {{ $t('dashboards.shared', { count: savedQueries.data.value?.items.length ?? 0 }) }}
          </span>
        </div>
        <EmptyState
          v-if="!savedQueries.data.value?.items.length"
          icon="explore"
          :title="$t('dashboards.noSavedQueries')"
          :description="$t('dashboards.noSavedQueriesHelp')"
        />
        <div v-else class="saved-query-list">
          <article v-for="saved in savedQueries.data.value.items" :key="saved.id">
            <div>
              <input
                v-if="canWrite"
                v-model="saved.name"
                maxlength="120"
                :aria-label="$t('dashboards.savedQueryName')"
              />
              <strong v-else>{{ saved.name }}</strong>
              <span
                >{{ datasetLabel(saved.query.dataset) }} · {{ shapeLabel(shapeFor(saved.query)) }} ·
                {{ $t('dashboards.revision', { revision: saved.revision }) }}</span
              >
            </div>
            <div v-if="canWrite" class="compact-actions">
              <button class="button button--secondary" type="button" @click="editSaved(saved)">
                <AppIcon name="settings" :size="15" /> {{ $t('dashboards.edit') }}
              </button>
              <button class="button button--danger" type="button" @click="removeSaved(saved)">
                <AppIcon name="delete" :size="15" /> {{ $t('dashboards.delete') }}
              </button>
            </div>
          </article>
        </div>
      </section>

      <section class="dashboard-section dashboard-section--views">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('dashboards.sharedView') }}</p>
            <h2>{{ visibleDashboards[0]?.name || $t('dashboards.projectDashboard') }}</h2>
          </div>
          <div class="compact-actions dashboard-view-actions">
            <BaseSelect
              v-if="dashboardOptions.length > 1"
              v-model="selectedDashboardId"
              class="dashboard-picker"
              :options="dashboardOptions"
              :label="$t('dashboards.dashboard')"
            />
          </div>
        </div>
        <EmptyState
          v-if="!dashboards.data.value?.items.length"
          icon="dashboard"
          :title="$t('dashboards.noDashboard')"
          :description="$t('dashboards.noDashboardHelp')"
        >
          <button
            v-if="canWrite"
            class="button button--primary"
            type="button"
            @click="editing = true"
          >
            <AppIcon name="plus" :size="16" />
            {{ $t('dashboards.createDashboard') }}
          </button>
        </EmptyState>
        <article
          v-for="dashboard in visibleDashboards"
          v-else
          :key="dashboard.id"
          class="dashboard-card"
        >
          <header>
            <div>
              <h3>{{ dashboard.name }}</h3>
              <span>
                {{ $t('dashboards.widgets', dashboard.widgets.length) }} ·
                {{ refreshLabel(dashboard.refresh_interval) }}
              </span>
            </div>
            <div v-if="canWrite" class="compact-actions dashboard-card__actions">
              <button
                class="button button--secondary"
                type="button"
                :aria-pressed="editing"
                @click="editing = !editing"
              >
                <AppIcon :name="editing ? 'close' : 'settings'" :size="15" />
                {{ editing ? $t('dashboards.closeEditor') : $t('dashboards.editDashboard') }}
              </button>
              <button
                v-if="editing"
                class="button button--secondary"
                type="button"
                @click="editDashboard(dashboard)"
              >
                <AppIcon name="settings" :size="15" /> {{ $t('dashboards.editWidgets') }}
              </button>
              <button
                v-if="editing"
                class="button button--danger"
                type="button"
                @click="removeDashboard(dashboard)"
              >
                <AppIcon name="delete" :size="15" /> {{ $t('dashboards.delete') }}
              </button>
            </div>
          </header>

          <div class="dashboard-refresh">
            <span>
              {{
                refreshResults[dashboard.id]
                  ? $t('dashboards.liveData')
                  : $t('dashboards.loadingData')
              }}
            </span>
            <button
              class="button button--secondary button--fit"
              type="button"
              :disabled="mutation.isPending.value"
              @click="refresh(dashboard)"
            >
              <AppIcon name="refresh" :size="16" />
              {{ $t('dashboards.refresh') }}
            </button>
          </div>

          <p v-if="refreshResults[dashboard.id]" class="dashboard-cost">
            {{
              $t('dashboards.updatedCost', {
                cost: formatNumber(refreshResults[dashboard.id].total_cost),
              })
            }}
          </p>
          <div class="dashboard-widgets">
            <article v-for="widget in dashboard.widgets" :key="widget.id" class="dashboard-widget">
              <header>
                <div>
                  <p class="eyebrow">{{ shapeLabel(widget.shape) }}</p>
                  <h4>{{ widget.title }}</h4>
                </div>
                <span v-if="widgetResult(dashboard.id, widget.id)?.cost">
                  {{
                    $t('dashboards.cost', {
                      cost: formatNumber(widgetResult(dashboard.id, widget.id)?.cost ?? 0),
                    })
                  }}
                </span>
              </header>
              <div
                v-if="widgetResult(dashboard.id, widget.id)?.error_code"
                class="dashboard-widget__error"
              >
                <AppIcon name="alert" :size="18" />
                <div>
                  <strong>{{ $t('dashboards.widgetFailed') }}</strong>
                  <code>{{ widgetResult(dashboard.id, widget.id)?.error_code }}</code>
                </div>
              </div>
              <p
                v-else-if="!widgetResult(dashboard.id, widget.id)"
                class="dashboard-widget__waiting"
              >
                {{ $t('dashboards.loadingData') }}
              </p>
              <strong v-else-if="widget.shape === 'number'" class="dashboard-widget__number">
                {{
                  Number(
                    Object.values(widgetResult(dashboard.id, widget.id)?.items?.[0] ?? {})[0] ?? 0,
                  ).toLocaleString(locale)
                }}
              </strong>
              <div v-else class="issue-table-wrap">
                <table class="issue-table">
                  <thead>
                    <tr>
                      <th
                        v-for="column in columns(widgetResult(dashboard.id, widget.id)?.items)"
                        :key="column"
                      >
                        {{ fieldLabel(column) }}
                      </th>
                    </tr>
                  </thead>
                  <tbody>
                    <tr
                      v-for="(row, index) in widgetResult(dashboard.id, widget.id)?.items ?? []"
                      :key="index"
                    >
                      <td
                        v-for="column in columns(widgetResult(dashboard.id, widget.id)?.items)"
                        :key="column"
                      >
                        {{ row[column] }}
                      </td>
                    </tr>
                  </tbody>
                </table>
              </div>
            </article>
          </div>
        </article>
      </section>
    </template>
  </section>
</template>
