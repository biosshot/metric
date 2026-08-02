<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import UnifiedQueryBar from '../components/UnifiedQueryBar.vue';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';
import type { Issue } from '../api/types';

const session = useSessionStore();
const route = useRoute();
const { locale } = useI18n();
const defaultQuery = 'status:open';
const initialQuery = typeof route.query.q === 'string' ? route.query.q : defaultQuery;
const query = ref(initialQuery);
const appliedQuery = ref(initialQuery);
const range = ref('all');
const appliedRange = ref('all');
const selectedWindow = ref(timeWindow('all'));
const appliedWindow = ref({ ...selectedWindow.value });
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const projectId = computed(() => session.selectedProjectId ?? '');
const hasFilters = computed(
  () =>
    query.value.trim() !== defaultQuery ||
    appliedQuery.value !== defaultQuery ||
    range.value !== 'all',
);
const isDefaultView = computed(
  () => appliedQuery.value === defaultQuery && appliedRange.value === 'all',
);

const result = useQuery({
  queryKey: computed(() => [
    'unified-query',
    'issues',
    projectId.value,
    appliedQuery.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
    cursor.value,
  ]),
  queryFn: () =>
    api.query<Issue>(projectId.value, {
      source: 'issues',
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
  query.value = defaultQuery;
  appliedQuery.value = defaultQuery;
  range.value = 'all';
  appliedRange.value = 'all';
  selectedWindow.value = timeWindow('all');
  appliedWindow.value = { ...selectedWindow.value };
  resetPage();
}

function nextPage(): void {
  const next = result.data.value?.next_cursor;
  if (!next) return;
  history.value.push(cursor.value);
  cursor.value = next;
}

function previousPage(): void {
  cursor.value = history.value.pop() ?? null;
}

function resetPage(): void {
  cursor.value = null;
  history.value = [];
}

function formatTime(value: string): string {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{ $t('issues.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('issues.title') }}</h1>
        <p>{{ $t('issues.description') }}</p>
      </div>
    </header>

    <UnifiedQueryBar
      v-model="query"
      source="issues"
      :default-query="defaultQuery"
      :show-reset="hasFilters"
      @submit="submitQuery"
      @reset="resetFilters"
    >
      <template #actions>
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          :aria-label="$t('issues.timeRange')"
          @update:window-value="selectedWindow = $event"
        />
      </template>
    </UnifiedQueryBar>

    <LoadingPanel v-if="result.isPending.value" :label="$t('issues.loading')" />
    <ApiErrorPanel
      v-else-if="result.error.value"
      :error="result.error.value"
      @retry="result.refetch()"
    />
    <EmptyState
      v-else-if="!result.data.value?.items.length"
      :title="isDefaultView ? $t('issues.empty') : $t('issues.noMatches')"
      :description="
        isDefaultView ? $t('issues.emptyDescription') : $t('issues.noMatchesDescription')
      "
    >
      <SdkSetupButton v-if="isDefaultView" />
    </EmptyState>

    <div v-else class="issue-table-wrap">
      <nav class="pagination" :aria-label="$t('issues.resultPages')">
        <button
          class="button button--secondary"
          type="button"
          :disabled="history.length === 0"
          @click="previousPage"
        >
          {{ $t('common.previous') }}
        </button>
        <span>{{ $t('common.page', { page: history.length + 1 }) }}</span>
        <button
          class="button button--secondary"
          type="button"
          :disabled="!result.data.value.next_cursor"
          @click="nextPage"
        >
          {{ $t('common.next') }}
        </button>
      </nav>
      <div class="issue-table-scroll">
        <table class="issue-table">
          <thead>
            <tr>
              <th scope="col">{{ $t('issues.issue') }}</th>
              <th scope="col">{{ $t('issues.status') }}</th>
              <th scope="col">{{ $t('issues.events') }}</th>
              <th scope="col">{{ $t('issues.lastSeen') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="issue in result.data.value.items" :key="issue.id">
              <td>
                <RouterLink :to="`/issues/${issue.id}`" class="issue-title">
                  {{ issue.title }}
                </RouterLink>
                <span>{{ issue.culprit || issue.grouping.summary }}</span>
              </td>
              <td><StatusBadge :status="issue.status" /></td>
              <td>
                {{ issue.occurrence_count.toLocaleString(locale) }}
                <abbr
                  v-if="issue.occurrence_count_approximate"
                  :title="$t('common.approximateCount')"
                  >~</abbr
                >
              </td>
              <td>{{ formatTime(issue.last_seen) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </section>
</template>
