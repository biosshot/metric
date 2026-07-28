<script setup lang="ts">
import { computed, ref, watch } from 'vue';
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
import { timeWindow } from '../lib/timeRange';
import type { StructuredLog } from '../api/types';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
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
const range = ref('24h');
const appliedRange = ref('24h');
const selectedWindow = ref(timeWindow('24h'));
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
    ) || range.value !== '24h',
);
const levelOptions: SelectOption[] = [
  { value: '', label: 'All levels', icon: 'filter' },
  { value: 'trace', label: 'Trace', icon: 'status' },
  { value: 'debug', label: 'Debug', icon: 'code' },
  { value: 'info', label: 'Info', icon: 'info' },
  { value: 'warn', label: 'Warning', icon: 'alert' },
  { value: 'error', label: 'Error', icon: 'failure' },
  { value: 'fatal', label: 'Fatal', icon: 'blocked' },
];

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
      ...appliedWindow.value,
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
  return levelOptions
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
  range.value = '24h';
  selectedWindow.value = timeWindow('24h');
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
  return new Intl.DateTimeFormat(undefined, {
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
        <p class="eyebrow">{{ session.selectedProject?.slug }} / signals</p>
        <h1>Structured Logs</h1>
        <p>Search exact service context and follow logs into the Trace that produced them.</p>
      </div>
    </header>

    <form class="signal-toolbar" role="search" @submit.prevent="search">
      <label class="search-field">
        <span class="sr-only">Message contains</span>
        <input v-model="message" type="search" maxlength="512" placeholder="Message contains…" />
      </label>
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
      <BaseSelect v-model="level" :options="levelOptions" aria-label="Log level" />
      <div class="signal-toolbar__actions">
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          aria-label="Log time range"
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

    <LoadingPanel v-if="logs.isPending.value" label="Loading structured logs…" />
    <ApiErrorPanel v-else-if="logs.error.value" :error="logs.error.value" @retry="logs.refetch()" />
    <EmptyState
      v-else-if="!logs.data.value?.items.length"
      icon="logs"
      title="No logs in this view"
      description="Enable Sentry SDK Logs and send a log entry. Filters are exact except for message text."
    >
      <SdkSetupButton />
    </EmptyState>
    <div v-else class="signal-list">
      <div class="signal-histogram" aria-label="Log levels on this page">
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
        <span class="signal-level">{{ log.level }}</span>
        <RouterLink class="signal-title" :to="`/logs/${log.id}`">{{ log.message }}</RouterLink>
        <span>{{ log.service || 'unknown service' }}</span>
        <RouterLink v-if="traceLink(log)" class="text-link" :to="traceLink(log)!">
          Trace {{ log.trace_id?.slice(0, 8) }}
        </RouterLink>
        <time :datetime="log.timestamp">{{ formatTime(log.timestamp) }}</time>
      </article>
      <nav class="pagination" aria-label="Log result pages">
        <button
          class="button button--secondary"
          type="button"
          :disabled="history.length === 0"
          @click="previousPage"
        >
          Previous
        </button>
        <span>Page {{ history.length + 1 }}</span>
        <button
          class="button button--secondary"
          type="button"
          :disabled="!logs.data.value.next_cursor"
          @click="nextPage"
        >
          Next
        </button>
      </nav>
    </div>
  </section>
</template>
