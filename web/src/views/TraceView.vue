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
import { api } from '../api/client';
import type { Span } from '../api/types';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const { locale } = useI18n();
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
      <section v-if="trace.data.value.errors.length" class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('traceDetail.correlation') }}</p>
            <h2>{{ $t('traceDetail.errorEvents') }}</h2>
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
              ><strong>{{ $t('traceDetail.errorEvent') }}</strong
              ><small>{{ error.event_id }}</small></span
            >
            <AppIcon name="view" :size="16" />
          </RouterLink>
        </div>
      </section>
      <section class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('traceDetail.correlation') }}</p>
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
            :to="`/logs/${log.id}`"
          >
            <span class="signal-level">{{ $t(`status.${log.level}`) }}</span>
            <strong>{{ log.message }}</strong>
            <span>{{ log.service || $t('traceDetail.service') }}</span>
            <time :datetime="log.timestamp">{{ formatTime(log.timestamp) }}</time>
          </RouterLink>
        </div>
      </section>
    </template>
  </section>
</template>
