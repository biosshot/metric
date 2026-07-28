<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import TimeRangeSelect from '../components/TimeRangeSelect.vue';
import { api } from '../api/client';
import { timeWindow } from '../lib/timeRange';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const projectId = computed(() => session.selectedProjectId ?? '');
const search = ref('');
const submittedSearch = ref('');
const range = ref('24h');
const appliedRange = ref('24h');
const appliedWindow = ref(timeWindow('24h'));
const replays = useQuery({
  queryKey: computed(() => [
    'replays',
    projectId.value,
    appliedRange.value,
    appliedWindow.value.from,
    appliedWindow.value.until,
  ]),
  queryFn: () => api.replays(projectId.value, appliedWindow.value),
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

function submitSearch(): void {
  submittedSearch.value = search.value.trim();
  appliedRange.value = range.value;
  appliedWindow.value = timeWindow(range.value);
}

function clearSearch(): void {
  search.value = '';
  submittedSearch.value = '';
}

function duration(milliseconds: number): string {
  if (milliseconds < 1000) return `${milliseconds} ms`;
  return `${(milliseconds / 1000).toFixed(1)} s`;
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / diagnostics</p>
        <h1>Session Replays</h1>
        <p>Masked browser recordings linked to Errors and Traces.</p>
      </div>
    </header>
    <div class="privacy-notice">
      <AppIcon name="shield" :size="18" />
      <span>
        Replay bytes are captured and masked by the pinned browser SDK. Metric stores them as
        opaque, untrusted recordings.
      </span>
    </div>
    <form
      class="signal-toolbar signal-toolbar--replays"
      role="search"
      @submit.prevent="submitSearch"
    >
      <label class="search-field">
        <span>Search loaded Replays</span>
        <input
          v-model="search"
          type="search"
          maxlength="2048"
          placeholder="Replay ID, URL, environment, or release"
        />
        <small>Searches the latest 50 Replay manifests loaded for this project.</small>
      </label>
      <TimeRangeSelect v-model="range" aria-label="Replay time range" />
      <div class="signal-toolbar__actions">
        <button class="button button--primary" type="submit">
          <AppIcon name="search" :size="16" />
          Search
        </button>
        <button
          v-if="submittedSearch"
          class="button button--secondary"
          type="button"
          @click="clearSearch"
        >
          <AppIcon name="close" :size="16" />
          Reset
        </button>
      </div>
    </form>
    <div v-if="submittedSearch" class="search-context">
      {{ visibleReplays.length }} matching Replay{{ visibleReplays.length === 1 ? '' : 's' }} for
      <code>{{ submittedSearch }}</code>
    </div>
    <LoadingPanel v-if="replays.isPending.value" label="Loading Session Replays…" />
    <ApiErrorPanel
      v-else-if="replays.error.value"
      :error="replays.error.value"
      @retry="replays.refetch()"
    />
    <EmptyState
      v-else-if="!replays.data.value?.items.length"
      icon="replay"
      title="No Session Replays yet"
      description="Enable Replay for this project and configure the pinned browser SDK."
    >
      <SdkSetupButton />
    </EmptyState>
    <EmptyState
      v-else-if="!visibleReplays.length"
      icon="search"
      title="No matching Replays"
      description="Try a Replay ID, URL, environment, or release from the loaded set."
    >
      <button class="button button--secondary" type="button" @click="clearSearch">
        <AppIcon name="close" :size="16" />
        Reset search
      </button>
    </EmptyState>
    <div v-else class="transaction-list">
      <RouterLink
        v-for="replay in visibleReplays"
        :key="replay.id"
        class="transaction-row replay-row"
        :to="`/replays/${replay.id}`"
      >
        <div>
          <strong>{{ replay.url || 'Browser session' }}</strong>
          <span>
            {{ replay.environment || 'default environment' }} ·
            {{ replay.release || 'unknown release' }}
          </span>
        </div>
        <span v-if="replay.partial" class="status-pill status-pill--warning">Partial</span>
        <span
          >{{ replay.segments.length }} segment{{ replay.segments.length === 1 ? '' : 's' }}</span
        >
        <span>{{ duration(replay.duration_ms) }}</span>
        <time :datetime="replay.received_at">{{ replay.received_at }}</time>
      </RouterLink>
    </div>
  </section>
</template>
