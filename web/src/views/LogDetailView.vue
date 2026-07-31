<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
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
const { locale } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const logId = computed(() => String(route.params.logId ?? ''));
const log = useQuery({
  queryKey: computed(() => ['log', projectId.value, logId.value]),
  queryFn: () => api.log(projectId.value, logId.value),
  enabled: computed(() => Boolean(projectId.value && logId.value)),
});

function formatTime(value: string): string {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'medium',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <RouterLink class="back-link" to="/logs">
      <AppIcon name="back" :size="16" />
      {{ $t('logDetail.all') }}
    </RouterLink>
    <LoadingPanel v-if="log.isPending.value" :label="$t('logDetail.loading')" />
    <ApiErrorPanel v-else-if="log.error.value" :error="log.error.value" @retry="log.refetch()" />
    <template v-else-if="log.data.value">
      <header class="page-header signal-detail-header">
        <div>
          <p class="eyebrow">
            {{ $t(`status.${log.data.value.level}`) }} /
            {{ log.data.value.service || $t('logDetail.service') }}
          </p>
          <h1>{{ log.data.value.message }}</h1>
          <p>{{ formatTime(log.data.value.timestamp) }}</p>
        </div>
        <RouterLink
          v-if="log.data.value.trace_id"
          class="button button--secondary"
          :to="`/traces/${log.data.value.trace_id}`"
        >
          <AppIcon name="traces" :size="16" />
          {{ $t('logDetail.openTrace') }}
        </RouterLink>
      </header>
      <div class="metric-grid">
        <article>
          <span>{{ $t('logDetail.environment') }}</span
          ><strong>{{ log.data.value.environment || '—' }}</strong>
        </article>
        <article>
          <span>{{ $t('logDetail.release') }}</span
          ><strong>{{ log.data.value.release || '—' }}</strong>
        </article>
        <article>
          <span>{{ $t('logDetail.trace') }}</span
          ><strong>{{ log.data.value.trace_id?.slice(0, 12) || '—' }}</strong>
        </article>
        <article>
          <span>{{ $t('logDetail.span') }}</span
          ><strong>{{ log.data.value.span_id || '—' }}</strong>
        </article>
      </div>
      <section class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('logDetail.payload') }}</p>
            <h2>{{ $t('logDetail.attributes') }}</h2>
          </div>
        </div>
        <CodeBlock language="json" :code="JSON.stringify(log.data.value.body, null, 2)" />
      </section>
    </template>
  </section>
</template>
