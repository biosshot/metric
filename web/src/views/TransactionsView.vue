<script setup lang="ts">
import { computed, ref, watch } from 'vue';
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
import { timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const service = ref('');
const environment = ref('');
const release = ref(typeof route.query.release === 'string' ? route.query.release : '');
const appliedService = ref('');
const appliedEnvironment = ref('');
const appliedRelease = ref(release.value);
const range = ref('24h');
const appliedRange = ref('24h');
const selectedWindow = ref(timeWindow('24h'));
const appliedWindow = ref({ ...selectedWindow.value });
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const projectId = computed(() => session.selectedProjectId ?? '');
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
      ...appliedWindow.value,
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
  range.value = '24h';
  selectedWindow.value = timeWindow('24h');
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
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / tracing</p>
        <h1>Traces</h1>
        <p>Root segments from Sentry transactions and streamed spans.</p>
      </div>
    </header>
    <TraceSectionNav />
    <form
      class="signal-toolbar signal-toolbar--compact"
      role="search"
      @submit.prevent="applyFilters"
    >
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
      <div class="signal-toolbar__actions">
        <TimeRangeSelect
          v-model="range"
          :window-value="selectedWindow"
          aria-label="Trace time range"
          @update:window-value="selectedWindow = $event"
        />
        <button class="button button--primary" type="submit">
          <AppIcon name="search" :size="16" />
          Search
        </button>
        <button class="button button--secondary" type="button" @click="resetFilters">
          <AppIcon name="close" :size="16" />
          Reset
        </button>
      </div>
    </form>
    <LoadingPanel v-if="transactions.isPending.value" label="Loading transactions…" />
    <ApiErrorPanel
      v-else-if="transactions.error.value"
      :error="transactions.error.value"
      @retry="transactions.refetch()"
    />
    <EmptyState
      v-else-if="!transactions.data.value?.items.length"
      icon="traces"
      title="No transactions yet"
      description="Set tracesSampleRate above zero in a supported SDK and finish a transaction."
    >
      <SdkSetupButton />
    </EmptyState>
    <div v-else class="transaction-list">
      <RouterLink
        v-for="transaction in transactions.data.value.items"
        :key="transaction.id"
        class="transaction-row"
        :to="`/traces/${transaction.trace_id}`"
      >
        <div>
          <strong>{{ transaction.name }}</strong>
          <span>{{ transaction.service || 'unknown service' }} · {{ transaction.operation }}</span>
        </div>
        <span v-if="transaction.insight_flags" class="insight-pill">
          {{ transaction.insight_flags.toString(2).replaceAll('0', '').length }} insights
        </span>
        <span :class="{ 'duration--slow': transaction.duration_ms >= 1000 }">
          {{ transaction.duration_ms.toFixed(1) }} ms
        </span>
        <time :datetime="transaction.started_at">{{ transaction.started_at }}</time>
      </RouterLink>
      <nav class="pagination" aria-label="Transaction pages">
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
          :disabled="!transactions.data.value.next_cursor"
          @click="nextPage"
        >
          Next
        </button>
      </nav>
    </div>
  </section>
</template>
