<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
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
const { locale, t } = useI18n();
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

const datasetOptions = computed<SelectOption[]>(() => [
  {
    value: 'errors',
    label: t('queryBuilder.errors'),
    icon: 'bug',
    description: t('queryBuilder.errorsHelp'),
  },
  {
    value: 'logs',
    label: t('queryBuilder.logs'),
    icon: 'logs',
    description: t('queryBuilder.logsHelp'),
  },
  {
    value: 'spans',
    label: t('queryBuilder.spans'),
    icon: 'traces',
    description: t('queryBuilder.spansHelp'),
  },
  {
    value: 'metrics',
    label: t('queryBuilder.metrics'),
    icon: 'gauge',
    description: t('queryBuilder.metricsHelp'),
  },
]);
const modeOptions = computed<SelectOption[]>(() => [
  {
    value: 'table',
    label: t('queryBuilder.table'),
    icon: 'clipboard',
    description: t('queryBuilder.tableHelp'),
  },
  {
    value: 'number',
    label: t('queryBuilder.number'),
    icon: 'gauge',
    description: t('queryBuilder.numberHelp'),
  },
  {
    value: 'timeseries',
    label: t('queryBuilder.timeseries'),
    icon: 'activity',
    description: t('queryBuilder.timeseriesHelp'),
  },
]);
const intervalOptions = computed<SelectOption[]>(() => [
  { value: '5m', label: t('queryBuilder.minutes5') },
  { value: '1h', label: t('queryBuilder.hour1') },
  { value: '1d', label: t('queryBuilder.day1') },
]);
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

const fieldsByDataset = computed<Record<ExploreDataset, SelectOption[]>>(() => ({
  errors: [
    { value: 'level', label: t('queryBuilder.level') },
    { value: 'platform', label: t('queryBuilder.platform') },
    { value: 'issue_id', label: t('queryBuilder.issueId') },
  ],
  logs: [
    { value: 'level', label: t('queryBuilder.level') },
    { value: 'environment', label: t('queryBuilder.environment') },
    { value: 'release', label: t('queryBuilder.release') },
    { value: 'service', label: t('queryBuilder.service') },
    { value: 'trace_id', label: t('queryBuilder.traceId') },
  ],
  spans: [
    { value: 'environment', label: t('queryBuilder.environment') },
    { value: 'release', label: t('queryBuilder.release') },
    { value: 'service', label: t('queryBuilder.service') },
    { value: 'operation', label: t('queryBuilder.operation') },
    { value: 'status', label: t('queryBuilder.status') },
    { value: 'is_segment', label: t('queryBuilder.rootSegment') },
  ],
  metrics: [
    { value: 'name', label: t('queryBuilder.metricName') },
    { value: 'metric_kind', label: t('queryBuilder.metricKind') },
    { value: 'unit', label: t('queryBuilder.unit') },
    { value: 'trace_id', label: t('queryBuilder.traceId') },
  ],
}));
const groupFieldsByDataset = computed<Record<ExploreDataset, SelectOption[]>>(() => ({
  errors: [
    { value: 'level', label: t('queryBuilder.level') },
    { value: 'platform', label: t('queryBuilder.platform') },
  ],
  logs: [{ value: 'level', label: t('queryBuilder.level') }],
  spans: [
    { value: 'operation_class', label: t('queryBuilder.operationClass') },
    { value: 'is_segment', label: t('queryBuilder.rootSegment') },
  ],
  metrics: [
    { value: 'metric_kind', label: t('queryBuilder.metricKind') },
    { value: 'unit', label: t('queryBuilder.unit') },
  ],
}));
const optionalFields = computed<SelectOption[]>(() => [
  { value: '', label: t('queryBuilder.noExactFilter') },
  ...fieldsByDataset.value[dataset.value],
]);
const optionalGroups = computed<SelectOption[]>(() => [
  { value: '', label: t('queryBuilder.noGrouping') },
  ...groupFieldsByDataset.value[dataset.value],
]);
const textOperators = computed<SelectOption[]>(() => [
  { value: 'exact', label: t('queryBuilder.equals') },
  { value: 'contains', label: t('queryBuilder.contains') },
  { value: 'starts_with', label: t('queryBuilder.startsWith') },
  { value: 'ends_with', label: t('queryBuilder.endsWith') },
]);
const exactOperator = computed<SelectOption[]>(() => [
  { value: 'exact', label: t('queryBuilder.equals') },
]);
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
  textFields.has(filterField.value) ? textOperators.value : exactOperator.value,
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
  filterField.value = validSelection(fieldsByDataset.value[dataset.value], filterField.value);
  filterOperator.value = validSelection(
    operatorOptions.value,
    filterOperator.value,
  ) as typeof filterOperator.value;
  groupField.value = validSelection(groupFieldsByDataset.value[dataset.value], groupField.value);
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
    return new Date(value).toLocaleString(locale.value);
  }
  if (key === 'duration_ms' && typeof value === 'number') return `${value.toFixed(2)} ms`;
  return String(value);
}

function metricValue(value: ExploreScalar | undefined): string {
  const number = Number(value ?? 0);
  if (!Number.isFinite(number)) return '—';
  return new Intl.NumberFormat(locale.value, { maximumFractionDigits: 3 }).format(number);
}

function datasetLabel(value: ExploreDataset): string {
  return t(`queryBuilder.${value}`);
}

function shapeLabel(value: ExploreMode): string {
  return t(`queryBuilder.${value}`);
}

function fieldLabel(value: string): string {
  const keys: Record<string, string> = {
    count: 'queryBuilder.count',
    duration_ms: 'queryBuilder.duration',
    environment: 'queryBuilder.environment',
    id: 'queryBuilder.id',
    is_segment: 'queryBuilder.rootSegment',
    issue_id: 'queryBuilder.issueId',
    level: 'queryBuilder.level',
    message: 'queryBuilder.message',
    metric_kind: 'queryBuilder.metricKind',
    name: 'queryBuilder.name',
    operation: 'queryBuilder.operation',
    operation_class: 'queryBuilder.operationClass',
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

function formatNumber(value: ExploreScalar | undefined): string {
  return new Intl.NumberFormat(locale.value).format(Number(value ?? 0));
}
</script>

<template>
  <section class="explore-page">
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{
            $t(datasetLocked ? 'explore.eyebrowMetrics' : 'explore.eyebrowExplore', {
              project: session.selectedProject?.slug,
            })
          }}
        </p>
        <h1>{{ datasetLocked ? $t('explore.metricsTitle') : $t('explore.title') }}</h1>
        <p>
          {{ datasetLocked ? $t('explore.metricsDescription') : $t('explore.description') }}
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
          <p class="eyebrow">{{ $t('explore.catalog') }}</p>
          <h2 id="metrics-overview-title">{{ $t('explore.overview') }}</h2>
          <p>{{ $t('explore.overviewHelp') }}</p>
        </div>
        <button
          class="button button--secondary button--fit"
          type="button"
          :disabled="metricsOverview.isFetching.value"
          @click="metricsOverview.refetch()"
        >
          <AppIcon name="refresh" :size="16" />
          {{ $t('explore.refresh') }}
        </button>
      </div>
      <LoadingPanel v-if="metricsOverview.isPending.value" :label="$t('explore.loadingOverview')" />
      <ApiErrorPanel
        v-else-if="metricsOverview.error.value"
        :error="metricsOverview.error.value"
        :title="$t('explore.overviewFailed')"
        @retry="metricsOverview.refetch()"
      />
      <EmptyState
        v-else-if="!metricOverviewItems.length"
        icon="gauge"
        :title="$t('explore.noMetrics')"
        :description="$t('explore.noMetricsHelp')"
      />
      <div v-else class="dashboard-widgets metrics-overview__grid">
        <article
          v-for="(item, index) in metricOverviewItems"
          :key="`${item.name}:${index}`"
          class="dashboard-widget"
        >
          <header>
            <div>
              <p class="eyebrow">{{ $t('explore.metric') }}</p>
              <h4>{{ item.name ?? $t('explore.unnamedMetric') }}</h4>
            </div>
            <span>{{ $t('explore.samples', { count: metricValue(item.samples) }) }}</span>
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
        :label="$t('queryBuilder.dataset')"
      />
      <BaseSelect v-model="mode" :options="modeOptions" :label="$t('queryBuilder.result')" />
      <TimeRangeSelect
        v-model="range"
        :window-value="selectedWindow"
        :label="$t('queryBuilder.timeRange')"
        @update:window-value="selectedWindow = $event"
      />
      <BaseSelect
        v-if="dataset === 'metrics' && mode !== 'table'"
        v-model="metricMeasure"
        :options="metricMeasureOptions"
        :label="$t('queryBuilder.measure')"
      />
      <BaseSelect
        v-model="filterField"
        :options="optionalFields"
        :label="$t('queryBuilder.exactFilter')"
      />
      <BaseSelect
        v-if="filterField"
        v-model="filterOperator"
        :options="operatorOptions"
        :label="$t('queryBuilder.match')"
      />
      <label>
        <span>{{ $t('queryBuilder.value') }}</span>
        <input
          v-model="filterValue"
          maxlength="256"
          :disabled="!filterField"
          :placeholder="$t('queryBuilder.exactValue')"
        />
      </label>
      <BaseSelect
        v-if="mode === 'timeseries'"
        v-model="interval"
        :options="intervalOptions"
        :label="$t('queryBuilder.interval')"
      />
      <BaseSelect
        v-if="mode === 'timeseries'"
        v-model="groupField"
        :options="optionalGroups"
        :label="$t('queryBuilder.groupBy')"
      />
      <button
        class="button button--primary explore-run"
        type="submit"
        :disabled="query.isPending.value"
      >
        <AppIcon name="search" :size="17" />
        {{ $t('explore.runQuery') }}
      </button>
    </form>

    <LoadingPanel
      v-if="(!datasetLocked || metricsView === 'query') && query.isPending.value"
      :label="$t('explore.running')"
    />
    <ApiErrorPanel
      v-else-if="(!datasetLocked || metricsView === 'query') && query.error.value"
      :error="query.error.value"
      @retry="run(cursor)"
    />
    <EmptyState
      v-else-if="(!datasetLocked || metricsView === 'query') && !query.data.value"
      icon="explore"
      :title="$t('explore.buildFirst')"
      :description="datasetLocked ? $t('explore.buildMetricsHelp') : $t('explore.buildHelp')"
    />
    <EmptyState
      v-else-if="
        (!datasetLocked || metricsView === 'query') &&
        query.data.value &&
        !query.data.value.items.length
      "
      icon="search"
      :title="$t('explore.noMatches')"
      :description="$t('explore.noMatchesHelp')"
    />
    <template v-else-if="(!datasetLocked || metricsView === 'query') && query.data.value">
      <div class="explore-result-meta">
        <span>
          {{
            $t('explore.datasetMeta', {
              dataset: datasetLabel(query.data.value.dataset),
            })
          }}
        </span>
        <span>
          {{
            $t('explore.estimatedCost', {
              cost: formatNumber(query.data.value.cost),
            })
          }}
        </span>
        <span>{{ $t('explore.resultMeta', { shape: shapeLabel(query.data.value.shape) }) }}</span>
      </div>

      <article v-if="query.data.value.shape === 'number'" class="explore-number">
        <span>{{ fieldLabel(columns[0]) }}</span>
        <strong>{{ formatNumber(query.data.value.items[0]?.[columns[0]]) }}</strong>
      </article>

      <div v-else-if="query.data.value.shape === 'timeseries'" class="explore-chart">
        <article
          v-for="(item, index) in query.data.value.items"
          :key="`${item.timestamp}:${index}`"
        >
          <div>
            <strong>{{ formatNumber(item[resultValueColumn]) }}</strong>
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
            {{ $t('explore.nextPage') }}
          </button>
        </div>
        <table class="issue-table explore-table">
          <thead>
            <tr>
              <th v-for="column in columns" :key="column">{{ fieldLabel(column) }}</th>
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
