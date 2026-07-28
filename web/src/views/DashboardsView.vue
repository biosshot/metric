<script setup lang="ts">
import { computed, onUnmounted, reactive, ref, watch } from 'vue';
import { useRoute } from 'vue-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import type {
  Dashboard,
  DashboardRefresh,
  ExploreDataset,
  ExploreRequest,
  ExploreShape,
  SavedQuery,
} from '../api/types';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const queryClient = useQueryClient();
const projectId = computed(() => session.selectedProjectId ?? '');
const canWrite = computed(() => session.has('issue:write'));

const savedName = ref('');
const savedDataset = ref<ExploreDataset>('logs');
const savedShape = ref<ExploreShape>('number');
const savedField = ref('');
const savedOperator = ref<'exact' | 'contains' | 'starts_with' | 'ends_with'>('exact');
const savedValue = ref('');
const savedRange = ref('24h');
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

const datasetOptions: SelectOption[] = [
  { value: 'errors', label: 'Errors', icon: 'bug' },
  { value: 'logs', label: 'Logs', icon: 'logs' },
  { value: 'spans', label: 'Spans', icon: 'traces' },
  { value: 'metrics', label: 'Metrics', icon: 'gauge' },
];
const shapeOptions: SelectOption[] = [
  { value: 'number', label: 'Number', icon: 'gauge' },
  { value: 'table', label: 'Table', icon: 'clipboard' },
  { value: 'timeseries', label: 'Timeseries', icon: 'activity' },
];
const refreshOptions: SelectOption[] = [
  { value: 'manual', label: 'Manual refresh' },
  { value: '30s', label: 'Every 30 seconds' },
  { value: '1m', label: 'Every minute' },
  { value: '5m', label: 'Every 5 minutes' },
];
const rangeOptions: SelectOption[] = [
  { value: '24h', label: 'Last 24 hours', icon: 'history' },
  { value: '7d', label: 'Last 7 days', icon: 'history' },
  { value: '30d', label: 'Last 30 days', icon: 'history' },
];
const rangeMillis: Record<string, number> = {
  '24h': 24 * 60 * 60 * 1_000,
  '7d': 7 * 24 * 60 * 60 * 1_000,
  '30d': 30 * 24 * 60 * 60 * 1_000,
};
const fieldOptionsByDataset: Record<ExploreDataset, SelectOption[]> = {
  errors: [
    { value: 'level', label: 'Level' },
    { value: 'platform', label: 'Platform' },
  ],
  logs: [
    { value: 'message', label: 'Message' },
    { value: 'level', label: 'Level' },
    { value: 'environment', label: 'Environment' },
    { value: 'release', label: 'Release' },
    { value: 'service', label: 'Service' },
  ],
  spans: [
    { value: 'name', label: 'Name' },
    { value: 'operation', label: 'Operation' },
    { value: 'status', label: 'Status' },
    { value: 'environment', label: 'Environment' },
    { value: 'release', label: 'Release' },
    { value: 'service', label: 'Service' },
  ],
  metrics: [
    { value: 'name', label: 'Metric name' },
    { value: 'metric_kind', label: 'Metric kind' },
    { value: 'unit', label: 'Unit' },
  ],
};
const widgetFieldOptions = computed<SelectOption[]>(() => [
  { value: '', label: 'No filter' },
  ...fieldOptionsByDataset[savedDataset.value],
]);
const operatorOptions: SelectOption[] = [
  { value: 'exact', label: 'Equals' },
  { value: 'contains', label: 'Contains' },
  { value: 'starts_with', label: 'Starts with' },
  { value: 'ends_with', label: 'Ends with' },
];
const exactOperatorOptions: SelectOption[] = [{ value: 'exact', label: 'Equals' }];
const widgetTextFields = new Set([
  'message',
  'environment',
  'release',
  'service',
  'name',
  'operation',
  'status',
  'unit',
]);
const widgetOperatorOptions = computed(() =>
  widgetTextFields.has(savedField.value) ? operatorOptions : exactOperatorOptions,
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
  { value: '', label: 'Choose an existing widget' },
  ...(savedQueries.data.value?.items ?? []).map((query) => ({
    value: query.id,
    label: query.name,
    description: `${query.query.dataset} · ${shapeFor(query.query)}`,
  })),
]);
const dashboardOptions = computed<SelectOption[]>(() =>
  (dashboards.data.value?.items ?? []).map((dashboard) => ({
    value: dashboard.id,
    label: dashboard.name,
    description: `${dashboard.widgets.length} widget${dashboard.widgets.length === 1 ? '' : 's'}`,
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
          crypto.randomUUID().replaceAll('-', ''),
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
  const until = Date.now();
  const predicates: ExploreRequest['predicates'] =
    savedField.value && savedValue.value.trim()
      ? [
          {
            field: savedField.value,
            op: widgetTextFields.has(savedField.value) ? savedOperator.value : 'exact',
            value: savedValue.value.trim(),
          },
        ]
      : [];
  return {
    dataset,
    from: until - rangeMillis[savedRange.value],
    until,
    predicates,
    aggregates: shape === 'table' ? [] : [{ function: 'count', alias: 'count' }],
    group_by: [],
    interval: shape === 'timeseries' ? '1h' : undefined,
    limit: 50,
  };
}

function shapeFor(query: ExploreRequest): ExploreShape {
  if (query.aggregates.length === 1 && !query.group_by.length && !query.interval) return 'number';
  return query.interval ? 'timeseries' : 'table';
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
  const predicate = saved.query.predicates[0];
  savedField.value = predicate?.field ?? '';
  savedOperator.value =
    predicate?.op === 'contains' || predicate?.op === 'starts_with' || predicate?.op === 'ends_with'
      ? predicate.op
      : 'exact';
  savedValue.value = typeof predicate?.value === 'string' ? predicate.value : '';
  const age = saved.query.until - saved.query.from;
  savedRange.value = age > rangeMillis['7d'] ? '30d' : age > rangeMillis['24h'] ? '7d' : '24h';
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
        <p class="eyebrow">{{ session.selectedProject?.slug }} / overview</p>
        <h1>Dashboard</h1>
        <p>See the shared project view first. Open editing only when the view needs to change.</p>
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
            <p class="eyebrow">Step 1 · Widget query</p>
            <h2>{{ editingSavedId ? 'Edit widget' : 'Add a widget' }}</h2>
            <p>Choose what to measure, narrow the signals, and save it to the widget library.</p>
          </div>
          <AppIcon name="explore" :size="20" />
        </div>
        <label>
          <span>Widget title</span>
          <input v-model="savedName" maxlength="120" required placeholder="Production log volume" />
        </label>
        <div class="dashboard-builder__row">
          <BaseSelect v-model="savedDataset" :options="datasetOptions" label="Dataset" />
          <BaseSelect v-model="savedShape" :options="shapeOptions" label="Result" />
        </div>
        <BaseSelect v-model="savedRange" :options="rangeOptions" label="Time window" />
        <div class="dashboard-widget-filter">
          <BaseSelect v-model="savedField" :options="widgetFieldOptions" label="Filter field" />
          <BaseSelect
            v-if="savedField"
            v-model="savedOperator"
            :options="widgetOperatorOptions"
            label="Match"
          />
          <label v-if="savedField">
            <span>Value</span>
            <input v-model="savedValue" maxlength="256" required placeholder="Filter value" />
          </label>
        </div>
        <button
          class="button button--primary"
          :disabled="createSaved.isPending.value"
          type="submit"
        >
          <AppIcon name="save" :size="17" />
          {{ editingSavedId ? 'Save widget' : 'Add widget' }}
        </button>
      </form>

      <form class="panel dashboard-builder" @submit.prevent="createDashboard.mutate()">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Step 2 · Dashboard</p>
            <h2>{{ editingDashboardId ? 'Edit dashboard' : 'Arrange the shared view' }}</h2>
            <p>Pick saved widgets, choose refresh frequency, and create the team view.</p>
          </div>
          <AppIcon name="dashboard" :size="20" />
        </div>
        <label>
          <span>Name</span>
          <input v-model="dashboardName" maxlength="120" required placeholder="Service health" />
        </label>
        <div class="dashboard-widget-picker">
          <BaseSelect
            v-model="selectedSavedQuery"
            :options="savedOptions"
            label="Add an existing widget"
          />
          <button
            class="button button--secondary"
            type="button"
            :disabled="!selectedSavedQuery || draftWidgets.length >= 8"
            @click="addWidget"
          >
            <AppIcon name="plus" :size="16" />
            Add
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
          label="Refresh preference"
        />
        <button
          class="button button--primary"
          type="submit"
          :disabled="createDashboard.isPending.value || !draftWidgets.length"
        >
          <AppIcon name="plus" :size="17" />
          {{ editingDashboardId ? 'Save dashboard' : 'Create dashboard' }}
        </button>
      </form>
    </div>

    <LoadingPanel
      v-if="savedQueries.isPending.value || dashboards.isPending.value"
      label="Loading dashboard…"
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
            <p class="eyebrow">Widget library</p>
            <h2>Saved widget queries</h2>
          </div>
          <span>{{ savedQueries.data.value?.items.length ?? 0 }} shared</span>
        </div>
        <EmptyState
          v-if="!savedQueries.data.value?.items.length"
          icon="explore"
          title="No saved queries"
          description="Add a widget in the editor above. Its reusable definition will appear here."
        />
        <div v-else class="saved-query-list">
          <article v-for="saved in savedQueries.data.value.items" :key="saved.id">
            <div>
              <input
                v-if="canWrite"
                v-model="saved.name"
                maxlength="120"
                aria-label="Saved query name"
              />
              <strong v-else>{{ saved.name }}</strong>
              <span
                >{{ saved.query.dataset }} · {{ shapeFor(saved.query) }} · revision
                {{ saved.revision }}</span
              >
            </div>
            <div v-if="canWrite" class="compact-actions">
              <button class="button button--secondary" type="button" @click="editSaved(saved)">
                <AppIcon name="settings" :size="15" /> Edit
              </button>
              <button class="button button--danger" type="button" @click="removeSaved(saved)">
                <AppIcon name="delete" :size="15" /> Delete
              </button>
            </div>
          </article>
        </div>
      </section>

      <section class="dashboard-section dashboard-section--views">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Shared project view</p>
            <h2>{{ visibleDashboards[0]?.name || 'Project dashboard' }}</h2>
          </div>
          <div class="compact-actions dashboard-view-actions">
            <BaseSelect
              v-if="dashboardOptions.length > 1"
              v-model="selectedDashboardId"
              class="dashboard-picker"
              :options="dashboardOptions"
              label="Dashboard"
            />
          </div>
        </div>
        <EmptyState
          v-if="!dashboards.data.value?.items.length"
          icon="dashboard"
          title="No dashboard yet"
          description="Create a shared view with the signals your team checks most often."
        >
          <button
            v-if="canWrite"
            class="button button--primary"
            type="button"
            @click="editing = true"
          >
            <AppIcon name="plus" :size="16" />
            Create dashboard
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
              <span
                >{{ dashboard.widgets.length }} widget{{
                  dashboard.widgets.length === 1 ? '' : 's'
                }}
                · {{ dashboard.refresh_interval }}</span
              >
            </div>
            <div v-if="canWrite" class="compact-actions dashboard-card__actions">
              <button
                class="button button--secondary"
                type="button"
                :aria-pressed="editing"
                @click="editing = !editing"
              >
                <AppIcon :name="editing ? 'close' : 'settings'" :size="15" />
                {{ editing ? 'Close editor' : 'Edit dashboard' }}
              </button>
              <button
                v-if="editing"
                class="button button--secondary"
                type="button"
                @click="editDashboard(dashboard)"
              >
                <AppIcon name="settings" :size="15" /> Edit widgets
              </button>
              <button
                v-if="editing"
                class="button button--danger"
                type="button"
                @click="removeDashboard(dashboard)"
              >
                <AppIcon name="delete" :size="15" /> Delete
              </button>
            </div>
          </header>

          <div class="dashboard-refresh">
            <span>
              {{
                refreshResults[dashboard.id]
                  ? 'Live data loaded for each widget window.'
                  : 'Loading current widget data…'
              }}
            </span>
            <button
              class="button button--secondary button--fit"
              type="button"
              :disabled="mutation.isPending.value"
              @click="refresh(dashboard)"
            >
              <AppIcon name="refresh" :size="16" />
              Refresh
            </button>
          </div>

          <p v-if="refreshResults[dashboard.id]" class="dashboard-cost">
            Updated now · estimated cost {{ refreshResults[dashboard.id].total_cost }} / 25,000
          </p>
          <div class="dashboard-widgets">
            <article v-for="widget in dashboard.widgets" :key="widget.id" class="dashboard-widget">
              <header>
                <div>
                  <p class="eyebrow">{{ widget.shape }}</p>
                  <h4>{{ widget.title }}</h4>
                </div>
                <span v-if="widgetResult(dashboard.id, widget.id)?.cost">
                  cost {{ widgetResult(dashboard.id, widget.id)?.cost }}
                </span>
              </header>
              <div
                v-if="widgetResult(dashboard.id, widget.id)?.error_code"
                class="dashboard-widget__error"
              >
                <AppIcon name="alert" :size="18" />
                <div>
                  <strong>Widget could not be refreshed</strong>
                  <code>{{ widgetResult(dashboard.id, widget.id)?.error_code }}</code>
                </div>
              </div>
              <p
                v-else-if="!widgetResult(dashboard.id, widget.id)"
                class="dashboard-widget__waiting"
              >
                Loading current data…
              </p>
              <strong v-else-if="widget.shape === 'number'" class="dashboard-widget__number">
                {{
                  Number(
                    Object.values(widgetResult(dashboard.id, widget.id)?.items?.[0] ?? {})[0] ?? 0,
                  ).toLocaleString()
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
                        {{ column.replaceAll('_', ' ') }}
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
