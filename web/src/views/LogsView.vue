<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import UnifiedQueryBar from '../components/UnifiedQueryBar.vue';
import { api } from '../api/client';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

interface LogQueryRow {
  id: string;
  timestamp: number;
  received_at: number;
  level: string;
  message: string;
  environment: string | null;
  release: string | null;
  service: string | null;
  trace_id: string | null;
  span_id: string | null;
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

const logs = useQuery({
  queryKey: computed(() => [
    'unified-query',
    'logs',
    projectId.value,
    appliedQuery.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
    cursor.value,
  ]),
  queryFn: () =>
    api.query<LogQueryRow>(projectId.value, {
      source: 'logs',
      query: appliedQuery.value,
      ...optionalTimeWindow(appliedRange.value, appliedWindow.value),
      result: { kind: 'records' },
      cursor: cursor.value,
      limit: 50,
    }),
  enabled: computed(() => Boolean(projectId.value)),
});

watch(projectId, resetPage);

const levelCounts = computed(() => {
  const counts = new Map<string, number>();
  for (const log of logs.data.value?.items ?? []) {
    counts.set(log.level, (counts.get(log.level) ?? 0) + 1);
  }
  return ['trace', 'debug', 'info', 'warning', 'error', 'fatal'].map((level) => ({
    level,
    count: counts.get(level) ?? 0,
  }));
});
const maximumLevelCount = computed(() =>
  Math.max(1, ...levelCounts.value.map((entry) => entry.count)),
);

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
  const next = logs.data.value?.next_cursor;
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
          {{ $t('logs.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('logs.title') }}</h1>
        <p>{{ $t('logs.description') }}</p>
      </div>
    </header>

    <UnifiedQueryBar
      v-model="query"
      source="logs"
      :show-reset="hasFilters"
      exportable
      :export-from="optionalTimeWindow(range, selectedWindow).from"
      :export-until="optionalTimeWindow(range, selectedWindow).until"
      @submit="submitQuery"
      @reset="resetFilters"
    >
      <template #actions>
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          :aria-label="$t('logs.timeRange')"
          @update:window-value="selectedWindow = $event"
        />
      </template>
    </UnifiedQueryBar>

    <LoadingPanel v-if="logs.isPending.value" :label="$t('logs.loading')" />
    <ApiErrorPanel v-else-if="logs.error.value" :error="logs.error.value" @retry="logs.refetch()" />
    <EmptyState
      v-else-if="!logs.data.value?.items.length"
      icon="logs"
      :title="$t('logs.empty')"
      :description="$t('logs.emptyDescription')"
    >
      <SdkSetupButton v-if="!appliedQuery" />
    </EmptyState>
    <div v-else class="signal-list">
      <nav class="pagination" :aria-label="$t('logs.resultPages')">
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
          :disabled="!logs.data.value.next_cursor"
          @click="nextPage"
        >
          {{ $t('common.next') }}
        </button>
      </nav>
      <div class="signal-histogram" :aria-label="$t('logs.levelsOnPage')">
        <span
          v-for="entry in levelCounts"
          :key="entry.level"
          :class="`signal-accent--${entry.level}`"
        >
          <i :style="{ '--level-height': `${(entry.count / maximumLevelCount) * 100}%` }"></i>
          <small>{{ $t(`status.${entry.level}`) }} {{ entry.count }}</small>
        </span>
      </div>
      <article
        v-for="log in logs.data.value.items"
        :key="log.id"
        class="log-row"
        :class="`signal-accent--${log.level}`"
      >
        <span class="signal-level">{{ $t(`status.${log.level}`) }}</span>
        <RouterLink class="signal-title" :to="`/logs/${log.id}`">{{ log.message }}</RouterLink>
        <span>{{ log.service || $t('logs.unknownService') }}</span>
        <RouterLink v-if="log.trace_id" class="text-link" :to="`/traces/${log.trace_id}`">
          {{ $t('logs.trace', { id: log.trace_id.slice(0, 8) }) }}
        </RouterLink>
        <time :datetime="new Date(log.timestamp).toISOString()">{{
          formatTime(log.timestamp)
        }}</time>
      </article>
    </div>
  </section>
</template>
