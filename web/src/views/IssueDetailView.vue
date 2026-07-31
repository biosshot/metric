<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useRoute } from 'vue-router';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import AppIcon from '../components/AppIcon.vue';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const queryClient = useQueryClient();
const { locale, t } = useI18n();
const mutationNotice = ref('');

const projectId = computed(() => session.selectedProjectId ?? '');
const issueId = computed(() => String(route.params.issueId));
const issue = useQuery({
  queryKey: computed(() => ['issue', projectId.value, issueId.value]),
  queryFn: () => api.issue(projectId.value, issueId.value),
});
const statistics = useQuery({
  queryKey: computed(() => ['issue-statistics', projectId.value, issueId.value]),
  queryFn: () => api.issueStatistics(projectId.value, issueId.value),
});
const activity = useQuery({
  queryKey: computed(() => ['issue-activity', projectId.value, issueId.value]),
  queryFn: () => api.issueActivity(projectId.value, issueId.value),
});
const events = useQuery({
  queryKey: computed(() => ['issue-events', projectId.value, issueId.value]),
  queryFn: () => api.issueEvents(projectId.value, issueId.value),
});

const lifecycle = useMutation({
  mutationFn: (action: string) => api.mutateIssue(projectId.value, issueId.value, action),
  onSuccess: async (response) => {
    const translatedStatus = t(`status.${response.issue.status}`);
    mutationNotice.value = response.applied
      ? t('issueDetail.marked', { status: translatedStatus })
      : t('issueDetail.unchanged', { status: translatedStatus });
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ['issue', projectId.value, issueId.value] }),
      queryClient.invalidateQueries({
        queryKey: ['issue-activity', projectId.value, issueId.value],
      }),
    ]);
  },
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
    <RouterLink class="back-link" to="/issues">
      <AppIcon name="back" :size="16" />
      {{ $t('issueDetail.all') }}
    </RouterLink>
    <LoadingPanel v-if="issue.isPending.value" :label="$t('issueDetail.loading')" />
    <ApiErrorPanel
      v-else-if="issue.error.value"
      :error="issue.error.value"
      @retry="issue.refetch()"
    />
    <template v-else-if="issue.data.value">
      <header class="issue-detail-header">
        <div>
          <div class="heading-line">
            <StatusBadge :status="issue.data.value.status" />
            <code>{{ issue.data.value.id }}</code>
          </div>
          <h1>{{ issue.data.value.title }}</h1>
          <p>{{ issue.data.value.culprit || issue.data.value.grouping.summary }}</p>
        </div>
        <div
          v-if="session.has('issue:write')"
          class="button-group"
          :aria-label="$t('issueDetail.actions')"
        >
          <button
            v-if="issue.data.value.status !== 'resolved'"
            class="button button--primary"
            type="button"
            :disabled="lifecycle.isPending.value"
            @click="lifecycle.mutate('resolve')"
          >
            <AppIcon name="success" :size="16" />
            {{ $t('issueDetail.resolve') }}
          </button>
          <button
            v-if="issue.data.value.status !== 'ignored'"
            class="button button--secondary"
            type="button"
            :disabled="lifecycle.isPending.value"
            @click="lifecycle.mutate('ignore')"
          >
            <AppIcon name="blocked" :size="16" />
            {{ $t('issueDetail.ignore') }}
          </button>
          <button
            v-if="issue.data.value.status !== 'open'"
            class="button button--secondary"
            type="button"
            :disabled="lifecycle.isPending.value"
            @click="lifecycle.mutate('reopen')"
          >
            <AppIcon name="refresh" :size="16" />
            {{ $t('issueDetail.reopen') }}
          </button>
        </div>
        <p v-else class="permission-note">{{ $t('issueDetail.readOnly') }}</p>
      </header>
      <p v-if="mutationNotice" class="success-notice" role="status">{{ mutationNotice }}</p>
      <ApiErrorPanel
        v-if="lifecycle.error.value"
        :error="lifecycle.error.value"
        :title="$t('issueDetail.changeFailed')"
      />

      <div class="metric-grid">
        <article>
          <span>{{ $t('issueDetail.events') }}</span>
          <strong>{{ issue.data.value.occurrence_count.toLocaleString(locale) }}</strong>
          <small v-if="issue.data.value.occurrence_count_approximate">{{
            $t('common.approximate')
          }}</small>
        </article>
        <article>
          <span>{{ $t('issueDetail.firstSeen') }}</span>
          <strong>{{ formatTime(issue.data.value.first_seen) }}</strong>
        </article>
        <article>
          <span>{{ $t('issueDetail.lastSeen') }}</span>
          <strong>{{ formatTime(issue.data.value.last_seen) }}</strong>
        </article>
        <article>
          <span>{{ $t('issueDetail.lastRelease') }}</span>
          <strong>{{ issue.data.value.last_release || $t('common.notReported') }}</strong>
        </article>
        <article v-if="issue.data.value.regression">
          <span>{{ $t('issueDetail.latestRegression') }}</span>
          <strong>{{
            issue.data.value.regression.release || $t('issueDetail.releaseNotReported')
          }}</strong>
          <small>{{ formatTime(issue.data.value.regression.time) }}</small>
        </article>
      </div>

      <section class="panel" aria-labelledby="frequency-heading">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('issueDetail.statistics') }}</p>
            <h2 id="frequency-heading">{{ $t('issueDetail.frequency') }}</h2>
          </div>
          <span class="muted">{{ $t('issueDetail.approximateCounts') }}</span>
        </div>
        <LoadingPanel
          v-if="statistics.isPending.value"
          :label="$t('issueDetail.loadingStatistics')"
        />
        <ApiErrorPanel
          v-else-if="statistics.error.value"
          :error="statistics.error.value"
          @retry="statistics.refetch()"
        />
        <div v-else-if="statistics.data.value?.items.length" class="stat-bars">
          <div
            v-for="bucket in statistics.data.value.items"
            :key="bucket.bucket_start"
            class="stat-bar"
            :title="`${formatTime(bucket.bucket_start)}: ${bucket.occurrence_count}`"
          >
            <span
              :style="{
                height: `${Math.max(4, Math.min(100, bucket.occurrence_count * 8))}%`,
              }"
            ></span>
          </div>
        </div>
        <p v-else class="muted">{{ $t('issueDetail.noStatistics') }}</p>
      </section>

      <div class="detail-columns">
        <section class="panel">
          <div class="section-heading">
            <div>
              <p class="eyebrow">{{ $t('issueDetail.evidence') }}</p>
              <h2>{{ $t('issueDetail.recentEvents') }}</h2>
            </div>
          </div>
          <LoadingPanel v-if="events.isPending.value" :label="$t('issueDetail.loadingEvents')" />
          <ApiErrorPanel
            v-else-if="events.error.value"
            :error="events.error.value"
            @retry="events.refetch()"
          />
          <div v-else class="compact-list">
            <RouterLink
              v-for="event in events.data.value?.items"
              :key="event.event_id"
              :to="`/events/${event.event_id}`"
            >
              <span class="level-dot" :class="`level-dot--${event.level}`"></span>
              <span>
                <strong>{{ event.platform }} · {{ $t(`status.${event.level}`) }}</strong>
                <small>{{ formatTime(event.occurred_at) }}</small>
              </span>
              <code>{{ event.event_id.slice(0, 8) }}</code>
            </RouterLink>
          </div>
        </section>
        <section class="panel">
          <div class="section-heading">
            <div>
              <p class="eyebrow">{{ $t('issueDetail.auditTrail') }}</p>
              <h2>{{ $t('issueDetail.activity') }}</h2>
            </div>
          </div>
          <LoadingPanel
            v-if="activity.isPending.value"
            :label="$t('issueDetail.loadingActivity')"
          />
          <ApiErrorPanel
            v-else-if="activity.error.value"
            :error="activity.error.value"
            @retry="activity.refetch()"
          />
          <ol v-else class="timeline">
            <li v-for="entry in activity.data.value?.items" :key="entry.id">
              <span class="timeline__dot"></span>
              <div>
                <strong>{{
                  $te(`activity.${entry.kind}`) ? $t(`activity.${entry.kind}`) : entry.kind
                }}</strong>
                <p>
                  {{
                    $te(`activity.${entry.actor.kind}`)
                      ? $t(`activity.${entry.actor.kind}`)
                      : entry.actor.kind
                  }}
                  · {{ formatTime(entry.at) }}
                </p>
              </div>
            </li>
            <li v-if="!activity.data.value?.items.length" class="muted">
              {{ $t('issueDetail.noActivity') }}
            </li>
          </ol>
        </section>
      </div>
    </template>
  </section>
</template>
