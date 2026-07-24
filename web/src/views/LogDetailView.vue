<script setup lang="ts">
import { computed } from 'vue';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import CodeBlock from '../components/CodeBlock.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import { api } from '../api/client';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const projectId = computed(() => session.selectedProjectId ?? '');
const logId = computed(() => String(route.params.logId ?? ''));
const log = useQuery({
  queryKey: computed(() => ['log', projectId.value, logId.value]),
  queryFn: () => api.log(projectId.value, logId.value),
  enabled: computed(() => Boolean(projectId.value && logId.value)),
});
</script>

<template>
  <section>
    <RouterLink class="back-link" to="/logs">
      <AppIcon name="back" :size="16" />
      Structured Logs
    </RouterLink>
    <LoadingPanel v-if="log.isPending.value" label="Loading log detail…" />
    <ApiErrorPanel v-else-if="log.error.value" :error="log.error.value" @retry="log.refetch()" />
    <template v-else-if="log.data.value">
      <header class="page-header signal-detail-header">
        <div>
          <p class="eyebrow">
            {{ log.data.value.level }} / {{ log.data.value.service || 'service' }}
          </p>
          <h1>{{ log.data.value.message }}</h1>
          <p>{{ log.data.value.timestamp }}</p>
        </div>
        <RouterLink
          v-if="log.data.value.trace_id"
          class="button button--secondary"
          :to="`/traces/${log.data.value.trace_id}`"
        >
          <AppIcon name="traces" :size="16" />
          Open Trace
        </RouterLink>
      </header>
      <div class="metric-grid">
        <article>
          <span>Environment</span><strong>{{ log.data.value.environment || '—' }}</strong>
        </article>
        <article>
          <span>Release</span><strong>{{ log.data.value.release || '—' }}</strong>
        </article>
        <article>
          <span>Trace</span><strong>{{ log.data.value.trace_id?.slice(0, 12) || '—' }}</strong>
        </article>
        <article>
          <span>Span</span><strong>{{ log.data.value.span_id || '—' }}</strong>
        </article>
      </div>
      <section class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Accepted payload</p>
            <h2>Attributes and context</h2>
          </div>
        </div>
        <CodeBlock language="json" :code="JSON.stringify(log.data.value.body, null, 2)" />
      </section>
    </template>
  </section>
</template>
