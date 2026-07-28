<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref } from 'vue';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import Player from 'rrweb-player';
import 'rrweb-player/dist/style.css';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import { api, ApiError } from '../api/client';
import { useSessionStore } from '../stores/session';
import { prepareReplayEvents } from './replayEvents';

const MAX_PLAYER_SEGMENTS = 100;
const MAX_PLAYER_EVENTS = 100_000;
const MAX_PLAYER_BYTES = 50 * 1024 * 1024;

const session = useSessionStore();
const route = useRoute();
const projectId = computed(() => session.selectedProjectId ?? '');
const replayId = computed(() => String(route.params.replayId ?? ''));
const target = ref<HTMLElement | null>(null);
const playerError = ref<unknown>(null);
const loadingPlayer = ref(false);
let player: Player | null = null;

const replay = useQuery({
  queryKey: computed(() => ['replay', projectId.value, replayId.value]),
  queryFn: () => api.replay(projectId.value, replayId.value),
  enabled: computed(() => Boolean(projectId.value && replayId.value)),
});

async function inflate(payload: Uint8Array): Promise<string> {
  if (payload[0] === 0x5b) return new TextDecoder().decode(payload);
  if (typeof DecompressionStream === 'undefined') {
    throw new Error('This browser cannot safely decompress Replay segments.');
  }
  const stream = new Blob([payload]).stream().pipeThrough(new DecompressionStream('deflate'));
  const bytes = await new Response(stream).arrayBuffer();
  if (bytes.byteLength > MAX_PLAYER_BYTES) throw new Error('Replay exceeds the player byte limit.');
  return new TextDecoder().decode(bytes);
}

async function loadPlayer(): Promise<void> {
  const record = replay.data.value;
  if (!record) return;
  playerError.value = null;
  if (record.segments.length > MAX_PLAYER_SEGMENTS) {
    playerError.value = new Error(
      `This Replay has ${record.segments.length} segments; the bounded player allows ${MAX_PLAYER_SEGMENTS}.`,
    );
    return;
  }
  const expectedEvents = record.segments.reduce((sum, segment) => sum + segment.event_count, 0);
  const expectedBytes = record.segments.reduce(
    (sum, segment) => sum + segment.decompressed_bytes,
    0,
  );
  if (expectedEvents > MAX_PLAYER_EVENTS || expectedBytes > MAX_PLAYER_BYTES) {
    playerError.value = new Error('This Replay exceeds the bounded player limits.');
    return;
  }
  loadingPlayer.value = true;
  try {
    const events: unknown[] = [];
    for (const segment of record.segments) {
      const raw = new Uint8Array(
        await api.replaySegment(projectId.value, record.id, segment.segment_id),
      );
      const separator = raw.indexOf(0x0a);
      if (separator <= 0) throw new Error('Replay segment header is malformed.');
      const decoded = JSON.parse(await inflate(raw.subarray(separator + 1))) as unknown;
      if (!Array.isArray(decoded)) throw new Error('Replay segment is not an event array.');
      events.push(...decoded);
      if (events.length > MAX_PLAYER_EVENTS) throw new Error('Replay exceeds the event limit.');
    }
    if (events.length < 2) throw new Error('Replay does not contain enough events to play.');
    const playerEvents = prepareReplayEvents(events);
    await nextTick();
    if (!target.value) return;
    target.value.replaceChildren();
    player?.$destroy();
    player = new Player({
      target: target.value,
      props: {
        events: playerEvents as never[],
        width: Math.min(1100, target.value.clientWidth || 1100),
        height: 620,
        autoPlay: false,
        showController: true,
        skipInactive: true,
      },
    });
  } catch (error) {
    playerError.value =
      error instanceof ApiError
        ? error
        : new ApiError(
            0,
            'replay_player_error',
            null,
            error instanceof Error
              ? error.message
              : `Replay could not be decoded: ${String(error)}`,
          );
  } finally {
    loadingPlayer.value = false;
  }
}

onBeforeUnmount(() => player?.$destroy());
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <RouterLink class="back-link" to="/replays">← Session Replays</RouterLink>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / replay</p>
        <h1>Session Replay</h1>
        <p class="mono">{{ replayId }}</p>
      </div>
    </header>
    <LoadingPanel v-if="replay.isPending.value" label="Loading Replay metadata…" />
    <ApiErrorPanel
      v-else-if="replay.error.value"
      :error="replay.error.value"
      @retry="replay.refetch()"
    />
    <template v-else-if="replay.data.value">
      <div class="privacy-notice privacy-notice--warning">
        <AppIcon name="shield" :size="18" />
        Playback renders untrusted historical DOM inside rrweb’s sandbox. Client-side masking must
        be configured before capture; the server cannot retroactively scrub opaque recordings.
      </div>
      <div class="metric-grid replay-metadata-grid">
        <article>
          <span>Status</span>
          <strong>{{ replay.data.value.partial ? 'Partial' : 'Complete' }}</strong>
        </article>
        <article>
          <span>Segments</span>
          <strong>{{ replay.data.value.segments.length }}</strong>
        </article>
        <article>
          <span>Environment</span>
          <strong>{{ replay.data.value.environment || '—' }}</strong>
        </article>
        <article>
          <span>Release</span>
          <strong>{{ replay.data.value.release || '—' }}</strong>
        </article>
      </div>
      <ApiErrorPanel v-if="playerError" :error="playerError" title="Replay playback failed" />
      <div class="replay-player-shell">
        <div v-if="!loadingPlayer && !player" class="replay-player-placeholder">
          <AppIcon name="replay" :size="28" />
          <strong>Recording is not downloaded automatically</strong>
          <span>Use “Load recording” to fetch the bounded segment set.</span>
          <button
            class="button button--primary"
            type="button"
            :disabled="loadingPlayer"
            @click="loadPlayer"
          >
            <AppIcon name="replay" :size="17" />
            Load recording
          </button>
        </div>
        <LoadingPanel v-if="loadingPlayer" label="Downloading and validating Replay…" />
        <div ref="target" class="replay-player"></div>
      </div>
      <section class="detail-panel">
        <h2>Correlations</h2>
        <p v-if="!replay.data.value.error_ids.length && !replay.data.value.trace_ids.length">
          This Replay is not linked to an Error or Trace.
        </p>
        <div class="correlation-links">
          <RouterLink :to="`/feedback?replay_id=${replay.data.value.id}`">
            Feedback linked to this Replay
          </RouterLink>
          <RouterLink
            v-for="eventId in replay.data.value.error_ids"
            :key="eventId"
            :to="`/events/${eventId}`"
          >
            Error {{ eventId }}
          </RouterLink>
          <RouterLink
            v-for="traceId in replay.data.value.trace_ids"
            :key="traceId"
            :to="`/traces/${traceId}`"
          >
            Trace and linked Logs {{ traceId }}
          </RouterLink>
        </div>
      </section>
    </template>
  </section>
</template>
