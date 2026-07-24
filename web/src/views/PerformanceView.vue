<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import { api } from '../api/client';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const service = ref('');
const environment = ref('');
const release = ref('');
const projectId = computed(() => session.selectedProjectId ?? '');
const performance = useQuery({
  queryKey: computed(() => [
    'performance',
    projectId.value,
    service.value,
    environment.value,
    release.value,
  ]),
  queryFn: () =>
    api.performance(projectId.value, {
      service: service.value.trim() || undefined,
      environment: environment.value.trim() || undefined,
      release: release.value.trim() || undefined,
    }),
  enabled: computed(() => Boolean(projectId.value)),
});
const total = computed(() =>
  (performance.data.value?.items ?? []).reduce((sum, item) => sum + item.count, 0),
);
const failed = computed(() =>
  (performance.data.value?.items ?? []).reduce((sum, item) => sum + item.failure_count, 0),
);
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / performance</p>
        <h1>Performance Insights</h1>
        <p>Bounded hourly summaries built from durable root spans.</p>
      </div>
      <div class="compact-filter-group">
        <label class="compact-filter">
          <span>Service</span>
          <input v-model="service" maxlength="256" placeholder="All services" />
        </label>
        <label class="compact-filter">
          <span>Environment</span>
          <input v-model="environment" maxlength="128" placeholder="All environments" />
        </label>
        <label class="compact-filter">
          <span>Release</span>
          <input v-model="release" maxlength="256" placeholder="All releases" />
        </label>
      </div>
    </header>
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
