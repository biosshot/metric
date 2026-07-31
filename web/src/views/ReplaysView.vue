<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import { api } from '../api/client';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const { locale } = useI18n();
const PAGE_SIZE = 10;
const projectId = computed(() => session.selectedProjectId ?? '');
const search = ref('');
const submittedSearch = ref('');
const page = ref(1);
const range = ref('all');
const appliedRange = ref('all');
const selectedWindow = ref(timeWindow('all'));
const appliedWindow = ref({ ...selectedWindow.value });
const hasFilters = computed(
  () => Boolean(search.value.trim() || submittedSearch.value) || range.value !== 'all',
);
const replays = useQuery({
  queryKey: computed(() => [
    'replays',
    projectId.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
  ]),
  queryFn: () =>
    api.replays(projectId.value, optionalTimeWindow(appliedRange.value, appliedWindow.value)),
  enabled: computed(() => Boolean(projectId.value)),
});
const visibleReplays = computed(() => {
  const term = submittedSearch.value.toLowerCase();
  const items = replays.data.value?.items ?? [];
  if (!term) return items;
  return items.filter((replay) =>
    [replay.id, replay.url, replay.environment, replay.release]
      .filter((value): value is string => Boolean(value))
      .some((value) => value.toLowerCase().includes(term)),
  );
});
const pageCount = computed(() => Math.max(1, Math.ceil(visibleReplays.value.length / PAGE_SIZE)));
const paginatedReplays = computed(() => {
  const start = (page.value - 1) * PAGE_SIZE;
  return visibleReplays.value.slice(start, start + PAGE_SIZE);
});

watch(pageCount, (count) => {
  if (page.value > count) page.value = count;
});

function submitSearch(): void {
  page.value = 1;
  submittedSearch.value = search.value.trim();
  appliedRange.value = range.value;
  appliedWindow.value = { ...selectedWindow.value };
}

function clearSearch(): void {
  page.value = 1;
  search.value = '';
  submittedSearch.value = '';
  range.value = 'all';
  appliedRange.value = 'all';
  selectedWindow.value = timeWindow('all');
  appliedWindow.value = { ...selectedWindow.value };
}

function duration(milliseconds: number): string {
  if (milliseconds < 1000) return `${milliseconds} ms`;
  return `${new Intl.NumberFormat(locale.value, { maximumFractionDigits: 1 }).format(
    milliseconds / 1000,
  )} s`;
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
    <form
      class="signal-toolbar signal-toolbar--replays"
      role="search"
      @submit.prevent="submitSearch"
    >
      <label class="search-field">
        <span>{{ $t('replays.searchLabel') }}</span>
        <input
          v-model="search"
          type="search"
          maxlength="2048"
          :placeholder="$t('replays.searchPlaceholder')"
        />
        <small>{{ $t('replays.searchHelp') }}</small>
      </label>
      <div class="signal-toolbar__actions">
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          :aria-label="$t('replays.timeRange')"
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
          @click="clearSearch"
        >
          <AppIcon name="close" :size="16" />
          {{ $t('common.reset') }}
        </button>
      </div>
    </form>
    <div v-if="submittedSearch" class="search-context">
      {{ $t('replays.matches', visibleReplays.length) }} {{ $t('replays.for') }}
      <code>{{ submittedSearch }}</code>
    </div>
    <LoadingPanel v-if="replays.isPending.value" :label="$t('replays.loading')" />
    <ApiErrorPanel
      v-else-if="replays.error.value"
      :error="replays.error.value"
      @retry="replays.refetch()"
    />
    <EmptyState
      v-else-if="!replays.data.value?.items.length"
      icon="replay"
      :title="$t('replays.empty')"
      :description="$t('replays.emptyDescription')"
    >
      <SdkSetupButton />
    </EmptyState>
    <EmptyState
      v-else-if="!visibleReplays.length"
      icon="search"
      :title="$t('replays.noMatches')"
      :description="$t('replays.noMatchesDescription')"
    >
      <button class="button button--secondary" type="button" @click="clearSearch">
        <AppIcon name="close" :size="16" />
        {{ $t('replays.reset') }}
      </button>
    </EmptyState>
    <div v-else class="transaction-list">
      <nav class="pagination" :aria-label="$t('replays.pages')">
        <button
          class="button button--secondary"
          type="button"
          :disabled="page === 1"
          @click="page -= 1"
        >
          {{ $t('common.previous') }}
        </button>
        <span>{{ $t('replays.page', { page, count: pageCount }) }}</span>
        <button
          class="button button--secondary"
          type="button"
          :disabled="page === pageCount"
          @click="page += 1"
        >
          {{ $t('common.next') }}
        </button>
      </nav>
      <RouterLink
        v-for="replay in paginatedReplays"
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
