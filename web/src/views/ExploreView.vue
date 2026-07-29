<script setup lang="ts">
import { computed, ref } from 'vue';
import { useMutation, useQuery } from '@tanstack/vue-query';
import type { ExploreDataset, ExploreRequest, ExploreScalar } from '../api/types';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import MetricsSectionNav from '../components/MetricsSectionNav.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import { timeWindow, type TimeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

type ExploreMode = 'table' | 'number' | 'timeseries';
type MetricMeasure = 'value' | 'samples';
type MetricsView = 'overview' | 'query';

const props = withDefaults(
  defineProps<{
    initialDataset?: ExploreDataset;
    datasetLocked?: boolean;
    metricsView?: MetricsView;
  }>(),
  {
    initialDataset: 'errors',
    datasetLocked: false,
    metricsView: 'query',
  },
);
const session = useSessionStore();
const dataset = ref<ExploreDataset>(props.initialDataset);
const mode = ref<ExploreMode>('table');
const metricMeasure = ref<MetricMeasure>('value');
const filterField = ref('');
const filterOperator = ref<'exact' | 'contains' | 'starts_with' | 'ends_with'>('exact');
const filterValue = ref('');
const groupField = ref('');
const interval = ref<'5m' | '1h' | '1d'>('1h');
const range = ref('all');
const selectedWindow = ref<TimeWindow>(timeWindow('all'));
const appliedWindow = ref<TimeWindow>({ ...selectedWindow.value });
const cursor = ref<string | null>(null);
const projectId = computed(() => session.selectedProjectId ?? '');

const datasetOptions: SelectOption[] = [
  { value: 'errors', label: 'Errors', icon: 'bug', description: 'Finalized Error events' },
  { value: 'logs', label: 'Logs', icon: 'logs', description: 'Structured log records' },
  { value: 'spans', label: 'Spans', icon: 'traces', description: 'Root and child spans' },
  {
    value: 'metrics',
    label: 'Metrics',
    icon: 'gauge',
    description: 'Counters, gauges, and distributions',
  },
];
const modeOptions: SelectOption[] = [
  {
    value: 'table',
    label: 'Table',
    icon: 'clipboard',
    description: 'Raw rows with a stable cursor',
  },
  { value: 'number', label: 'Number', icon: 'gauge', description: 'One bounded count' },
  {
    value: 'timeseries',
    label: 'Timeseries',
    icon: 'activity',
    description: 'Count in fixed time buckets',
  },
];
const intervalOptions: SelectOption[] = [
  { value: '5m', label: '5 minutes' },
  { value: '1h', label: '1 hour' },
  { value: '1d', label: '1 day' },
];
const metricMeasureOptions: SelectOption[] = [
  {
    value: 'value',
    label: 'Metric value',
    icon: 'gauge',
    description: 'Sum the values reported by the SDK',
  },
  {
    value: 'samples',
    label: 'Sample count',
    icon: 'activity',
    description: 'Count the observations behind each metric',
  },
];

const fieldsByDataset: Record<ExploreDataset, SelectOption[]> = {
  errors: [
    { value: 'level', label: 'Level' },
    { value: 'platform', label: 'Platform' },
    { value: 'issue_id', label: 'Issue ID' },
  ],
  logs: [
    { value: 'level', label: 'Level' },
    { value: 'environment', label: 'Environment' },
    { value: 'release', label: 'Release' },
    { value: 'service', label: 'Service' },
    { value: 'trace_id', label: 'Trace ID' },
  ],
  spans: [
    { value: 'environment', label: 'Environment' },
    { value: 'release', label: 'Release' },
    { value: 'service', label: 'Service' },
    { value: 'operation', label: 'Operation' },
    { value: 'status', label: 'Status' },
    { value: 'is_segment', label: 'Root segment' },
  ],
  metrics: [
    { value: 'name', label: 'Metric name' },
    { value: 'metric_kind', label: 'Metric kind' },
    { value: 'unit', label: 'Unit' },
    { value: 'trace_id', label: 'Trace ID' },
  ],
};
const groupFieldsByDataset: Record<ExploreDataset, SelectOption[]> = {
  errors: [
    { value: 'level', label: 'Level' },
    { value: 'platform', label: 'Platform' },
  ],
  logs: [{ value: 'level', label: 'Level' }],
  spans: [
    { value: 'operation_class', label: 'Operation class' },
    { value: 'is_segment', label: 'Root segment' },
  ],
  metrics: [
    { value: 'metric_kind', label: 'Metric kind' },
    { value: 'unit', label: 'Unit' },
  ],
};
const optionalFields = computed<SelectOption[]>(() => [
  { value: '', label: 'No exact filter' },
  ...fieldsByDataset[dataset.value],
]);
const optionalGroups = computed<SelectOption[]>(() => [
  { value: '', label: 'No grouping' },
  ...groupFieldsByDataset[dataset.value],
]);
const textOperators: SelectOption[] = [
  { value: 'exact', label: 'Equals' },
  { value: 'contains', label: 'Contains' },
  { value: 'starts_with', label: 'Starts with' },
  { value: 'ends_with', label: 'Ends with' },
];
const exactOperator: SelectOption[] = [{ value: 'exact', label: 'Equals' }];
const textFields = new Set([
  'message',
  'environment',
  'release',
  'service',
  'operation',
  'status',
  'name',
  'unit',
]);
const operatorOptions = computed(() =>
  textFields.has(filterField.value) ? textOperators : exactOperator,
);

const query = useMutation({
  mutationFn: (request: ExploreRequest) => api.explore(projectId.value, request),
});
const metricsOverview = useQuery({
  queryKey: computed(() => ['metrics-overview', projectId.value]),
  queryFn: () => {
    const until = Date.now();
    return api.explore(projectId.value, {
      dataset: 'metrics',
      from: 0,
      until,
      predicates: [],
      aggregates: [
        { function: 'sum', field: 'metric_sum', alias: 'value' },
        { function: 'sum', field: 'metric_count', alias: 'samples' },
      ],
      group_by: ['name'],
      cursor: null,
      limit: 100,
    });
  },
  enabled: computed(
    () => props.datasetLocked && props.metricsView === 'overview' && Boolean(projectId.value),
  ),
  refetchInterval: 30_000,
});
const metricOverviewItems = computed(() =>
  [...(metricsOverview.data.value?.items ?? [])].sort((left, right) =>
    String(left.name ?? '').localeCompare(String(right.name ?? '')),
  ),
);
const columns = computed(() => {
  const first = query.data.value?.items[0];
  return first ? Object.keys(first) : [];
});
const chartMaximum = computed(() =>
  Math.max(
    1,
    ...(query.data.value?.items ?? []).map((item) => Number(item[resultValueColumn.value] ?? 0)),
  ),
);
const resultValueColumn = computed(() =>
  dataset.value === 'metrics' ? (metricMeasure.value === 'value' ? 'value' : 'samples') : 'count',
);

function validSelection(options: SelectOption[], value: string): string {
  return options.some((option) => option.value === value) ? value : '';
}

async function run(nextCursor: string | null = null): Promise<void> {
  filterField.value = validSelection(fieldsByDataset[dataset.value], filterField.value);
  filterOperator.value = validSelection(
    operatorOptions.value,
    filterOperator.value,
  ) as typeof filterOperator.value;
  groupField.value = validSelection(groupFieldsByDataset[dataset.value], groupField.value);
  cursor.value = nextCursor;
  if (!nextCursor) appliedWindow.value = { ...selectedWindow.value };
  const predicates: ExploreRequest['predicates'] = [];
  if (filterField.value && filterValue.value.trim()) {
    const raw = filterValue.value.trim();
    predicates.push({
      field: filterField.value,
      op: filterOperator.value,
      value: filterField.value === 'is_segment' ? raw === 'true' : raw,
    });
  }
  const aggregate =
    mode.value === 'table'
      ? []
      : dataset.value === 'metrics'
        ? [
            metricMeasure.value === 'value'
              ? { function: 'sum' as const, field: 'metric_sum', alias: 'value' }
              : { function: 'sum' as const, field: 'metric_count', alias: 'samples' },
          ]
        : [{ function: 'count' as const, alias: 'count' }];
  await query.mutateAsync({
    dataset: dataset.value,
    from: appliedWindow.value.from,
    until: appliedWindow.value.until,
    predicates,
    aggregates: aggregate,
    group_by: mode.value === 'number' || !groupField.value ? [] : [groupField.value],
    interval: mode.value === 'timeseries' ? interval.value : undefined,
    cursor: nextCursor,
    limit: 50,
  });
}

function display(value: ExploreScalar, key: string): string {
  if (value === null) return '—';
  if ((key === 'timestamp' || key === 'received_at') && typeof value === 'number') {
    return new Date(value).toLocaleString();
  }
  if (key === 'duration_ms' && typeof value === 'number') return `${value.toFixed(2)} ms`;
  return String(value);
}

function metricValue(value: ExploreScalar | undefined): string {
  const number = Number(value ?? 0);
  if (!Number.isFinite(number)) return '—';
  return new Intl.NumberFormat(undefined, { maximumFractionDigits: 3 }).format(number);
}
</script>

<template>
  <section class="explore-page">
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{ session.selectedProject?.slug }} / {{ datasetLocked ? 'metrics' : 'explore' }}
        </p>
        <h1>{{ datasetLocked ? 'Metrics' : 'Unified Explore' }}</h1>
        <p>
          {{
            datasetLocked
              ? 'Inspect counters, gauges, and distributions reported by your SDKs.'
              : 'Ask one bounded question of Errors, Logs, Spans, or Metrics without raw database syntax.'
          }}
        </p>
      </div>
    </header>

    <MetricsSectionNav v-if="datasetLocked" />

    <section
      v-if="datasetLocked && metricsView === 'overview'"
      class="metrics-overview"
      aria-labelledby="metrics-overview-title"
    >
      <div class="section-heading">
        <div>
          <p class="eyebrow">Current catalog</p>
          <h2 id="metrics-overview-title">Metric overview</h2>
          <p>Every retained metric name with its reported value and sample count.</p>
        </div>
        <button
          class="button button--secondary button--fit"
          type="button"
          :disabled="metricsOverview.isFetching.value"
          @click="metricsOverview.refetch()"
        >
          <AppIcon name="refresh" :size="16" />
          Refresh
        </button>
      </div>
      <LoadingPanel v-if="metricsOverview.isPending.value" label="Loading metric overview…" />
      <ApiErrorPanel
        v-else-if="metricsOverview.error.value"
        :error="metricsOverview.error.value"
        title="Metric overview could not be loaded"
        @retry="metricsOverview.refetch()"
      />
      <EmptyState
        v-else-if="!metricOverviewItems.length"
        icon="gauge"
        title="No metrics reported yet"
        description="Send metrics from an SDK and their names and values will appear here."
      />
      <div v-else class="dashboard-widgets metrics-overview__grid">
        <article
          v-for="(item, index) in metricOverviewItems"
          :key="`${item.name}:${index}`"
          class="dashboard-widget"
        >
          <header>
            <div>
              <p class="eyebrow">Metric</p>
              <h4>{{ item.name ?? 'Unnamed metric' }}</h4>
            </div>
            <span>{{ metricValue(item.samples) }} samples</span>
          </header>
          <strong class="dashboard-widget__number">{{ metricValue(item.value) }}</strong>
        </article>
      </div>
    </section>

    <form
      v-if="!datasetLocked || metricsView === 'query'"
      class="panel explore-builder"
      @submit.prevent="run(null)"
    >
      <BaseSelect
        v-if="!datasetLocked"
        v-model="dataset"
        :options="datasetOptions"
        label="Dataset"
      />
      <BaseSelect v-model="mode" :options="modeOptions" label="Result" />
      <TimeRangeSelect
        v-model="range"
        :window-value="selectedWindow"
        label="Time range"
        @update:window-value="selectedWindow = $event"
      />
      <BaseSelect
        v-if="dataset === 'metrics' && mode !== 'table'"
        v-model="metricMeasure"
        :options="metricMeasureOptions"
        label="Measure"
      />
      <BaseSelect v-model="filterField" :options="optionalFields" label="Exact filter" />
      <BaseSelect
        v-if="filterField"
        v-model="filterOperator"
        :options="operatorOptions"
        label="Match"
      />
      <label>
        <span>Value</span>
        <input
          v-model="filterValue"
          maxlength="256"
          :disabled="!filterField"
          placeholder="Exact value"
        />
      </label>
      <BaseSelect
        v-if="mode === 'timeseries'"
        v-model="interval"
        :options="intervalOptions"
        label="Interval"
      />
      <BaseSelect
        v-if="mode === 'timeseries'"
        v-model="groupField"
        :options="optionalGroups"
        label="Group by"
      />
      <button
        class="button button--primary explore-run"
        type="submit"
        :disabled="query.isPending.value"
      >
        <AppIcon name="search" :size="17" />
        Run query
      </button>
    </form>

    <LoadingPanel
      v-if="(!datasetLocked || metricsView === 'query') && query.isPending.value"
      label="Running bounded query…"
    />
    <ApiErrorPanel
      v-else-if="(!datasetLocked || metricsView === 'query') && query.error.value"
      :error="query.error.value"
      @retry="run(cursor)"
    />
    <EmptyState
      v-else-if="(!datasetLocked || metricsView === 'query') && !query.data.value"
      icon="explore"
      title="Build your first query"
      :description="
        datasetLocked
          ? 'Choose a result shape and whether to show reported values or sample counts.'
          : 'Choose a dataset, result shape, and bounded time range.'
      "
    />
    <EmptyState
      v-else-if="
        (!datasetLocked || metricsView === 'query') &&
        query.data.value &&
        !query.data.value.items.length
      "
      icon="search"
      title="No matching signals"
      description="The query completed safely but found no rows in the selected time range."
    />
    <template v-else-if="(!datasetLocked || metricsView === 'query') && query.data.value">
      <div class="explore-result-meta">
        <span
          ><strong>{{ query.data.value.dataset }}</strong> dataset</span
        >
        <span
          >Estimated cost <strong>{{ query.data.value.cost }}</strong> / 10,000</span
        >
        <span>{{ query.data.value.shape }} result</span>
      </div>

      <article v-if="query.data.value.shape === 'number'" class="explore-number">
        <span>{{ columns[0] }}</span>
        <strong>{{ Number(query.data.value.items[0]?.[columns[0]] ?? 0).toLocaleString() }}</strong>
      </article>

      <div v-else-if="query.data.value.shape === 'timeseries'" class="explore-chart">
        <article
          v-for="(item, index) in query.data.value.items"
          :key="`${item.timestamp}:${index}`"
        >
          <div>
            <strong>{{ Number(item[resultValueColumn] ?? 0).toLocaleString() }}</strong>
            <span>{{ display(item.timestamp ?? null, 'timestamp') }}</span>
          </div>
          <span
            class="explore-chart__bar"
            :style="{
              '--bar-width': `${(Number(item[resultValueColumn] ?? 0) / chartMaximum) * 100}%`,
            }"
          ></span>
          <small v-if="groupField">{{ display(item[groupField] ?? null, groupField) }}</small>
        </article>
      </div>

      <div v-else class="issue-table-wrap">
        <div v-if="query.data.value.shape === 'table'" class="pagination">
          <button
            class="button button--secondary"
            type="button"
            :disabled="!query.data.value.next_cursor"
            @click="run(query.data.value.next_cursor)"
          >
            Next page
          </button>
        </div>
        <table class="issue-table explore-table">
          <thead>
            <tr>
              <th v-for="column in columns" :key="column">{{ column.replaceAll('_', ' ') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(item, index) in query.data.value.items" :key="String(item.id ?? index)">
              <td v-for="column in columns" :key="column">{{ display(item[column], column) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </section>
</template>
