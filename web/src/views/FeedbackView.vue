<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import StatusBadge from '../components/StatusBadge.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import UnifiedQueryBar from '../components/UnifiedQueryBar.vue';
import { api } from '../api/client';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';
import type { Feedback } from '../api/types';

const session = useSessionStore();
const route = useRoute();
const { locale } = useI18n();
const routeQuery = typeof route.query.q === 'string' ? route.query.q : '';
const query = ref(routeQuery);
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

const feedback = useQuery({
  queryKey: computed(() => [
    'unified-query',
    'feedback',
    projectId.value,
    appliedQuery.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
    cursor.value,
  ]),
  queryFn: () =>
    api.query<Feedback>(projectId.value, {
      source: 'feedback',
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
  const next = feedback.data.value?.next_cursor;
  if (!next) return;
  history.value.push(cursor.value);
  cursor.value = next;
}

function previousPage(): void {
  cursor.value = history.value.pop() ?? null;
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
          {{ $t('feedback.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('feedback.title') }}</h1>
        <p>{{ $t('feedback.description') }}</p>
      </div>
    </header>

    <UnifiedQueryBar
      v-model="query"
      source="feedback"
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
          aria-label="Feedback time range"
          @update:window-value="selectedWindow = $event"
        />
      </template>
    </UnifiedQueryBar>

    <LoadingPanel v-if="feedback.isPending.value" :label="$t('feedback.loading')" />
    <ApiErrorPanel
      v-else-if="feedback.error.value"
      :error="feedback.error.value"
      @retry="feedback.refetch()"
    />
    <EmptyState
      v-else-if="!feedback.data.value?.items.length"
      icon="message"
      :title="$t('feedback.empty')"
      :description="$t('feedback.emptyDescription')"
    >
      <SdkSetupButton v-if="!appliedQuery" />
    </EmptyState>
    <div v-else class="issue-table-wrap">
      <nav class="pagination" :aria-label="$t('feedback.pages')">
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
          :disabled="!feedback.data.value.next_cursor"
          @click="nextPage"
        >
          {{ $t('common.next') }}
        </button>
      </nav>
      <div class="issue-table-scroll">
        <table class="issue-table feedback-table">
          <thead>
            <tr>
              <th scope="col">{{ $t('feedback.feedback') }}</th>
              <th scope="col">{{ $t('feedback.status') }}</th>
              <th scope="col">{{ $t('feedback.reporter') }}</th>
              <th scope="col">{{ $t('feedback.attachments') }}</th>
              <th scope="col">{{ $t('feedback.received') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in feedback.data.value.items" :key="item.id">
              <td>
                <RouterLink :to="`/feedback/${item.id}`" class="issue-title">
                  {{ item.message }}
                </RouterLink>
                <span>{{ item.url || $t('feedback.noUrl') }}</span>
              </td>
              <td><StatusBadge :status="item.status" /></td>
              <td>
                <span class="feedback-reporter">
                  <strong>{{ item.name || $t('feedback.anonymous') }}</strong>
                  <small v-if="item.contact_email">{{ item.contact_email }}</small>
                </span>
              </td>
              <td>{{ item.attachments.length.toLocaleString(locale) }}</td>
              <td>
                <time :datetime="item.received_at">{{ formatTime(item.received_at) }}</time>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </section>
</template>
