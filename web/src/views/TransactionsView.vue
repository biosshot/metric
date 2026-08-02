<script setup lang="ts">
import { computed, ref, watch } from 'vue';
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

interface TraceQueryRow {
  id: string;
  timestamp: number;
  received_at: number;
  duration_ms: number;
  trace_id: string;
  span_id: string;
  operation_class: string;
  operation: string | null;
  status: string | null;
  name: string;
  environment: string | null;
  release: string | null;
  service: string | null;
  is_segment: boolean;
}

const session = useSessionStore();
const route = useRoute();
const { locale } = useI18n();
const routeQuery = typeof route.query.q === 'string' ? route.query.q : '';
const releaseQuery =
  typeof route.query.release === 'string'
    ? `rel:"${route.query.release.replaceAll('"', '\\"')}"`
    : '';
const query = ref(routeQuery || releaseQuery);
const appliedQuery = ref(query.value);
const range = ref('all');
const appliedRange = ref('all');
const selectedWindow = ref(timeWindow('all'));
const appliedWindow = ref({ ...selectedWindow.value });
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const projectId = computed(() => session.selectedProjectId ?? '');
const hasFilters = computed(
  () => Boolean(query.value.trim() || appliedQuery.value) || range.value !== 'all',
);

const transactions = useQuery({
  queryKey: computed(() => [
    'unified-query',
    'traces',
    projectId.value,
    appliedQuery.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
    cursor.value,
  ]),
  queryFn: () =>
    api.query<TraceQueryRow>(projectId.value, {
      source: 'traces',
      query: appliedQuery.value,
      ...optionalTimeWindow(appliedRange.value, appliedWindow.value),
      result: { kind: 'records' },
      cursor: cursor.value,
      limit: 50,
    }),
  enabled: computed(() => Boolean(projectId.value)),
});

watch(projectId, resetPage);

function submitQuery(): void {
  appliedQuery.value = query.value.trim();
  appliedRange.value = range.value;
  appliedWindow.value = { ...selectedWindow.value };
  resetPage();
}

function resetFilters(): void {
  query.value = '';
  appliedQuery.value = '';
  range.value = 'all';
  appliedRange.value = 'all';
  selectedWindow.value = timeWindow('all');
  appliedWindow.value = { ...selectedWindow.value };
  resetPage();
}

function resetPage(): void {
  cursor.value = null;
  history.value = [];
}

function nextPage(): void {
  const next = transactions.data.value?.next_cursor;
  if (!next) return;
  history.value.push(cursor.value);
  cursor.value = next;
}

function previousPage(): void {
  cursor.value = history.value.pop() ?? null;
}

function formatTime(value: number): string {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'medium',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{ $t('transactions.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('transactions.title') }}</h1>
        <p>{{ $t('transactions.description') }}</p>
      </div>
    </header>
    <TraceSectionNav />
    <UnifiedQueryBar
      v-model="query"
      source="traces"
      :show-reset="hasFilters"
      @submit="submitQuery"
      @reset="resetFilters"
    >
      <template #actions>
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          :aria-label="$t('transactions.timeRange')"
          @update:window-value="selectedWindow = $event"
        />
      </template>
    </UnifiedQueryBar>
    <LoadingPanel v-if="transactions.isPending.value" :label="$t('transactions.loading')" />
    <ApiErrorPanel
      v-else-if="transactions.error.value"
      :error="transactions.error.value"
      @retry="transactions.refetch()"
    />
    <EmptyState
      v-else-if="!transactions.data.value?.items.length"
      icon="traces"
      :title="$t('transactions.empty')"
      :description="$t('transactions.emptyDescription')"
    >
      <SdkSetupButton v-if="!appliedQuery" />
    </EmptyState>
    <div v-else class="transaction-list">
      <nav class="pagination" :aria-label="$t('transactions.pages')">
        <button
          class="button button--secondary"
          type="button"
          :disabled="!history.length"
          @click="previousPage"
        >
          {{ $t('common.previous') }}
        </button>
        <span>{{ $t('common.page', { page: history.length + 1 }) }}</span>
        <button
          class="button button--secondary"
          type="button"
          :disabled="!transactions.data.value.next_cursor"
          @click="nextPage"
        >
          {{ $t('common.next') }}
        </button>
      </nav>
      <RouterLink
        v-for="transaction in transactions.data.value.items"
        :key="transaction.id"
        class="transaction-row"
        :to="`/traces/${transaction.trace_id}`"
      >
        <div>
          <strong>{{ transaction.name }}</strong>
          <span>
            {{ transaction.service || $t('transactions.unknownService') }} ·
            {{ transaction.operation || transaction.operation_class }}
          </span>
        </div>
        <span :class="{ 'duration--slow': transaction.duration_ms >= 1000 }">
          {{ transaction.duration_ms.toFixed(1) }} ms
        </span>
        <time :datetime="new Date(transaction.timestamp).toISOString()">
          {{ formatTime(transaction.timestamp) }}
        </time>
      </RouterLink>
    </div>
  </section>
</template>
