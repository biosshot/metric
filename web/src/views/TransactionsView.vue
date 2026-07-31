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
import TraceSectionNav from '../components/TraceSectionNav.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import { api } from '../api/client';
import { optionalTimeWindow, timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const { locale } = useI18n();
const service = ref('');
const environment = ref('');
const release = ref(typeof route.query.release === 'string' ? route.query.release : '');
const appliedService = ref('');
const appliedEnvironment = ref('');
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
    Boolean(service.value.trim() || environment.value.trim() || release.value.trim()) ||
    range.value !== 'all',
);
const transactions = useQuery({
  queryKey: computed(() => [
    'transactions',
    projectId.value,
    appliedService.value,
    appliedEnvironment.value,
    appliedRelease.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
    cursor.value,
  ]),
  queryFn: () =>
    api.transactions(projectId.value, {
      ...optionalTimeWindow(appliedRange.value, appliedWindow.value),
      service: appliedService.value || undefined,
      environment: appliedEnvironment.value || undefined,
      release: appliedRelease.value || undefined,
      cursor: cursor.value,
    }),
  enabled: computed(() => Boolean(projectId.value)),
});

watch(projectId, () => {
  cursor.value = null;
  history.value = [];
});

function applyFilters(): void {
  appliedService.value = service.value.trim();
  appliedEnvironment.value = environment.value.trim();
  appliedRelease.value = release.value.trim();
  appliedRange.value = range.value;
  appliedWindow.value = { ...selectedWindow.value };
  cursor.value = null;
  history.value = [];
}

function resetFilters(): void {
  service.value = '';
  environment.value = '';
  release.value = '';
  range.value = 'all';
  selectedWindow.value = timeWindow('all');
  applyFilters();
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
          {{ $t('transactions.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('transactions.title') }}</h1>
        <p>{{ $t('transactions.description') }}</p>
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
          :aria-label="$t('transactions.timeRange')"
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
      <SdkSetupButton />
    </EmptyState>
    <div v-else class="transaction-list">
      <nav class="pagination" :aria-label="$t('transactions.pages')">
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
          <span
            >{{ transaction.service || $t('transactions.unknownService') }} ·
            {{ transaction.operation }}</span
          >
        </div>
        <span v-if="transaction.insight_flags" class="insight-pill">
          {{
            $t(
              'transactions.insights',
              transaction.insight_flags.toString(2).replaceAll('0', '').length,
            )
          }}
        </span>
        <span :class="{ 'duration--slow': transaction.duration_ms >= 1000 }">
          {{ transaction.duration_ms.toFixed(1) }} ms
        </span>
        <time :datetime="transaction.started_at">{{ formatTime(transaction.started_at) }}</time>
      </RouterLink>
    </div>
  </section>
</template>
