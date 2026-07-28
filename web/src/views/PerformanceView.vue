<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TraceSectionNav from '../components/TraceSectionNav.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import { api } from '../api/client';
import { timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const service = ref('');
const environment = ref('');
const release = ref('');
const appliedService = ref('');
const appliedEnvironment = ref('');
const appliedRelease = ref('');
const range = ref('24h');
const appliedRange = ref('24h');
const selectedWindow = ref(timeWindow('24h'));
const appliedWindow = ref({ ...selectedWindow.value });
const projectId = computed(() => session.selectedProjectId ?? '');
const hasFilters = computed(
  () =>
    Boolean(service.value.trim() || environment.value.trim() || release.value.trim()) ||
    range.value !== '24h',
);
const performance = useQuery({
  queryKey: computed(() => [
    'performance',
    projectId.value,
    appliedService.value,
    appliedEnvironment.value,
    appliedRelease.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
  ]),
  queryFn: () =>
    api.performance(projectId.value, {
      ...appliedWindow.value,
      service: appliedService.value || undefined,
      environment: appliedEnvironment.value || undefined,
      release: appliedRelease.value || undefined,
    }),
  enabled: computed(() => Boolean(projectId.value)),
});
const total = computed(() =>
  (performance.data.value?.items ?? []).reduce((sum, item) => sum + item.count, 0),
);
const failed = computed(() =>
  (performance.data.value?.items ?? []).reduce((sum, item) => sum + item.failure_count, 0),
);

function applyFilters(): void {
  appliedService.value = service.value.trim();
  appliedEnvironment.value = environment.value.trim();
  appliedRelease.value = release.value.trim();
  appliedRange.value = range.value;
  appliedWindow.value = { ...selectedWindow.value };
}

function resetFilters(): void {
  service.value = '';
  environment.value = '';
  release.value = '';
  range.value = '24h';
  selectedWindow.value = timeWindow('24h');
  applyFilters();
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / performance</p>
        <h1>Performance Insights</h1>
        <p>Bounded hourly summaries built from durable root spans.</p>
      </div>
    </header>
    <TraceSectionNav />
    <form
      class="signal-toolbar signal-toolbar--compact"
      role="search"
      @submit.prevent="applyFilters"
    >
      <label>
        <span class="sr-only">Service</span>
        <input v-model="service" maxlength="256" placeholder="Service" />
      </label>
      <label>
        <span class="sr-only">Environment</span>
        <input v-model="environment" maxlength="128" placeholder="Environment" />
      </label>
      <label>
        <span class="sr-only">Release</span>
        <input v-model="release" maxlength="256" placeholder="Release" />
      </label>
      <div class="signal-toolbar__actions">
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          aria-label="Performance time range"
          @update:window-value="selectedWindow = $event"
        />
        <button class="button button--primary" type="submit">
          <AppIcon name="search" :size="16" />
          Search
        </button>
        <button
          v-if="hasFilters"
          class="button button--secondary"
          type="button"
          @click="resetFilters"
        >
          <AppIcon name="close" :size="16" />
          Reset
        </button>
      </div>
    </form>
    <LoadingPanel v-if="performance.isPending.value" label="Loading performance summaries…" />
    <ApiErrorPanel
      v-else-if="performance.error.value"
      :error="performance.error.value"
      @retry="performance.refetch()"
    />
    <EmptyState
      v-else-if="!performance.data.value?.items.length"
      icon="gauge"
      title="No performance data"
      description="Performance buckets are created after a root transaction or segment is accepted."
    >
      <SdkSetupButton />
    </EmptyState>
    <template v-else>
      <div class="metric-grid">
        <article>
          <span>Transactions</span><strong>{{ total.toLocaleString() }}</strong>
        </article>
        <article>
          <span>Failures</span><strong>{{ failed.toLocaleString() }}</strong>
        </article>
        <article>
          <span>Failure rate</span>
          <strong>{{ total ? ((failed / total) * 100).toFixed(2) : '0.00' }}%</strong>
        </article>
        <article><span>Model</span><strong>Bounded sample</strong></article>
      </div>
      <div class="issue-table-wrap performance-table">
        <table class="issue-table">
          <thead>
            <tr>
              <th>Transaction</th>
              <th>Throughput</th>
              <th>Failure</th>
              <th>Average</th>
              <th>p95</th>
              <th>p99</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="item in performance.data.value.items"
              :key="`${item.hour}:${item.name}:${item.service}`"
            >
              <td>
                <RouterLink class="text-link" :to="`/traces/${item.representative_trace_id}`">
                  <strong>{{ item.name }}</strong>
                </RouterLink>
                <span>
                  {{ item.service || 'service' }} · {{ item.operation }}
                  <template v-if="item.environment"> · {{ item.environment }}</template>
                  <template v-if="item.release"> · {{ item.release }}</template>
                </span>
              </td>
              <td>{{ item.count.toLocaleString() }}</td>
              <td>{{ (item.failure_rate * 100).toFixed(1) }}%</td>
              <td>{{ item.average_duration_ms.toFixed(1) }} ms</td>
              <td>{{ item.p95_ms.toFixed(1) }} ms</td>
              <td>{{ item.p99_ms.toFixed(1) }} ms</td>
            </tr>
          </tbody>
        </table>
      </div>
      <p class="approximation-note">
        Percentiles are approximate: each hourly dimension keeps at most 2,048 recent duration
        samples. Durable spans remain the source of truth and aggregates can be rebuilt.
      </p>
    </template>
  </section>
</template>
