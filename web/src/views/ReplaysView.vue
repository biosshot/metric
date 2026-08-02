<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import UnifiedQueryBar from '../components/UnifiedQueryBar.vue';
import { api } from '../api/client';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';
import type { Replay } from '../api/types';

const session = useSessionStore();
const route = useRoute();
const { locale } = useI18n();
const initialQuery = typeof route.query.q === 'string' ? route.query.q : '';
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
  () => Boolean(query.value.trim() || appliedQuery.value) || range.value !== 'all',
);

const replays = useQuery({
  queryKey: computed(() => [
    'unified-query',
    'replays',
    projectId.value,
    appliedQuery.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
    cursor.value,
  ]),
  queryFn: () =>
    api.query<Replay>(projectId.value, {
      source: 'replays',
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
  const next = replays.data.value?.next_cursor;
  if (!next) return;
  history.value.push(cursor.value);
  cursor.value = next;
}

function previousPage(): void {
  cursor.value = history.value.pop() ?? null;
}

function duration(milliseconds: number): string {
  if (milliseconds < 1000) return `${milliseconds} ms`;
  return `${new Intl.NumberFormat(locale.value, { maximumFractionDigits: 1 }).format(milliseconds / 1000)} s`;
}

function formatTime(value: string): string {
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
          {{ $t('replays.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('replays.title') }}</h1>
        <p>{{ $t('replays.description') }}</p>
      </div>
    </header>
    <div class="privacy-notice">
      <AppIcon name="shield" :size="18" />
      <span>{{ $t('replays.privacy') }}</span>
    </div>
    <UnifiedQueryBar
      v-model="query"
      source="replays"
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
          :aria-label="$t('replays.timeRange')"
          @update:window-value="selectedWindow = $event"
        />
      </template>
    </UnifiedQueryBar>
    <LoadingPanel v-if="replays.isPending.value" :label="$t('replays.loading')" />
    <ApiErrorPanel
      v-else-if="replays.error.value"
      :error="replays.error.value"
      @retry="replays.refetch()"
    />
    <EmptyState
      v-else-if="!replays.data.value?.items.length"
      icon="replay"
      :title="appliedQuery ? $t('replays.noMatches') : $t('replays.empty')"
      :description="
        appliedQuery ? $t('replays.noMatchesDescription') : $t('replays.emptyDescription')
      "
    >
      <SdkSetupButton v-if="!appliedQuery" />
    </EmptyState>
    <div v-else class="transaction-list">
      <nav class="pagination" :aria-label="$t('replays.pages')">
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
          :disabled="!replays.data.value.next_cursor"
          @click="nextPage"
        >
          {{ $t('common.next') }}
        </button>
      </nav>
      <RouterLink
        v-for="replay in replays.data.value.items"
        :key="replay.id"
        class="transaction-row replay-row"
        :to="`/replays/${replay.id}`"
      >
        <div>
          <strong>{{ replay.url || $t('replays.browserSession') }}</strong>
          <span>
            {{ replay.environment || $t('replays.defaultEnvironment') }} ·
            {{ replay.release || $t('replays.unknownRelease') }}
          </span>
        </div>
        <span v-if="replay.partial" class="status-pill status-pill--warning">{{
          $t('replays.partial')
        }}</span>
        <span>{{ $t('replays.segments', replay.segments.length) }}</span>
        <span>{{ duration(replay.duration_ms) }}</span>
        <time :datetime="replay.received_at">{{ formatTime(replay.received_at) }}</time>
      </RouterLink>
    </div>
  </section>
</template>
