<script setup lang="ts">
import { computed } from 'vue';
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
      Issue details
    </RouterLink>
    <LoadingPanel v-if="event.isPending.value" label="Loading exact Event…" />
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
          <h1>{{ exceptionTitle || String(event.data.value.body.message || 'Event details') }}</h1>
          <p>
            Occurred
            <time :datetime="event.data.value.occurred_at">
              {{ new Date(event.data.value.occurred_at).toLocaleString() }}
            </time>
          </p>
        </div>
        <dl class="event-identifiers">
          <div>
            <dt>Event ID</dt>
            <dd>
              <code>{{ event.data.value.event_id }}</code>
            </dd>
          </div>
          <div>
            <dt>Issue ID</dt>
            <dd>
              <code>{{ event.data.value.issue_id }}</code>
            </dd>
          </div>
        </dl>
      </header>
      <StackTrace :body="event.data.value.body" />
      <details class="raw-event">
        <summary>Raw normalized Event</summary>
        <p>
          This is the complete stable API representation. Secrets and disallowed PII have already
          been processed by the server policy.
        </p>
        <pre tabindex="0" aria-label="Scrollable normalized Event JSON"><code>{{
          JSON.stringify(event.data.value.body, null, 2)
        }}</code></pre>
      </details>
    </template>
  </section>
</template>
