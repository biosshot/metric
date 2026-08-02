<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import Player from 'rrweb-player';
import 'rrweb-player/dist/style.css';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import RelatedSignals, { type RelatedSignalLink } from '../components/RelatedSignals.vue';
import { api, ApiError } from '../api/client';
import { queryLink } from '../lib/queryLinks';
import { useSessionStore } from '../stores/session';
import { prepareReplayEvents } from './replayEvents';

const MAX_PLAYER_SEGMENTS = 100;
const MAX_PLAYER_EVENTS = 100_000;
const MAX_PLAYER_BYTES = 50 * 1024 * 1024;

const session = useSessionStore();
const route = useRoute();
const { t } = useI18n();
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
const relatedLinks = computed<RelatedSignalLink[]>(() => {
  const value = replay.data.value;
  if (!value) return [];
  const links: RelatedSignalLink[] = [];
  if (value.release) {
    links.push({
      key: 'release',
      icon: 'release',
      label: t('relations.viewRelease'),
      description: value.release,
      to: queryLink('/releases', 'rel', value.release),
    });
  }
  if (value.environment) {
    links.push(
      {
        key: 'environment-errors',
        icon: 'bug',
        label: t('relations.environmentErrors'),
        description: value.environment,
        to: queryLink('/explore', 'env', value.environment),
      },
      {
        key: 'environment-logs',
        icon: 'logs',
        label: t('relations.environmentLogs'),
        description: value.environment,
        to: queryLink('/logs', 'env', value.environment),
      },
      {
        key: 'environment-traces',
        icon: 'traces',
        label: t('relations.environmentTraces'),
        description: value.environment,
        to: queryLink('/traces', 'env', value.environment),
      },
    );
  }
  return links;
});

async function inflate(payload: Uint8Array): Promise<string> {
  if (payload[0] === 0x5b) return new TextDecoder().decode(payload);
  if (typeof DecompressionStream === 'undefined') {
    throw new Error(t('replayDetail.decompressUnsupported'));
  }
  const stream = new Blob([payload]).stream().pipeThrough(new DecompressionStream('deflate'));
  const bytes = await new Response(stream).arrayBuffer();
  if (bytes.byteLength > MAX_PLAYER_BYTES) throw new Error(t('replayDetail.byteLimit'));
  return new TextDecoder().decode(bytes);
}

async function loadPlayer(): Promise<void> {
  const record = replay.data.value;
  if (!record) return;
  playerError.value = null;
  if (record.segments.length > MAX_PLAYER_SEGMENTS) {
    playerError.value = new Error(
      t('replayDetail.tooManySegments', {
        count: record.segments.length,
        limit: MAX_PLAYER_SEGMENTS,
      }),
    );
    return;
  }
  const expectedEvents = record.segments.reduce((sum, segment) => sum + segment.event_count, 0);
  const expectedBytes = record.segments.reduce(
    (sum, segment) => sum + segment.decompressed_bytes,
    0,
  );
  if (expectedEvents > MAX_PLAYER_EVENTS || expectedBytes > MAX_PLAYER_BYTES) {
    playerError.value = new Error(t('replayDetail.playerLimits'));
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
      if (separator <= 0) throw new Error(t('replayDetail.malformedHeader'));
      const decoded = JSON.parse(await inflate(raw.subarray(separator + 1))) as unknown;
      if (!Array.isArray(decoded)) throw new Error(t('replayDetail.invalidEvents'));
      events.push(...decoded);
      if (events.length > MAX_PLAYER_EVENTS) throw new Error(t('replayDetail.eventLimit'));
    }
    if (events.length < 2) throw new Error(t('replayDetail.insufficientEvents'));
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
              : t('replayDetail.decodeFailed', { error: String(error) }),
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
        <RouterLink class="back-link" to="/replays">
          <AppIcon name="back" :size="16" /> {{ $t('replayDetail.all') }}
        </RouterLink>
        <p class="eyebrow">
          {{ $t('replayDetail.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('replayDetail.title') }}</h1>
        <p class="mono">{{ replayId }}</p>
      </div>
    </header>
    <LoadingPanel v-if="replay.isPending.value" :label="$t('replayDetail.loading')" />
    <ApiErrorPanel
      v-else-if="replay.error.value"
      :error="replay.error.value"
      @retry="replay.refetch()"
    />
    <template v-else-if="replay.data.value">
      <div class="privacy-notice privacy-notice--warning">
        <AppIcon name="shield" :size="18" />
        {{ $t('replayDetail.privacy') }}
      </div>
      <div class="metric-grid replay-metadata-grid">
        <article>
          <span>{{ $t('replayDetail.status') }}</span>
          <strong>{{
            replay.data.value.partial ? $t('replayDetail.partial') : $t('replayDetail.complete')
          }}</strong>
        </article>
        <article>
          <span>{{ $t('replayDetail.segments') }}</span>
          <strong>{{ replay.data.value.segments.length }}</strong>
        </article>
        <article>
          <span>{{ $t('replayDetail.environment') }}</span>
          <strong>{{ replay.data.value.environment || '—' }}</strong>
        </article>
        <article>
          <span>{{ $t('replayDetail.release') }}</span>
          <strong>{{ replay.data.value.release || '—' }}</strong>
        </article>
      </div>
      <RelatedSignals :links="relatedLinks" />
      <ApiErrorPanel
        v-if="playerError"
        :error="playerError"
        :title="$t('replayDetail.playbackFailed')"
      />
      <div class="replay-player-shell">
        <div v-if="!loadingPlayer && !player" class="replay-player-placeholder">
          <AppIcon name="replay" :size="28" />
          <strong>{{ $t('replayDetail.notDownloaded') }}</strong>
          <span>{{ $t('replayDetail.loadHelp') }}</span>
          <button
            class="button button--primary"
            type="button"
            :disabled="loadingPlayer"
            @click="loadPlayer"
          >
            <AppIcon name="replay" :size="17" />
            {{ $t('replayDetail.load') }}
          </button>
        </div>
        <LoadingPanel v-if="loadingPlayer" :label="$t('replayDetail.downloading')" />
        <div ref="target" class="replay-player"></div>
      </div>
      <section class="detail-panel">
        <h2>{{ $t('replayDetail.correlations') }}</h2>
        <p v-if="!replay.data.value.error_ids.length && !replay.data.value.trace_ids.length">
          {{ $t('replayDetail.noCorrelations') }}
        </p>
        <div class="correlation-links">
          <RouterLink :to="queryLink('/feedback', 'replay', replay.data.value.id)">
            {{ $t('replayDetail.linkedFeedback') }}
          </RouterLink>
          <RouterLink
            v-for="eventId in replay.data.value.error_ids"
            :key="eventId"
            :to="`/events/${eventId}`"
          >
            {{ $t('replayDetail.error', { id: eventId }) }}
          </RouterLink>
          <RouterLink
            v-for="traceId in replay.data.value.trace_ids"
            :key="traceId"
            :to="`/traces/${traceId}`"
          >
            {{ $t('replayDetail.trace', { id: traceId }) }}
          </RouterLink>
        </div>
      </section>
    </template>
  </section>
</template>
