<script setup lang="ts">
import { computed, ref, watch } from 'vue';
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
const projectId = computed(() => session.selectedProjectId ?? '');
const replayId = computed(() =>
  typeof route.query.replay_id === 'string' ? route.query.replay_id : '',
);
const status = ref('');
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const statusOptions: SelectOption[] = [
  { value: '', label: 'All feedback', icon: 'message' },
  { value: 'open', label: 'Open', icon: 'alert' },
  { value: 'resolved', label: 'Resolved', icon: 'success' },
  { value: 'spam', label: 'Spam', icon: 'blocked' },
];

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
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / users</p>
        <h1>User Feedback</h1>
        <p v-if="replayId">Feedback linked to Replay {{ replayId }}.</p>
        <p v-else>
          Read SDK-submitted reports, follow their investigation links, and triage status.
        </p>
      </div>
    </header>

    <div class="issue-toolbar feedback-toolbar">
      <BaseSelect
        v-model="status"
        class="compact-select"
        :options="statusOptions"
        label="Status"
        aria-label="Feedback status"
      />
    </div>

    <LoadingPanel v-if="feedback.isPending.value" label="Loading user feedback…" />
    <ApiErrorPanel
      v-else-if="feedback.error.value"
      :error="feedback.error.value"
      @retry="feedback.refetch()"
    />
    <EmptyState
      v-else-if="!feedback.data.value?.items.length"
      icon="message"
      title="No feedback in this view"
      description="Enable User Feedback for the project and connect the Sentry Browser Feedback API."
    >
      <SdkSetupButton />
    </EmptyState>
    <div v-else class="issue-table-wrap">
      <nav class="pagination" aria-label="Feedback result pages">
        <button
          class="button button--secondary"
          type="button"
          :disabled="history.length === 0"
          @click="previousPage"
        >
          Previous
        </button>
        <span>Page {{ history.length + 1 }}</span>
        <button
          class="button button--secondary"
          type="button"
          :disabled="!feedback.data.value.next_cursor"
          @click="nextPage"
        >
          Next
        </button>
      </nav>
      <div class="issue-table-scroll">
        <table class="issue-table feedback-table">
          <thead>
            <tr>
              <th scope="col">Feedback</th>
              <th scope="col">Status</th>
              <th scope="col">Reporter</th>
              <th scope="col">Attachments</th>
              <th scope="col">Received</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in feedback.data.value.items" :key="item.id">
              <td>
                <RouterLink :to="`/feedback/${item.id}`" class="issue-title">
                  {{ item.message }}
                </RouterLink>
                <span>{{ item.url || 'No page URL reported' }}</span>
              </td>
              <td><StatusBadge :status="item.status" /></td>
              <td>
                <span class="feedback-reporter">
                  <strong>{{ item.name || 'Anonymous user' }}</strong>
                  <small v-if="item.contact_email">{{ item.contact_email }}</small>
                </span>
              </td>
              <td>{{ item.attachments.length.toLocaleString() }}</td>
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
