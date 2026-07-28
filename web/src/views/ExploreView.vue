<script setup lang="ts">
import { computed, ref } from 'vue';
import { useMutation } from '@tanstack/vue-query';
import type { ExploreDataset, ExploreRequest, ExploreScalar } from '../api/types';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import { useSessionStore } from '../stores/session';

type ExploreMode = 'table' | 'number' | 'timeseries';

const session = useSessionStore();
const dataset = ref<ExploreDataset>('errors');
const mode = ref<ExploreMode>('table');
const filterField = ref('');
const filterValue = ref('');
const groupField = ref('');
const interval = ref<'5m' | '1h' | '1d'>('1h');
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

const query = useMutation({
  mutationFn: (request: ExploreRequest) => api.explore(projectId.value, request),
});
const columns = computed(() => {
  const first = query.data.value?.items[0];
  return first ? Object.keys(first) : [];
});
const chartMaximum = computed(() =>
  Math.max(1, ...(query.data.value?.items ?? []).map((item) => Number(item.count ?? 0))),
);

function validSelection(options: SelectOption[], value: string): string {
  return options.some((option) => option.value === value) ? value : '';
}

async function run(nextCursor: string | null = null): Promise<void> {
  filterField.value = validSelection(fieldsByDataset[dataset.value], filterField.value);
  groupField.value = validSelection(groupFieldsByDataset[dataset.value], groupField.value);
  cursor.value = nextCursor;
  const until = Date.now();
  const predicates: ExploreRequest['predicates'] = [];
  if (filterField.value && filterValue.value.trim()) {
    const raw = filterValue.value.trim();
    predicates.push({
      field: filterField.value,
      op: 'exact',
      value: filterField.value === 'is_segment' ? raw === 'true' : raw,
    });
  }
  const aggregate =
    mode.value === 'table'
      ? []
      : dataset.value === 'metrics'
        ? [{ function: 'sum' as const, field: 'metric_count', alias: 'count' }]
        : [{ function: 'count' as const, alias: 'count' }];
  await query.mutateAsync({
    dataset: dataset.value,
    from: until - 24 * 60 * 60 * 1000,
    until,
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
</script>

<template>
  <section class="explore-page">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / explore</p>
        <h1>Unified Explore</h1>
        <p>
          Ask one bounded question of Errors, Logs, Spans, or Metrics without raw database syntax.
        </p>
      </div>
    </header>

    <form class="panel explore-builder" @submit.prevent="run(null)">
      <BaseSelect v-model="dataset" :options="datasetOptions" label="Dataset" />
      <BaseSelect v-model="mode" :options="modeOptions" label="Result" />
      <BaseSelect v-model="filterField" :options="optionalFields" label="Exact filter" />
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

    <LoadingPanel v-if="query.isPending.value" label="Running bounded query…" />
    <ApiErrorPanel v-else-if="query.error.value" :error="query.error.value" @retry="run(cursor)" />
    <EmptyState
      v-else-if="!query.data.value"
      icon="explore"
      title="Build your first query"
      description="Choose a dataset and result shape. Project scope and a 24-hour range are always enforced."
    />
    <EmptyState
      v-else-if="!query.data.value.items.length"
      icon="search"
      title="No matching signals"
      description="The query completed safely but found no rows in the selected time range."
    />
    <template v-else>
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
            <strong>{{ Number(item.count ?? 0).toLocaleString() }}</strong>
            <span>{{ display(item.timestamp ?? null, 'timestamp') }}</span>
          </div>
          <span
            class="explore-chart__bar"
            :style="{ '--bar-width': `${(Number(item.count ?? 0) / chartMaximum) * 100}%` }"
          ></span>
          <small v-if="groupField">{{ display(item[groupField] ?? null, groupField) }}</small>
        </article>
      </div>

      <div v-else class="issue-table-wrap">
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
    </template>
  </section>
</template>
