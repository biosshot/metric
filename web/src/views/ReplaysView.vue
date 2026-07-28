<script setup lang="ts">
import { computed } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import { api } from '../api/client';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const projectId = computed(() => session.selectedProjectId ?? '');
const replays = useQuery({
  queryKey: computed(() => ['replays', projectId.value]),
  queryFn: () => api.replays(projectId.value),
  enabled: computed(() => Boolean(projectId.value)),
});

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
    <div v-else class="transaction-list">
      <RouterLink
        v-for="replay in replays.data.value.items"
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
