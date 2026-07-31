<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useRoute } from 'vue-router';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { api } from '../api/client';
import type { FeedbackAttachment, FeedbackStatus } from '../api/types';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const queryClient = useQueryClient();
const { locale, t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const feedbackId = computed(() => String(route.params.feedbackId ?? ''));
const attachmentError = ref<unknown>(null);
const downloadingAttachmentId = ref('');
const mutationNotice = ref('');
const feedback = useQuery({
  queryKey: computed(() => ['feedback-item', projectId.value, feedbackId.value]),
  queryFn: () => api.feedbackItem(projectId.value, feedbackId.value),
  enabled: computed(() => Boolean(projectId.value && feedbackId.value)),
});

const saveStatus = useMutation({
  mutationFn: (status: FeedbackStatus) =>
    api.updateFeedbackStatus(projectId.value, feedbackId.value, status),
  onSuccess: async (updated) => {
    mutationNotice.value = t('feedbackDetail.marked', { status: t(`status.${updated.status}`) });
    await queryClient.invalidateQueries({
      queryKey: ['feedback-item', projectId.value, feedbackId.value],
    });
    await queryClient.invalidateQueries({ queryKey: ['feedback', projectId.value] });
  },
});

function formatTime(value: string): string {
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}

function formatBytes(bytes: number): string {
  const number = new Intl.NumberFormat(locale.value, { maximumFractionDigits: 1 });
  if (bytes < 1024) return `${number.format(bytes)} B`;
  return `${number.format(bytes / 1024)} KiB`;
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
  <section class="feedback-detail">
    <RouterLink class="back-link" to="/feedback">
      <AppIcon name="back" :size="16" />
      {{ $t('feedbackDetail.all') }}
    </RouterLink>
    <LoadingPanel v-if="feedback.isPending.value" :label="$t('feedbackDetail.loading')" />
    <ApiErrorPanel
      v-else-if="feedback.error.value"
      :error="feedback.error.value"
      @retry="feedback.refetch()"
    />
    <template v-else-if="feedback.data.value">
      <header class="issue-detail-header feedback-detail-header">
        <div>
          <div class="heading-line">
            <StatusBadge :status="feedback.data.value.status" />
            <code>{{ feedback.data.value.id }}</code>
          </div>
          <h1>{{ feedback.data.value.message }}</h1>
          <p>
            {{
              $t('feedbackDetail.submitted', {
                name: feedback.data.value.name || $t('feedbackDetail.anonymous'),
                time: formatTime(feedback.data.value.received_at),
              })
            }}
          </p>
        </div>
        <div
          v-if="session.has('issue:write')"
          class="feedback-workflow"
          :aria-label="$t('feedbackDetail.workflow')"
        >
          <span class="eyebrow">{{ $t('feedbackDetail.workflowStatus') }}</span>
          <div class="button-group">
            <button
              v-if="feedback.data.value.status !== 'resolved'"
              class="button button--primary"
              type="button"
              :disabled="saveStatus.isPending.value"
              @click="saveStatus.mutate('resolved')"
            >
              <AppIcon name="success" :size="16" />
              {{ $t('feedbackDetail.resolve') }}
            </button>
            <button
              v-if="feedback.data.value.status !== 'spam'"
              class="button button--secondary"
              type="button"
              :disabled="saveStatus.isPending.value"
              @click="saveStatus.mutate('spam')"
            >
              <AppIcon name="blocked" :size="16" />
              {{ $t('feedbackDetail.markSpam') }}
            </button>
            <button
              v-if="feedback.data.value.status !== 'open'"
              class="button button--secondary"
              type="button"
              :disabled="saveStatus.isPending.value"
              @click="saveStatus.mutate('open')"
            >
              <AppIcon name="refresh" :size="16" />
              {{ $t('feedbackDetail.reopen') }}
            </button>
          </div>
        </div>
        <p v-else class="permission-note">{{ $t('feedbackDetail.readOnly') }}</p>
      </header>

      <p v-if="mutationNotice" class="success-notice" role="status">{{ mutationNotice }}</p>
      <ApiErrorPanel
        v-if="saveStatus.error.value"
        :error="saveStatus.error.value"
        :title="$t('feedbackDetail.saveFailed')"
      />
      <div class="metric-grid feedback-metadata-grid">
        <article>
          <span>{{ $t('feedbackDetail.reporter') }}</span>
          <strong>{{ feedback.data.value.name || $t('feedback.anonymous') }}</strong>
          <small>{{ feedback.data.value.contact_email || $t('feedbackDetail.noContact') }}</small>
        </article>
        <article>
          <span>{{ $t('feedbackDetail.pageUrl') }}</span>
          <a
            v-if="feedback.data.value.url"
            class="text-link"
            :href="feedback.data.value.url"
            target="_blank"
            rel="noreferrer"
          >
            {{ feedback.data.value.url }}
          </a>
          <strong v-else>{{ $t('feedbackDetail.notProvided') }}</strong>
        </article>
        <article>
          <span>{{ $t('feedbackDetail.received') }}</span
          ><strong>{{ formatTime(feedback.data.value.received_at) }}</strong>
        </article>
        <article>
          <span>{{ $t('feedbackDetail.expires') }}</span
          ><strong>{{ formatTime(feedback.data.value.expires_at) }}</strong>
        </article>
      </div>

      <section class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('feedbackDetail.investigation') }}</p>
            <h2>{{ $t('feedbackDetail.telemetry') }}</h2>
          </div>
        </div>
        <div class="feedback-links">
          <RouterLink
            v-if="feedback.data.value.associated_event_id"
            class="button button--secondary"
            :to="`/events/${feedback.data.value.associated_event_id}`"
          >
            <AppIcon name="bug" :size="16" /> {{ $t('feedbackDetail.event') }}
          </RouterLink>
          <RouterLink
            v-if="feedback.data.value.issue_id"
            class="button button--secondary"
            :to="`/issues/${feedback.data.value.issue_id}`"
          >
            <AppIcon name="clipboard" :size="16" /> {{ $t('feedbackDetail.issue') }}
          </RouterLink>
          <RouterLink
            v-if="feedback.data.value.trace_id"
            class="button button--secondary"
            :to="`/traces/${feedback.data.value.trace_id}`"
          >
            <AppIcon name="traces" :size="16" /> {{ $t('feedbackDetail.trace') }}
          </RouterLink>
          <RouterLink
            v-if="feedback.data.value.replay_id && session.selectedProject?.policy.items.replay"
            class="button button--secondary"
            :to="`/replays/${feedback.data.value.replay_id}`"
          >
            <AppIcon name="replay" :size="16" /> {{ $t('feedbackDetail.replay') }}
          </RouterLink>
          <span
            v-else-if="feedback.data.value.replay_id"
            class="permission-note feedback-replay-note"
          >
            {{
              $t('feedbackDetail.replayDisabled', {
                id: feedback.data.value.replay_id.slice(0, 12),
              })
            }}
          </span>
          <span
            v-if="
              !feedback.data.value.associated_event_id &&
              !feedback.data.value.issue_id &&
              !feedback.data.value.trace_id &&
              !feedback.data.value.replay_id
            "
            class="permission-note"
          >
            {{ $t('feedbackDetail.noTelemetry') }}
          </span>
        </div>
      </section>

      <section v-if="feedback.data.value.attachments.length" class="detail-panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">BlobStore</p>
            <h2>{{ $t('feedbackDetail.attachments') }}</h2>
          </div>
        </div>
        <ApiErrorPanel
          v-if="attachmentError"
          :error="attachmentError"
          :title="$t('feedbackDetail.attachmentFailed')"
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
                  ? $t('feedbackDetail.downloading')
                  : formatBytes(attachment.size)
              }}
            </span>
          </button>
        </div>
      </section>
    </template>
  </section>
</template>
