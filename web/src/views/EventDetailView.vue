<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { useQuery } from '@tanstack/vue-query';
import { useRoute } from 'vue-router';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StackTrace from '../components/StackTrace.vue';
import StatusBadge from '../components/StatusBadge.vue';
import AppIcon from '../components/AppIcon.vue';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const { locale } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const eventId = computed(() => String(route.params.eventId));
const event = useQuery({
  queryKey: computed(() => ['event', projectId.value, eventId.value]),
  queryFn: () => api.event(projectId.value, eventId.value),
});

const exceptionTitle = computed(() => {
  const body = event.data.value?.body;
  if (!body) return '';
  const exception = body.exception as { values?: Array<{ type?: string; value?: string }> };
  const first = exception?.values?.[0];
  return [first?.type, first?.value].filter(Boolean).join(': ');
});
</script>

<template>
  <section>
    <RouterLink
      v-if="event.data.value"
      class="back-link"
      :to="`/issues/${event.data.value.issue_id}`"
    >
      <AppIcon name="back" :size="16" />
      {{ $t('eventDetail.issueDetails') }}
    </RouterLink>
    <LoadingPanel v-if="event.isPending.value" :label="$t('eventDetail.loading')" />
    <ApiErrorPanel
      v-else-if="event.error.value"
      :error="event.error.value"
      @retry="event.refetch()"
    />
    <template v-else-if="event.data.value">
      <header class="event-header">
        <div>
          <div class="heading-line">
            <StatusBadge :status="event.data.value.level" />
            <span>{{ event.data.value.platform }}</span>
          </div>
          <h1>
            {{ exceptionTitle || String(event.data.value.body.message || $t('eventDetail.title')) }}
          </h1>
          <p>
            {{ $t('eventDetail.occurred') }}
            <time :datetime="event.data.value.occurred_at">
              {{ new Date(event.data.value.occurred_at).toLocaleString(locale) }}
            </time>
          </p>
        </div>
        <dl class="event-identifiers">
          <div>
            <dt>{{ $t('eventDetail.eventId') }}</dt>
            <dd>
              <code>{{ event.data.value.event_id }}</code>
            </dd>
          </div>
          <div>
            <dt>{{ $t('eventDetail.issueId') }}</dt>
            <dd>
              <code>{{ event.data.value.issue_id }}</code>
            </dd>
          </div>
        </dl>
      </header>
      <StackTrace :body="event.data.value.body" />
      <details class="raw-event">
        <summary>{{ $t('eventDetail.raw') }}</summary>
        <p>{{ $t('eventDetail.rawDescription') }}</p>
        <pre tabindex="0" :aria-label="$t('eventDetail.rawJson')"><code>{{
          JSON.stringify(event.data.value.body, null, 2)
        }}</code></pre>
      </details>
    </template>
  </section>
</template>
