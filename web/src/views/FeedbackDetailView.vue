<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useRoute } from 'vue-router';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { api } from '../api/client';
import type { FeedbackAttachment, FeedbackStatus } from '../api/types';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const queryClient = useQueryClient();
const projectId = computed(() => session.selectedProjectId ?? '');
const feedbackId = computed(() => String(route.params.feedbackId ?? ''));
const selectedStatus = ref<FeedbackStatus>('open');
const attachmentError = ref<unknown>(null);
const downloadingAttachmentId = ref('');
const statusOptions: SelectOption[] = [
  { value: 'open', label: 'Open', icon: 'alert' },
  { value: 'resolved', label: 'Resolved', icon: 'success' },
  { value: 'spam', label: 'Spam', icon: 'blocked' },
];
const feedback = useQuery({
  queryKey: computed(() => ['feedback-item', projectId.value, feedbackId.value]),
  queryFn: () => api.feedbackItem(projectId.value, feedbackId.value),
  enabled: computed(() => Boolean(projectId.value && feedbackId.value)),
});

watch(
  () => feedback.data.value?.status,
  (value) => {
    if (value) selectedStatus.value = value;
  },
  { immediate: true },
);

const saveStatus = useMutation({
  mutationFn: () =>
    api.updateFeedbackStatus(projectId.value, feedbackId.value, selectedStatus.value),
  onSuccess: async () => {
    await queryClient.invalidateQueries({
      queryKey: ['feedback-item', projectId.value, feedbackId.value],
    });
    await queryClient.invalidateQueries({ queryKey: ['feedback', projectId.value] });
  },
});

function setStatus(value: string): void {
  selectedStatus.value = value as FeedbackStatus;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KiB`;
}

async function downloadAttachment(attachment: FeedbackAttachment): Promise<void> {
  attachmentError.value = null;
  downloadingAttachmentId.value = attachment.attachment_id;
  try {
    const blob = await api.feedbackAttachment(
      projectId.value,
      feedbackId.value,
      attachment.attachment_id,
    );
    const objectUrl = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = objectUrl;
    link.download = attachment.filename;
    link.hidden = true;
    document.body.append(link);
    try {
      link.click();
    } finally {
      link.remove();
      URL.revokeObjectURL(objectUrl);
    }
  } catch (error) {
    attachmentError.value = error;
  } finally {
    downloadingAttachmentId.value = '';
  }
}
</script>

<template>
  <section>
    <RouterLink class="back-link" to="/feedback">
      <AppIcon name="back" :size="16" />
      User Feedback
    </RouterLink>
    <LoadingPanel v-if="feedback.isPending.value" label="Loading feedback detail…" />
    <ApiErrorPanel
      v-else-if="feedback.error.value"
      :error="feedback.error.value"
      @retry="feedback.refetch()"
    />
    <template v-else-if="feedback.data.value">
      <header class="page-header feedback-detail-header">
        <div>
          <p class="eyebrow">{{ feedback.data.value.id }}</p>
          <h1>{{ feedback.data.value.name || 'Anonymous user' }}</h1>
          <p>{{ feedback.data.value.received_at }}</p>
        </div>
        <StatusBadge :status="feedback.data.value.status" />
      </header>

      <ApiErrorPanel
        v-if="saveStatus.error.value"
        :error="saveStatus.error.value"
        title="Feedback status was not saved"
      />
      <section class="detail-panel feedback-message">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Submitted message</p>
            <h2>User report</h2>
          </div>
        </div>
        <p>{{ feedback.data.value.message }}</p>
      </section>

      <div class="metric-grid">
        <article>
          <span>Contact</span
          ><strong>{{ feedback.data.value.contact_email || 'Not provided' }}</strong>
        </article>
        <article>
          <span>Page URL</span><strong>{{ feedback.data.value.url || 'Not provided' }}</strong>
        </article>
        <article>
          <span>Attachments</span><strong>{{ feedback.data.value.attachments.length }}</strong>
        </article>
        <article>
          <span>Expires</span><strong>{{ feedback.data.value.expires_at }}</strong>
        </article>
      </div>

      <section class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Investigation</p>
            <h2>Related telemetry</h2>
          </div>
        </div>
        <div class="feedback-links">
          <RouterLink
            v-if="feedback.data.value.associated_event_id"
            class="button button--secondary"
            :to="`/events/${feedback.data.value.associated_event_id}`"
          >
            <AppIcon name="bug" :size="16" /> Event
          </RouterLink>
          <RouterLink
            v-if="feedback.data.value.issue_id"
            class="button button--secondary"
            :to="`/issues/${feedback.data.value.issue_id}`"
          >
            <AppIcon name="clipboard" :size="16" /> Issue
          </RouterLink>
          <RouterLink
            v-if="feedback.data.value.trace_id"
            class="button button--secondary"
            :to="`/traces/${feedback.data.value.trace_id}`"
          >
            <AppIcon name="traces" :size="16" /> Trace
          </RouterLink>
          <span v-if="feedback.data.value.replay_id" class="permission-note">
            Replay {{ feedback.data.value.replay_id.slice(0, 12) }} is linked but Replay UI is not
            enabled.
          </span>
          <span
            v-if="
              !feedback.data.value.associated_event_id &&
              !feedback.data.value.trace_id &&
              !feedback.data.value.replay_id
            "
            class="permission-note"
          >
            This report has no telemetry link.
          </span>
        </div>
      </section>

      <section v-if="feedback.data.value.attachments.length" class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">BlobStore</p>
            <h2>Attachments</h2>
          </div>
        </div>
        <ApiErrorPanel
          v-if="attachmentError"
          :error="attachmentError"
          title="Feedback attachment was not downloaded"
        />
        <div class="feedback-attachments">
          <button
            v-for="attachment in feedback.data.value.attachments"
            :key="attachment.attachment_id"
            class="feedback-attachment"
            type="button"
            :disabled="downloadingAttachmentId === attachment.attachment_id"
            @click="downloadAttachment(attachment)"
          >
            <AppIcon
              :name="downloadingAttachmentId === attachment.attachment_id ? 'loading' : 'fileCode'"
              :size="18"
            />
            <span
              ><strong>{{ attachment.filename }}</strong
              ><small>{{ attachment.content_type }}</small></span
            >
            <span>
              {{
                downloadingAttachmentId === attachment.attachment_id
                  ? 'Downloading…'
                  : formatBytes(attachment.size)
              }}
            </span>
          </button>
        </div>
      </section>

      <form
        v-if="session.has('issue:write')"
        class="panel feedback-status-form"
        @submit.prevent="saveStatus.mutate()"
      >
        <BaseSelect
          :model-value="selectedStatus"
          :options="statusOptions"
          label="Workflow status"
          @update:model-value="setStatus"
        />
        <button
          class="button button--primary"
          type="submit"
          :disabled="saveStatus.isPending.value || selectedStatus === feedback.data.value.status"
        >
          <AppIcon name="save" :size="16" />
          Save status
        </button>
      </form>
    </template>
  </section>
</template>
