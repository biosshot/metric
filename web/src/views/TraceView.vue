<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import RelatedSignals, { type RelatedSignalLink } from '../components/RelatedSignals.vue';
import { api } from '../api/client';
import type { Span } from '../api/types';
import { queryLink, queryLinks } from '../lib/queryLinks';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const { locale, t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const traceId = computed(() => String(route.params.traceId ?? ''));
const trace = useQuery({
  queryKey: computed(() => ['trace', projectId.value, traceId.value]),
  queryFn: () => api.trace(projectId.value, traceId.value),
  enabled: computed(() => Boolean(projectId.value && traceId.value)),
});
const selectedSpanId = computed(() => String(route.query.span ?? ''));
const selectedSpan = computed(() =>
  trace.data.value?.spans.find((span) => span.span_id === selectedSpanId.value),
);
const representativeSpan = computed(() => {
  const spans = trace.data.value?.spans ?? [];
  return (
    spans.find((span) => span.parent_span_id === null) ??
    spans.find((span) => span.is_segment) ??
    spans[0]
  );
});
const relatedLinks = computed<RelatedSignalLink[]>(() => {
  const span = representativeSpan.value;
  const links: RelatedSignalLink[] = [];
  for (const error of trace.data.value?.errors ?? []) {
    links.push({
      key: `event-${error.event_id}`,
      icon: 'failure',
      label: t('traceDetail.errorEvent'),
      description: error.event_id,
      to: { path: `/events/${error.event_id}` },
    });
  }
  if (session.selectedProject?.policy.items.replay) {
    for (const replayId of new Set(trace.data.value?.replay_ids ?? [])) {
      links.push({
        key: `replay-${replayId}`,
        icon: 'replay',
        label: t('relations.openReplay'),
        description: replayId,
        to: { path: `/replays/${replayId}` },
      });
    }
  }
  for (const feedbackId of new Set(trace.data.value?.feedback_ids ?? [])) {
    links.push({
      key: `feedback-${feedbackId}`,
      icon: 'message',
      label: t('relations.feedback'),
      description: feedbackId,
      to: { path: `/feedback/${feedbackId}` },
    });
  }
  if (!span) return links;
  if (span.release) {
    links.push({
      key: 'release',
      icon: 'release',
      label: t('relations.viewRelease'),
      description: span.release,
      to: queryLink('/releases', 'rel', span.release),
    });
  }
  if (span.environment) {
    links.push({
      key: 'environment-logs',
      icon: 'logs',
      label: t('relations.environmentLogs'),
      description: span.environment,
      to: queryLinks('/logs', [
        ['env', span.environment],
        ['trace', traceId.value],
      ]),
    });
  }
  if (span.service) {
    links.push({
      key: 'service-logs',
      icon: 'server',
      label: t('relations.serviceLogs'),
      description: span.service,
      to: queryLinks('/logs', [
        ['svc', span.service],
        ['trace', traceId.value],
      ]),
    });
  }
  return links;
});
const replayDisabled = computed(() =>
  Boolean(trace.data.value?.replay_ids?.length && !session.selectedProject?.policy.items.replay),
);
const bounds = computed(() => {
  const spans = trace.data.value?.spans ?? [];
  const start = Math.min(...spans.map((span) => Number(span.started_at_ns)));
  const end = Math.max(
    ...spans.map((span) => Number(span.started_at_ns) + Number(span.duration_ns)),
  );
  return { start, width: Math.max(end - start, 1) };
});

function waterfallStyle(span: Span): Record<string, string> {
  const left = ((Number(span.started_at_ns) - bounds.value.start) / bounds.value.width) * 100;
  const width = Math.max((Number(span.duration_ns) / bounds.value.width) * 100, 0.35);
  return { '--span-left': `${left}%`, '--span-width': `${Math.min(width, 100 - left)}%` };
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
    <RouterLink class="back-link" to="/traces">
      <AppIcon name="back" :size="16" />
      {{ $t('traceDetail.transactions') }}
    </RouterLink>
    <LoadingPanel v-if="trace.isPending.value" :label="$t('traceDetail.loading')" />
    <ApiErrorPanel
      v-else-if="trace.error.value"
      :error="trace.error.value"
      @retry="trace.refetch()"
    />
    <template v-else-if="trace.data.value">
      <header class="page-header">
        <div>
          <p class="eyebrow">{{ $t('traceDetail.virtual') }}</p>
          <h1>{{ trace.data.value.trace_id }}</h1>
          <p>
            {{ $t('traceDetail.spans', trace.data.value.spans.length) }} ·
            {{ $t('traceDetail.logs', trace.data.value.logs.length) }}
          </p>
        </div>
        <span v-if="trace.data.value.partial" class="insight-pill">
          {{ $t('traceDetail.partial') }} ·
          {{ $t('traceDetail.omitted', trace.data.value.omitted_spans) }}
        </span>
      </header>
      <section class="trace-waterfall">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('traceDetail.timeline') }}</p>
            <h2>{{ $t('traceDetail.waterfall') }}</h2>
          </div>
        </div>
        <RouterLink
          v-for="span in trace.data.value.spans"
          :key="span.id"
          class="waterfall-row"
          :to="{ query: { span: span.span_id } }"
        >
          <span class="waterfall-label">
            <strong>{{ span.name }}</strong>
            <small>{{ span.operation }} · {{ span.duration_ms.toFixed(1) }} ms</small>
          </span>
          <span class="waterfall-track">
            <i
              :class="{ 'waterfall-bar--failed': span.status && span.status !== 'ok' }"
              :style="waterfallStyle(span)"
            ></i>
          </span>
        </RouterLink>
      </section>
      <section v-if="selectedSpan" class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('traceDetail.spanDetail') }}</p>
            <h2>{{ selectedSpan.name }}</h2>
          </div>
        </div>
        <CodeBlock language="json" :code="JSON.stringify(selectedSpan.body, null, 2)" />
      </section>
      <section class="detail-panel">
        <div class="section-heading">
          <div>
            <h2>{{ $t('traceDetail.structuredLogs') }}</h2>
          </div>
        </div>
        <EmptyState
          v-if="!trace.data.value.logs.length"
          icon="logs"
          :title="$t('traceDetail.noLogs')"
          :description="$t('traceDetail.noLogsDescription')"
        />
        <div v-else class="signal-list">
          <RouterLink
            v-for="log in trace.data.value.logs"
            :key="log.id"
            class="log-row"
            :class="`signal-accent--${log.level}`"
            :to="`/logs/${log.id}`"
          >
            <span class="signal-level">{{ $t(`status.${log.level}`) }}</span>
            <strong>{{ log.message }}</strong>
            <span>{{ log.service || $t('traceDetail.service') }}</span>
            <time :datetime="log.timestamp">{{ formatTime(log.timestamp) }}</time>
          </RouterLink>
        </div>
      </section>
      <RelatedSignals :links="relatedLinks" />
      <p v-if="replayDisabled" class="permission-note">
        {{ $t('relations.replayDisabled') }}
      </p>
    </template>
  </section>
</template>
