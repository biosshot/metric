<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TraceSectionNav from '../components/TraceSectionNav.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import { api } from '../api/client';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const { locale } = useI18n();
const service = ref('');
const environment = ref('');
const release = ref('');
const appliedService = ref('');
const appliedEnvironment = ref('');
const appliedRelease = ref('');
const range = ref('all');
const appliedRange = ref('all');
const selectedWindow = ref(timeWindow('all'));
const appliedWindow = ref({ ...selectedWindow.value });
const projectId = computed(() => session.selectedProjectId ?? '');
const hasFilters = computed(
  () =>
    Boolean(service.value.trim() || environment.value.trim() || release.value.trim()) ||
    range.value !== 'all',
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
      ...optionalTimeWindow(appliedRange.value, appliedWindow.value),
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
  range.value = 'all';
  selectedWindow.value = timeWindow('all');
  applyFilters();
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
    <form
      class="signal-toolbar signal-toolbar--compact"
      role="search"
      @submit.prevent="applyFilters"
    >
      <label>
        <span class="sr-only">{{ $t('transactions.service') }}</span>
        <input v-model="service" maxlength="256" :placeholder="$t('transactions.service')" />
      </label>
      <label>
        <span class="sr-only">{{ $t('transactions.environment') }}</span>
        <input
          v-model="environment"
          maxlength="128"
          :placeholder="$t('transactions.environment')"
        />
      </label>
      <label>
        <span class="sr-only">{{ $t('transactions.release') }}</span>
        <input v-model="release" maxlength="256" :placeholder="$t('transactions.release')" />
      </label>
      <div class="signal-toolbar__actions">
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          :aria-label="$t('performance.timeRange')"
          @update:window-value="selectedWindow = $event"
        />
        <button class="button button--primary" type="submit">
          <AppIcon name="search" :size="16" />
          {{ $t('common.search') }}
        </button>
        <button
          v-if="hasFilters"
          class="button button--secondary"
          type="button"
          @click="resetFilters"
        >
          <AppIcon name="close" :size="16" />
          {{ $t('common.reset') }}
        </button>
      </div>
    </form>
    <LoadingPanel v-if="performance.isPending.value" :label="$t('performance.loading')" />
    <ApiErrorPanel
      v-else-if="performance.error.value"
      :error="performance.error.value"
      @retry="performance.refetch()"
    />
    <EmptyState
      v-else-if="!performance.data.value?.items.length"
      icon="gauge"
      :title="$t('performance.empty')"
      :description="$t('performance.emptyDescription')"
    >
      <SdkSetupButton />
    </EmptyState>
    <template v-else>
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
              v-for="item in performance.data.value.items"
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
