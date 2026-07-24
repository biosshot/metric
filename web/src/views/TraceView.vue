<script setup lang="ts">
import { computed } from 'vue';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import { api } from '../api/client';
import type { Span } from '../api/types';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
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
</script>

<template>
  <section>
    <RouterLink class="back-link" to="/traces">
      <AppIcon name="back" :size="16" />
      Transactions
    </RouterLink>
    <LoadingPanel v-if="trace.isPending.value" label="Reconstructing bounded Trace…" />
    <ApiErrorPanel
      v-else-if="trace.error.value"
      :error="trace.error.value"
      @retry="trace.refetch()"
    />
    <template v-else-if="trace.data.value">
      <header class="page-header">
        <div>
          <p class="eyebrow">Virtual Trace</p>
          <h1>{{ trace.data.value.trace_id }}</h1>
          <p>
            {{ trace.data.value.spans.length }} spans ·
            {{ trace.data.value.logs.length }} correlated logs
          </p>
        </div>
        <span v-if="trace.data.value.partial" class="insight-pill">
          Partial · {{ trace.data.value.omitted_spans }} omitted
        </span>
      </header>
      <section class="trace-waterfall">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Timeline</p>
            <h2>Waterfall</h2>
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
            <p class="eyebrow">Span detail</p>
            <h2>{{ selectedSpan.name }}</h2>
          </div>
        </div>
        <CodeBlock language="json" :code="JSON.stringify(selectedSpan.body, null, 2)" />
      </section>
      <section v-if="trace.data.value.errors.length" class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Correlation</p>
            <h2>Error Events</h2>
          </div>
        </div>
        <div class="compact-list">
          <RouterLink
            v-for="error in trace.data.value.errors"
            :key="error.event_id"
            :to="`/events/${error.event_id}`"
          >
            <AppIcon name="failure" :size="16" />
            <span
              ><strong>Error Event</strong><small>{{ error.event_id }}</small></span
            >
            <AppIcon name="view" :size="16" />
          </RouterLink>
        </div>
      </section>
      <section class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Correlation</p>
            <h2>Structured Logs</h2>
          </div>
        </div>
        <EmptyState
          v-if="!trace.data.value.logs.length"
          icon="logs"
          title="No correlated logs"
          description="Logs appear here when their Trace ID matches this Trace."
        />
        <div v-else class="signal-list">
          <RouterLink
            v-for="log in trace.data.value.logs"
            :key="log.id"
            class="log-row"
            :to="`/logs/${log.id}`"
          >
            <span class="signal-level">{{ log.level }}</span>
            <strong>{{ log.message }}</strong>
            <span>{{ log.service || 'service' }}</span>
            <time :datetime="log.timestamp">{{ log.timestamp }}</time>
          </RouterLink>
        </div>
      </section>
    </template>
  </section>
</template>
