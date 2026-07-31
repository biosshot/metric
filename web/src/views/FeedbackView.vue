<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { api } from '../api/client';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const { locale, t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const replayId = computed(() =>
  typeof route.query.replay_id === 'string' ? route.query.replay_id : '',
);
const status = ref('');
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const statusOptions = computed<SelectOption[]>(() => [
  { value: '', label: t('feedback.all'), icon: 'message' },
  { value: 'open', label: t('feedback.open'), icon: 'alert' },
  { value: 'resolved', label: t('feedback.resolved'), icon: 'success' },
  { value: 'spam', label: t('feedback.spam'), icon: 'blocked' },
]);

const feedback = useQuery({
  queryKey: computed(() => [
    'feedback',
    projectId.value,
    status.value,
    cursor.value,
    replayId.value,
  ]),
  queryFn: () =>
    api.feedback(
      projectId.value,
      status.value || undefined,
      cursor.value,
      replayId.value || undefined,
    ),
  enabled: computed(() => Boolean(projectId.value)),
});

watch([projectId, status, replayId], () => {
  cursor.value = null;
  history.value = [];
});

function nextPage(): void {
  const next = feedback.data.value?.next_cursor;
  if (!next) return;
  history.value.push(cursor.value);
  cursor.value = next;
}

function previousPage(): void {
  cursor.value = history.value.pop() ?? null;
}

function formatTime(value: string): string {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{ $t('feedback.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('feedback.title') }}</h1>
        <p v-if="replayId">{{ $t('feedback.replayDescription', { id: replayId }) }}</p>
        <p v-else>{{ $t('feedback.description') }}</p>
      </div>
    </header>

    <div class="issue-toolbar feedback-toolbar">
      <BaseSelect
        v-model="status"
        class="compact-select"
        :options="statusOptions"
        :label="$t('feedback.status')"
        :aria-label="$t('feedback.statusLabel')"
      />
    </div>

    <LoadingPanel v-if="feedback.isPending.value" :label="$t('feedback.loading')" />
    <ApiErrorPanel
      v-else-if="feedback.error.value"
      :error="feedback.error.value"
      @retry="feedback.refetch()"
    />
    <EmptyState
      v-else-if="!feedback.data.value?.items.length"
      icon="message"
      :title="$t('feedback.empty')"
      :description="$t('feedback.emptyDescription')"
    >
      <SdkSetupButton />
    </EmptyState>
    <div v-else class="issue-table-wrap">
      <nav class="pagination" :aria-label="$t('feedback.pages')">
        <button
          class="button button--secondary"
          type="button"
          :disabled="history.length === 0"
          @click="previousPage"
        >
          {{ $t('common.previous') }}
        </button>
        <span>{{ $t('common.page', { page: history.length + 1 }) }}</span>
        <button
          class="button button--secondary"
          type="button"
          :disabled="!feedback.data.value.next_cursor"
          @click="nextPage"
        >
          {{ $t('common.next') }}
        </button>
      </nav>
      <div class="issue-table-scroll">
        <table class="issue-table feedback-table">
          <thead>
            <tr>
              <th scope="col">{{ $t('feedback.feedback') }}</th>
              <th scope="col">{{ $t('feedback.status') }}</th>
              <th scope="col">{{ $t('feedback.reporter') }}</th>
              <th scope="col">{{ $t('feedback.attachments') }}</th>
              <th scope="col">{{ $t('feedback.received') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in feedback.data.value.items" :key="item.id">
              <td>
                <RouterLink :to="`/feedback/${item.id}`" class="issue-title">
                  {{ item.message }}
                </RouterLink>
                <span>{{ item.url || $t('feedback.noUrl') }}</span>
              </td>
              <td><StatusBadge :status="item.status" /></td>
              <td>
                <span class="feedback-reporter">
                  <strong>{{ item.name || $t('feedback.anonymous') }}</strong>
                  <small v-if="item.contact_email">{{ item.contact_email }}</small>
                </span>
              </td>
              <td>{{ item.attachments.length.toLocaleString(locale) }}</td>
              <td>
                <time :datetime="item.received_at">{{ formatTime(item.received_at) }}</time>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </section>
</template>
