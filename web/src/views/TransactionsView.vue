<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import { api } from '../api/client';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const service = ref('');
const environment = ref('');
const release = ref(typeof route.query.release === 'string' ? route.query.release : '');
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const projectId = computed(() => session.selectedProjectId ?? '');
const transactions = useQuery({
  queryKey: computed(() => [
    'transactions',
    projectId.value,
    service.value,
    environment.value,
    release.value,
    cursor.value,
  ]),
  queryFn: () =>
    api.transactions(projectId.value, {
      service: service.value.trim() || undefined,
      environment: environment.value.trim() || undefined,
      release: release.value.trim() || undefined,
      cursor: cursor.value,
    }),
  enabled: computed(() => Boolean(projectId.value)),
});

watch([projectId, service, environment, release], () => {
  cursor.value = null;
  history.value = [];
});

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
        <h1>Transactions</h1>
        <p>Root segments from Sentry transactions and streamed spans.</p>
      </div>
      <div class="compact-filter-group">
        <label class="compact-filter">
          <span>Service</span>
          <input v-model="service" maxlength="256" placeholder="All services" />
        </label>
        <label class="compact-filter">
          <span>Environment</span>
          <input v-model="environment" maxlength="128" placeholder="All environments" />
        </label>
        <label class="compact-filter">
          <span>Release</span>
          <input v-model="release" maxlength="256" placeholder="All releases" />
        </label>
      </div>
    </header>
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
