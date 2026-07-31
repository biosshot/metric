<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import { api } from '../api/client';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import type { StructuredLog } from '../api/types';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const { locale, t } = useI18n();
const level = ref('');
const appliedLevel = ref('');
const message = ref('');
const submittedMessage = ref('');
const service = ref('');
const appliedService = ref('');
const environment = ref('');
const appliedEnvironment = ref('');
const release = ref(typeof route.query.release === 'string' ? route.query.release : '');
const appliedRelease = ref(release.value);
const range = ref('all');
const appliedRange = ref('all');
const selectedWindow = ref(timeWindow('all'));
const appliedWindow = ref({ ...selectedWindow.value });
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const projectId = computed(() => session.selectedProjectId ?? '');
const hasFilters = computed(
  () =>
    Boolean(
      level.value ||
        message.value.trim() ||
        service.value.trim() ||
        environment.value.trim() ||
        release.value.trim(),
    ) || range.value !== 'all',
);
const levelOptions = computed<SelectOption[]>(() => [
  { value: '', label: t('logs.allLevels'), icon: 'filter' },
  { value: 'trace', label: t('status.trace'), icon: 'status' },
  { value: 'debug', label: t('status.debug'), icon: 'code' },
  { value: 'info', label: t('status.info'), icon: 'info' },
  { value: 'warn', label: t('status.warn'), icon: 'alert' },
  { value: 'error', label: t('status.error'), icon: 'failure' },
  { value: 'fatal', label: t('status.fatal'), icon: 'blocked' },
]);

const logs = useQuery({
  queryKey: computed(() => [
    'logs',
    projectId.value,
    appliedLevel.value,
    submittedMessage.value,
    appliedService.value,
    appliedEnvironment.value,
    appliedRelease.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
    cursor.value,
  ]),
  queryFn: () =>
    api.logs(projectId.value, {
      ...optionalTimeWindow(appliedRange.value, appliedWindow.value),
      level: appliedLevel.value || undefined,
      message: submittedMessage.value || undefined,
      service: appliedService.value || undefined,
      environment: appliedEnvironment.value || undefined,
      release: appliedRelease.value || undefined,
      cursor: cursor.value,
    }),
  enabled: computed(() => Boolean(projectId.value)),
});

watch(projectId, resetPage);

const levelCounts = computed(() => {
  const counts = new Map<string, number>();
  for (const log of logs.data.value?.items ?? []) {
    counts.set(log.level, (counts.get(log.level) ?? 0) + 1);
  }
  return levelOptions.value
    .filter((option) => option.value)
    .map((option) => ({
      level: option.value,
      label: option.label,
      count: counts.get(option.value) ?? 0,
    }));
});
const maximumLevelCount = computed(() =>
  Math.max(1, ...levelCounts.value.map((entry) => entry.count)),
);

function search(): void {
  submittedMessage.value = message.value.trim();
  appliedLevel.value = level.value;
  appliedService.value = service.value.trim();
  appliedEnvironment.value = environment.value.trim();
  appliedRelease.value = release.value.trim();
  appliedRange.value = range.value;
  appliedWindow.value = { ...selectedWindow.value };
  resetPage();
}

function resetFilters(): void {
  level.value = '';
  message.value = '';
  service.value = '';
  environment.value = '';
  release.value = '';
  range.value = 'all';
  selectedWindow.value = timeWindow('all');
  search();
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

function formatTime(value: string): string {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'medium',
  }).format(new Date(value));
}

function traceLink(log: StructuredLog): string | null {
  return log.trace_id ? `/traces/${log.trace_id}` : null;
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

    <form class="signal-toolbar" role="search" @submit.prevent="search">
      <label class="search-field">
        <span class="sr-only">{{ $t('logs.messageContains') }}</span>
        <input
          v-model="message"
          type="search"
          maxlength="512"
          :placeholder="$t('logs.messagePlaceholder')"
        />
      </label>
      <label>
        <span class="sr-only">{{ $t('logs.service') }}</span>
        <input v-model="service" maxlength="256" :placeholder="$t('logs.service')" />
      </label>
      <label>
        <span class="sr-only">{{ $t('logs.environment') }}</span>
        <input v-model="environment" maxlength="128" :placeholder="$t('logs.environment')" />
      </label>
      <label>
        <span class="sr-only">{{ $t('logs.release') }}</span>
        <input v-model="release" maxlength="256" :placeholder="$t('logs.release')" />
      </label>
      <BaseSelect v-model="level" :options="levelOptions" :aria-label="$t('logs.level')" />
      <div class="signal-toolbar__actions">
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          :aria-label="$t('logs.timeRange')"
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

    <LoadingPanel v-if="logs.isPending.value" :label="$t('logs.loading')" />
    <ApiErrorPanel v-else-if="logs.error.value" :error="logs.error.value" @retry="logs.refetch()" />
    <EmptyState
      v-else-if="!logs.data.value?.items.length"
      icon="logs"
      :title="$t('logs.empty')"
      :description="$t('logs.emptyDescription')"
    >
      <SdkSetupButton />
    </EmptyState>
    <div v-else class="signal-list">
      <nav class="pagination" :aria-label="$t('logs.resultPages')">
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
          <small>{{ entry.label }} {{ entry.count }}</small>
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
        <RouterLink v-if="traceLink(log)" class="text-link" :to="traceLink(log)!">
          {{ $t('logs.trace', { id: log.trace_id?.slice(0, 8) }) }}
        </RouterLink>
        <time :datetime="log.timestamp">{{ formatTime(log.timestamp) }}</time>
      </article>
    </div>
  </section>
</template>
