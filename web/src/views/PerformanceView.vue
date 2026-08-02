<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TraceSectionNav from '../components/TraceSectionNav.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import UnifiedQueryBar from '../components/UnifiedQueryBar.vue';
import { api } from '../api/client';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const { locale, t } = useI18n();
const performanceFields = ['service', 'environment', 'release'];
const initialQuery = typeof route.query.q === 'string' ? route.query.q : '';
const initialFilters = parsePerformanceFilters(initialQuery);
const queryText = ref(initialQuery);
const appliedFilters = ref(initialFilters ?? {});
const queryError = ref(initialFilters ? '' : t('performance.queryInvalid'));
const range = ref('all');
const appliedRange = ref('all');
const selectedWindow = ref(timeWindow('all'));
const appliedWindow = ref({ ...selectedWindow.value });
const projectId = computed(() => session.selectedProjectId ?? '');
const hasFilters = computed(() => Boolean(queryText.value.trim()) || range.value !== 'all');
const performance = useQuery({
  queryKey: computed(() => [
    'performance',
    projectId.value,
    appliedFilters.value.service,
    appliedFilters.value.environment,
    appliedFilters.value.release,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
  ]),
  queryFn: () =>
    api.performance(projectId.value, {
      ...optionalTimeWindow(appliedRange.value, appliedWindow.value),
      service: appliedFilters.value.service,
      environment: appliedFilters.value.environment,
      release: appliedFilters.value.release,
    }),
  enabled: computed(() => Boolean(projectId.value) && !queryError.value),
});
const total = computed(() =>
  (performance.data.value?.items ?? []).reduce((sum, item) => sum + item.count, 0),
);
const failed = computed(() =>
  (performance.data.value?.items ?? []).reduce((sum, item) => sum + item.failure_count, 0),
);

function applyFilters(): void {
  const filters = parsePerformanceFilters(queryText.value);
  if (!filters) {
    queryError.value = t('performance.queryInvalid');
    return;
  }
  queryError.value = '';
  appliedFilters.value = filters;
  appliedRange.value = range.value;
  appliedWindow.value = { ...selectedWindow.value };
}

function resetFilters(): void {
  queryText.value = '';
  range.value = 'all';
  selectedWindow.value = timeWindow('all');
  applyFilters();
}

interface PerformanceFilters {
  service?: string;
  environment?: string;
  release?: string;
}

function parsePerformanceFilters(value: string): PerformanceFilters | null {
  const query = value.trim();
  if (!query) return {};
  const aliases: Record<string, keyof PerformanceFilters> = {
    svc: 'service',
    service: 'service',
    env: 'environment',
    environment: 'environment',
    rel: 'release',
    release: 'release',
  };
  const token =
    /\s*(?:(?:AND)\s+)?(svc|service|env|environment|rel|release):(?:"((?:\\.|[^"])*)"|([^\s()]+))/iy;
  const filters: PerformanceFilters = {};
  let offset = 0;
  while (offset < query.length) {
    token.lastIndex = offset;
    const match = token.exec(query);
    if (!match || match.index !== offset) return null;
    const alias = match[1];
    const field = alias ? aliases[alias.toLowerCase()] : undefined;
    if (!field || filters[field] !== undefined) return null;
    const raw = match[2] ?? match[3] ?? '';
    const decoded = raw.replace(/\\(["\\])/g, '$1');
    if (!decoded) return null;
    filters[field] = decoded;
    offset = token.lastIndex;
  }
  return filters;
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{ $t('performance.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('performance.title') }}</h1>
        <p>{{ $t('performance.description') }}</p>
      </div>
    </header>
    <TraceSectionNav />
    <UnifiedQueryBar
      v-model="queryText"
      source="traces"
      :allowed-fields="performanceFields"
      :placeholder="$t('unifiedQuery.placeholders.performance')"
      :show-reset="hasFilters"
      @submit="applyFilters"
      @reset="resetFilters"
    >
      <template #actions>
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          :aria-label="$t('performance.timeRange')"
          @update:window-value="selectedWindow = $event"
        />
      </template>
    </UnifiedQueryBar>
    <p v-if="queryError" class="field-error performance-query-error" role="alert">
      {{ queryError }}
    </p>
    <LoadingPanel
      v-if="!queryError && performance.isPending.value"
      :label="$t('performance.loading')"
    />
    <ApiErrorPanel
      v-else-if="!queryError && performance.error.value"
      :error="performance.error.value"
      @retry="performance.refetch()"
    />
    <EmptyState
      v-else-if="!queryError && !performance.data.value?.items.length"
      icon="gauge"
      :title="$t('performance.empty')"
      :description="$t('performance.emptyDescription')"
    >
      <SdkSetupButton />
    </EmptyState>
    <template v-else-if="!queryError">
      <div class="metric-grid">
        <article>
          <span>{{ $t('performance.transactions') }}</span
          ><strong>{{ total.toLocaleString(locale) }}</strong>
        </article>
        <article>
          <span>{{ $t('performance.failures') }}</span
          ><strong>{{ failed.toLocaleString(locale) }}</strong>
        </article>
        <article>
          <span>{{ $t('performance.failureRate') }}</span>
          <strong>{{ total ? ((failed / total) * 100).toFixed(2) : '0.00' }}%</strong>
        </article>
        <article>
          <span>{{ $t('performance.model') }}</span
          ><strong>{{ $t('performance.boundedSample') }}</strong>
        </article>
      </div>
      <div class="issue-table-wrap performance-table">
        <table class="issue-table">
          <thead>
            <tr>
              <th>{{ $t('performance.transaction') }}</th>
              <th>{{ $t('performance.throughput') }}</th>
              <th>{{ $t('performance.failure') }}</th>
              <th>{{ $t('performance.average') }}</th>
              <th>p95</th>
              <th>p99</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="item in performance.data.value?.items ?? []"
              :key="`${item.hour}:${item.name}:${item.service}`"
            >
              <td>
                <RouterLink class="text-link" :to="`/traces/${item.representative_trace_id}`">
                  <strong>{{ item.name }}</strong>
                </RouterLink>
                <span>
                  {{ item.service || $t('performance.service') }} · {{ item.operation }}
                  <template v-if="item.environment"> · {{ item.environment }}</template>
                  <template v-if="item.release"> · {{ item.release }}</template>
                </span>
              </td>
              <td>{{ item.count.toLocaleString(locale) }}</td>
              <td>{{ (item.failure_rate * 100).toFixed(1) }}%</td>
              <td>{{ item.average_duration_ms.toFixed(1) }} ms</td>
              <td>{{ item.p95_ms.toFixed(1) }} ms</td>
              <td>{{ item.p99_ms.toFixed(1) }} ms</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="approximation-note">
        {{ $t('performance.approximation') }}
      </p>
    </template>
  </section>
</template>
