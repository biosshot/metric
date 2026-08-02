<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { useQuery } from '@tanstack/vue-query';
import { useRoute } from 'vue-router';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import RelatedSignals, { type RelatedSignalLink } from '../components/RelatedSignals.vue';
import StackTrace from '../components/StackTrace.vue';
import StatusBadge from '../components/StatusBadge.vue';
import AppIcon from '../components/AppIcon.vue';
import { extractEventRelations } from '../lib/eventRelations';
import { queryLink } from '../lib/queryLinks';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const { locale, t } = useI18n();
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
const relations = computed(() => extractEventRelations(event.data.value?.body));
const relatedLinks = computed<RelatedSignalLink[]>(() => {
  const value = event.data.value;
  if (!value) return [];
  const links: RelatedSignalLink[] = [
    {
      key: 'issue',
      icon: 'bug',
      label: t('relations.issue'),
      description: value.issue_id,
      to: { path: `/issues/${value.issue_id}` },
    },
  ];
  if (relations.value.traceId) {
    links.push({
      key: 'trace',
      icon: 'traces',
      label: t('relations.openTrace'),
      description: relations.value.traceId,
      to: { path: `/traces/${relations.value.traceId}` },
    });
  }
  if (relations.value.replayId && session.selectedProject?.policy.items.replay) {
    links.push({
      key: 'replay',
      icon: 'replay',
      label: t('relations.openReplay'),
      description: relations.value.replayId,
      to: { path: `/replays/${relations.value.replayId}` },
    });
  }
  if (relations.value.release) {
    links.push({
      key: 'release',
      icon: 'release',
      label: t('relations.viewRelease'),
      description: relations.value.release,
      to: queryLink('/releases', 'rel', relations.value.release),
    });
  }
  if (relations.value.environment) {
    links.push({
      key: 'environment',
      icon: 'explore',
      label: t('relations.environmentErrors'),
      description: relations.value.environment,
      to: queryLink('/explore', 'env', relations.value.environment),
    });
  }
  if (relations.value.userId) {
    links.push({
      key: 'user',
      icon: 'users',
      label: t('relations.userErrors'),
      description: relations.value.userId,
      to: queryLink('/explore', 'user', relations.value.userId),
    });
  }
  return links;
});
const replayDisabled = computed(() =>
  Boolean(relations.value.replayId && !session.selectedProject?.policy.items.replay),
);
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
      <RelatedSignals :links="relatedLinks" />
      <p v-if="replayDisabled" class="permission-note" :title="relations.replayId">
        {{ $t('relations.replayDisabled') }}
      </p>
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
