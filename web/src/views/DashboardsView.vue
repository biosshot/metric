<script setup lang="ts">
import { computed, onUnmounted, reactive, ref, watch } from 'vue';
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
const queryClient = useQueryClient();
const projectId = computed(() => session.selectedProjectId ?? '');
const canWrite = computed(() => session.has('issue:write'));

const savedName = ref('');
const savedDataset = ref<ExploreDataset>('logs');
const savedShape = ref<ExploreShape>('number');
const dashboardName = ref('');
const selectedSavedQuery = ref('');
const refreshInterval = ref('manual');
const draftWidgets = ref<SavedQuery[]>([]);
const environment = reactive<Record<string, string>>({});
const release = reactive<Record<string, string>>({});
const refreshResults = reactive<Record<string, DashboardRefresh>>({});
const lastAutoRefresh = reactive<Record<string, number>>({});

const datasetOptions: SelectOption[] = [
  { value: 'errors', label: 'Errors', icon: 'bug' },
  { value: 'logs', label: 'Logs', icon: 'logs' },
  { value: 'spans', label: 'Spans', icon: 'traces' },
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
  { value: '', label: 'Choose a saved query' },
  ...(savedQueries.data.value?.items ?? []).map((query) => ({
    value: query.id,
    label: query.name,
    description: `${query.query.dataset} · ${shapeFor(query.query)}`,
  })),
]);

watch(projectId, () => {
  draftWidgets.value = [];
  selectedSavedQuery.value = '';
  Object.keys(refreshResults).forEach((key) => delete refreshResults[key]);
});

const createSaved = useMutation({
  mutationFn: () =>
    api.createSavedQuery(projectId.value, savedName.value.trim(), buildQuery(savedDataset.value, savedShape.value)),
  onSuccess: async () => {
    savedName.value = '';
    await queryClient.invalidateQueries({ queryKey: ['saved-queries', projectId.value] });
  },
});
const createDashboard = useMutation({
  mutationFn: () =>
    api.createDashboard(projectId.value, {
      name: dashboardName.value.trim(),
      widgets: draftWidgets.value.map((saved) => ({
        title: saved.name,
        saved_query_id: saved.id,
        shape: shapeFor(saved.query),
      })),
      refresh_interval: refreshInterval.value,
    }),
  onSuccess: async () => {
    dashboardName.value = '';
    draftWidgets.value = [];
    await queryClient.invalidateQueries({ queryKey: ['dashboards', projectId.value] });
  },
});
const mutation = useMutation({
  mutationFn: (work: () => Promise<unknown>) => work(),
});

function buildQuery(dataset: ExploreDataset, shape: ExploreShape): ExploreRequest {
  const until = Date.now();
  return {
    dataset,
    from: until - 24 * 60 * 60 * 1000,
    until,
    predicates: [],
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
  if (!saved || draftWidgets.value.some((item) => item.id === saved.id) || draftWidgets.value.length >= 8)
    return;
  draftWidgets.value.push(saved);
  selectedSavedQuery.value = '';
}

async function refresh(dashboard: Dashboard): Promise<void> {
  const result = await mutation.mutateAsync(() =>
    api.refreshDashboard(projectId.value, dashboard.id, {
      environment: environment[dashboard.id]?.trim() || undefined,
      release: release[dashboard.id]?.trim() || undefined,
    }),
  );
  refreshResults[dashboard.id] = result as DashboardRefresh;
  lastAutoRefresh[dashboard.id] = Date.now();
}

async function removeSaved(saved: SavedQuery): Promise<void> {
  await mutation.mutateAsync(() => api.deleteSavedQuery(projectId.value, saved.id));
  await queryClient.invalidateQueries({ queryKey: ['saved-queries', projectId.value] });
}

async function saveSavedName(saved: SavedQuery): Promise<void> {
  await mutation.mutateAsync(() => api.updateSavedQuery(projectId.value, saved));
  await queryClient.invalidateQueries({ queryKey: ['saved-queries', projectId.value] });
}

async function removeDashboard(dashboard: Dashboard): Promise<void> {
  await mutation.mutateAsync(() => api.deleteDashboard(projectId.value, dashboard.id));
  delete refreshResults[dashboard.id];
  await queryClient.invalidateQueries({ queryKey: ['dashboards', projectId.value] });
}

async function saveDashboardName(dashboard: Dashboard): Promise<void> {
  await mutation.mutateAsync(() => api.updateDashboard(projectId.value, dashboard));
  await queryClient.invalidateQueries({ queryKey: ['dashboards', projectId.value] });
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
    const now = Date.now();
    for (const dashboard of items ?? []) lastAutoRefresh[dashboard.id] ??= now;
  },
  { immediate: true },
);
const autoRefreshTimer = window.setInterval(() => {
  if (mutation.isPending.value) return;
  const now = Date.now();
  for (const dashboard of dashboards.data.value?.items ?? []) {
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
        <p class="eyebrow">{{ session.selectedProject?.slug }} / shared query workspace</p>
        <h1>Dashboards</h1>
        <p>Save typed Explore questions and compose up to eight bounded widgets per project.</p>
      </div>
    </header>

    <ApiErrorPanel
      v-if="createSaved.error.value || createDashboard.error.value || mutation.error.value"
      :error="createSaved.error.value || createDashboard.error.value || mutation.error.value"
    />

    <div v-if="canWrite" class="dashboard-builders">
      <form class="panel dashboard-builder" @submit.prevent="createSaved.mutate()">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Saved query</p>
            <h2>Save a bounded question</h2>
          </div>
          <AppIcon name="explore" :size="20" />
        </div>
        <label>
          <span>Name</span>
          <input v-model="savedName" maxlength="120" required placeholder="Production log volume" />
        </label>
        <div class="dashboard-builder__row">
          <BaseSelect v-model="savedDataset" :options="datasetOptions" label="Dataset" />
          <BaseSelect v-model="savedShape" :options="shapeOptions" label="Result" />
        </div>
        <button class="button button--primary" :disabled="createSaved.isPending.value" type="submit">
          <AppIcon name="save" :size="17" />
          Save query
        </button>
      </form>

      <form class="panel dashboard-builder" @submit.prevent="createDashboard.mutate()">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Dashboard</p>
            <h2>Compose shared widgets</h2>
          </div>
          <AppIcon name="dashboard" :size="20" />
        </div>
        <label>
          <span>Name</span>
          <input v-model="dashboardName" maxlength="120" required placeholder="Service health" />
        </label>
        <div class="dashboard-widget-picker">
          <BaseSelect v-model="selectedSavedQuery" :options="savedOptions" label="Saved query" />
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
        <BaseSelect v-model="refreshInterval" :options="refreshOptions" label="Refresh preference" />
        <button
          class="button button--primary"
          type="submit"
          :disabled="createDashboard.isPending.value || !draftWidgets.length"
        >
          <AppIcon name="plus" :size="17" />
          Create dashboard
        </button>
      </form>
    </div>

    <LoadingPanel
      v-if="savedQueries.isPending.value || dashboards.isPending.value"
      label="Loading shared dashboards…"
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
      <section class="dashboard-section">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Typed definitions</p>
            <h2>Saved queries</h2>
          </div>
          <span>{{ savedQueries.data.value?.items.length ?? 0 }} shared</span>
        </div>
        <EmptyState
          v-if="!savedQueries.data.value?.items.length"
          icon="explore"
          title="No saved queries"
          description="Save a table, number, or timeseries question before composing a dashboard."
        />
        <div v-else class="saved-query-list">
          <article v-for="saved in savedQueries.data.value.items" :key="saved.id">
            <div>
              <input v-if="canWrite" v-model="saved.name" maxlength="120" aria-label="Saved query name" />
              <strong v-else>{{ saved.name }}</strong>
              <span>{{ saved.query.dataset }} · {{ shapeFor(saved.query) }} · revision {{ saved.revision }}</span>
            </div>
            <div v-if="canWrite" class="compact-actions">
              <button class="button button--secondary" type="button" @click="saveSavedName(saved)">
                <AppIcon name="save" :size="15" /> Save
              </button>
              <button class="button button--danger" type="button" @click="removeSaved(saved)">
                <AppIcon name="delete" :size="15" /> Delete
              </button>
            </div>
          </article>
        </div>
      </section>

      <section class="dashboard-section">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Shared project resources</p>
            <h2>Dashboard views</h2>
          </div>
          <span>{{ dashboards.data.value?.items.length ?? 0 }} dashboards</span>
        </div>
        <EmptyState
          v-if="!dashboards.data.value?.items.length"
          icon="dashboard"
          title="No dashboards yet"
          description="Compose saved queries into a bounded dashboard. Every project reader sees the same view."
        />
        <article
          v-for="dashboard in dashboards.data.value?.items ?? []"
          v-else
          :key="dashboard.id"
          class="dashboard-card"
        >
          <header>
            <div>
              <input
                v-if="canWrite"
                v-model="dashboard.name"
                maxlength="120"
                aria-label="Dashboard name"
              />
              <h3 v-else>{{ dashboard.name }}</h3>
              <span
                >{{ dashboard.widgets.length }} widget{{
                  dashboard.widgets.length === 1 ? '' : 's'
                }}
                · {{ dashboard.refresh_interval }}</span
              >
            </div>
            <div v-if="canWrite" class="compact-actions">
              <button
                class="button button--secondary"
                type="button"
                @click="saveDashboardName(dashboard)"
              >
                <AppIcon name="save" :size="15" /> Save
              </button>
              <button class="button button--danger" type="button" @click="removeDashboard(dashboard)">
                <AppIcon name="delete" :size="15" /> Delete
              </button>
            </div>
          </header>

          <div class="dashboard-variables">
            <label>
              <span>Environment</span>
              <input v-model="environment[dashboard.id]" maxlength="256" placeholder="All environments" />
            </label>
            <label>
              <span>Release</span>
              <input v-model="release[dashboard.id]" maxlength="256" placeholder="All releases" />
            </label>
            <button
              class="button button--primary"
              type="button"
              :disabled="mutation.isPending.value"
              @click="refresh(dashboard)"
            >
              <AppIcon name="refresh" :size="16" />
              Refresh
            </button>
          </div>

          <p v-if="refreshResults[dashboard.id]" class="dashboard-cost">
            Total estimated cost {{ refreshResults[dashboard.id].total_cost }} / 25,000
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
              <p v-else-if="!widgetResult(dashboard.id, widget.id)" class="dashboard-widget__waiting">
                Refresh to run this saved query.
              </p>
              <strong
                v-else-if="widget.shape === 'number'"
                class="dashboard-widget__number"
              >
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
                      <td v-for="column in columns(widgetResult(dashboard.id, widget.id)?.items)" :key="column">
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
