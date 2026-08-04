<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import { queryLink } from '../lib/queryLinks';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const queryClient = useQueryClient();
const { locale, t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const releaseId = computed(() => String(route.params.releaseId ?? ''));
const environment = ref('production');
const deployName = ref('');
const organization = useQuery({
  queryKey: ['organization'],
  queryFn: api.organization,
});

const release = useQuery({
  queryKey: computed(() => ['release', projectId.value, releaseId.value]),
  queryFn: () => api.release(projectId.value, releaseId.value),
  enabled: computed(() => Boolean(projectId.value && releaseId.value)),
});
const deploys = useQuery({
  queryKey: computed(() => ['release-deploys', projectId.value, releaseId.value]),
  queryFn: () => api.releaseDeploys(projectId.value, releaseId.value),
  enabled: computed(() => Boolean(projectId.value && releaseId.value)),
});
const newIssues = useQuery({
  queryKey: computed(() => ['release-issues', projectId.value, releaseId.value, 'new']),
  queryFn: () => api.releaseIssues(projectId.value, releaseId.value, 'new'),
  enabled: computed(() => Boolean(projectId.value && releaseId.value)),
});
const regressedIssues = useQuery({
  queryKey: computed(() => ['release-issues', projectId.value, releaseId.value, 'regressed']),
  queryFn: () => api.releaseIssues(projectId.value, releaseId.value, 'regressed'),
  enabled: computed(() => Boolean(projectId.value && releaseId.value)),
});
const health = useQuery({
  queryKey: computed(() => ['release-health', projectId.value, releaseId.value]),
  queryFn: () => api.releaseHealth(projectId.value, releaseId.value),
  enabled: computed(() => Boolean(projectId.value && releaseId.value)),
});
const healthSummary = computed(() => {
  const items = health.data.value?.items ?? [];
  const sessions = items.reduce((sum, item) => sum + item.sessions, 0);
  const crashed = items.reduce((sum, item) => sum + item.crashed, 0);
  return {
    sessions,
    crashFreeSessions: sessions ? (100 * (sessions - crashed)) / sessions : 100,
    crashFreeUsers: health.data.value?.crash_free_users ?? 100,
  };
});

const finalize = useMutation({
  mutationFn: () => api.finalizeRelease(projectId.value, releaseId.value),
  onSuccess: async () => {
    await queryClient.invalidateQueries({
      queryKey: ['release', projectId.value, releaseId.value],
    });
    await queryClient.invalidateQueries({ queryKey: ['releases', projectId.value] });
  },
});
const createDeploy = useMutation({
  mutationFn: () =>
    api.createDeploy(projectId.value, releaseId.value, {
      environment: environment.value.trim(),
      name: deployName.value.trim() || undefined,
    }),
  onSuccess: async () => {
    deployName.value = '';
    await queryClient.invalidateQueries({
      queryKey: ['release-deploys', projectId.value, releaseId.value],
    });
  },
});

const cli = computed(() => {
  const organizationSlug = organization.data.value?.slug ?? 'ORG_SLUG';
  const project = session.selectedProject?.slug ?? 'PROJECT';
  const version = release.data.value?.version ?? 'VERSION';
  return [
    `$env:SENTRY_URL="${window.location.origin}/"`,
    '$env:SENTRY_AUTH_TOKEN="YOUR_TOKEN"',
    `sentry-cli releases new --org ${organizationSlug} --project ${project} "${version}"`,
    `sentry-cli releases finalize --org ${organizationSlug} --project ${project} "${version}"`,
    `sentry-cli releases deploys new --org ${organizationSlug} --project ${project} --release "${version}" --env production`,
  ].join('\n');
});

function timestamp(value: string | null): string {
  if (!value) return t('common.notReported');
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <RouterLink class="back-link" to="/releases">
      <AppIcon name="back" :size="16" /> {{ $t('releaseDetail.all') }}
    </RouterLink>
    <LoadingPanel v-if="release.isPending.value" :label="$t('releaseDetail.loading')" />
    <ApiErrorPanel
      v-else-if="release.error.value"
      :error="release.error.value"
      :title="$t('releaseDetail.loadFailed')"
      @retry="release.refetch()"
    />
    <template v-else-if="release.data.value">
      <header class="page-header release-heading">
        <div>
          <p class="eyebrow">
            {{ $t('releaseDetail.eyebrow', { project: session.selectedProject?.slug }) }}
          </p>
          <h1>{{ release.data.value.version }}</h1>
          <p>
            {{
              release.data.value.released_at
                ? $t('releaseDetail.finalized', {
                    time: timestamp(release.data.value.released_at),
                  })
                : $t('releaseDetail.open')
            }}
          </p>
        </div>
        <button
          v-if="session.has('release:write') && !release.data.value.released_at"
          class="button button--primary"
          type="button"
          :disabled="finalize.isPending.value"
          @click="finalize.mutate()"
        >
          <AppIcon name="check" :size="16" />
          {{
            finalize.isPending.value ? $t('releaseDetail.finalizing') : $t('releaseDetail.finalize')
          }}
        </button>
      </header>

      <ApiErrorPanel
        v-if="finalize.error.value"
        :error="finalize.error.value"
        :title="$t('releaseDetail.finalizeFailed')"
      />

      <div class="summary-grid">
        <article class="panel summary-card">
          <span>{{ $t('releaseDetail.firstError') }}</span
          ><strong>{{ timestamp(release.data.value.first_seen) }}</strong>
        </article>
        <article class="panel summary-card">
          <span>{{ $t('releaseDetail.lastError') }}</span
          ><strong>{{ timestamp(release.data.value.last_seen) }}</strong>
        </article>
        <article class="panel summary-card">
          <span>{{ $t('releaseDetail.deployments') }}</span
          ><strong>{{ (deploys.data.value?.items.length ?? 0).toLocaleString(locale) }}</strong>
        </article>
        <article class="panel summary-card summary-card--success">
          <span>{{ $t('releaseDetail.crashFreeSessions') }}</span>
          <strong>{{ healthSummary.crashFreeSessions.toFixed(2) }}%</strong>
        </article>
        <article class="panel summary-card summary-card--info">
          <span>{{ $t('releaseDetail.crashFreeUsers') }}</span>
          <strong>{{ healthSummary.crashFreeUsers.toFixed(2) }}%</strong>
        </article>
      </div>

      <nav class="release-signal-links" :aria-label="$t('releaseDetail.relatedSignals')">
        <RouterLink :to="queryLink('/explore', 'rel', release.data.value.version)">
          <AppIcon name="bug" :size="16" /> {{ $t('releaseDetail.errors') }}
        </RouterLink>
        <RouterLink :to="queryLink('/logs', 'rel', release.data.value.version)">
          <AppIcon name="logs" :size="16" /> {{ $t('releaseDetail.logs') }}
        </RouterLink>
        <RouterLink :to="queryLink('/traces', 'rel', release.data.value.version)">
          <AppIcon name="traces" :size="16" /> {{ $t('releaseDetail.spans') }}
        </RouterLink>
      </nav>

      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('releaseDetail.health') }}</p>
            <h2>{{ $t('releaseDetail.sessionsTitle') }}</h2>
          </div>
          <span class="muted">{{
            $t('releaseDetail.sessionsSummary', {
              count: healthSummary.sessions.toLocaleString(locale),
            })
          }}</span>
        </div>
        <LoadingPanel v-if="health.isPending.value" :label="$t('releaseDetail.healthLoading')" />
        <ApiErrorPanel
          v-else-if="health.error.value"
          :error="health.error.value"
          :title="$t('releaseDetail.healthFailed')"
          @retry="health.refetch()"
        />
        <EmptyState
          v-else-if="!health.data.value?.items.length"
          icon="release"
          :title="$t('releaseDetail.noSessions')"
          :description="$t('releaseDetail.noSessionsDescription')"
        />
        <div v-else class="release-health-table-scroll">
          <table class="release-health-table">
            <thead>
              <tr>
                <th scope="col">{{ $t('releaseDetail.hour') }}</th>
                <th scope="col">{{ $t('releaseDetail.environment') }}</th>
                <th scope="col">{{ $t('releaseDetail.sessions') }}</th>
                <th scope="col">{{ $t('releaseDetail.crashed') }}</th>
                <th scope="col">{{ $t('releaseDetail.crashFree') }}</th>
                <th scope="col">{{ $t('releaseDetail.crashFreeUsers') }}</th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="bucket in health.data.value?.items"
                :key="`${bucket.hour}:${bucket.environment_id}`"
              >
                <td>{{ timestamp(bucket.hour) }}</td>
                <td>
                  <strong>{{ bucket.environment }}</strong>
                </td>
                <td>{{ bucket.sessions.toLocaleString(locale) }}</td>
                <td>{{ bucket.crashed.toLocaleString(locale) }}</td>
                <td>{{ bucket.crash_free_sessions.toFixed(2) }}%</td>
                <td>≈ {{ bucket.crash_free_users.toFixed(2) }}%</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-if="health.data.value" class="muted health-footnote">
          {{
            $t('releaseDetail.healthFootnote', {
              bytes: health.data.value.user_sketch_bytes.toLocaleString(locale),
              error: health.data.value.user_sketch_standard_error_percent,
              saturation: health.data.value.user_sketch_saturation_estimate.toLocaleString(locale),
            })
          }}
        </p>
      </section>

      <div class="release-columns">
        <section class="panel">
          <div class="section-heading">
            <div>
              <p class="eyebrow">{{ $t('releaseDetail.issues') }}</p>
              <h2>{{ $t('releaseDetail.newIssues') }}</h2>
            </div>
          </div>
          <LoadingPanel
            v-if="newIssues.isPending.value"
            :label="$t('releaseDetail.loadingNewIssues')"
          />
          <ApiErrorPanel
            v-else-if="newIssues.error.value"
            :error="newIssues.error.value"
            :title="$t('releaseDetail.newIssuesFailed')"
          />
          <EmptyState
            v-else-if="!newIssues.data.value?.items.length"
            icon="success"
            :title="$t('releaseDetail.noNewIssues')"
            :description="$t('releaseDetail.noNewIssuesDescription')"
          />
          <RouterLink
            v-for="issue in newIssues.data.value?.items"
            v-else
            :key="issue.id"
            class="release-issue"
            :to="`/issues/${issue.id}`"
          >
            <strong>{{ issue.title }}</strong
            ><span>{{ timestamp(issue.first_seen) }}</span>
          </RouterLink>
        </section>

        <section class="panel">
          <div class="section-heading">
            <div>
              <p class="eyebrow">{{ $t('releaseDetail.regressions') }}</p>
              <h2>{{ $t('releaseDetail.regressed') }}</h2>
            </div>
          </div>
          <LoadingPanel
            v-if="regressedIssues.isPending.value"
            :label="$t('releaseDetail.loadingRegressions')"
          />
          <ApiErrorPanel
            v-else-if="regressedIssues.error.value"
            :error="regressedIssues.error.value"
            :title="$t('releaseDetail.regressionsFailed')"
          />
          <EmptyState
            v-else-if="!regressedIssues.data.value?.items.length"
            icon="success"
            :title="$t('releaseDetail.noRegressions')"
            :description="$t('releaseDetail.noRegressionsDescription')"
          />
          <RouterLink
            v-for="issue in regressedIssues.data.value?.items"
            v-else
            :key="issue.id"
            class="release-issue"
            :to="`/issues/${issue.id}`"
          >
            <strong>{{ issue.title }}</strong
            ><span>{{ timestamp(issue.last_seen) }}</span>
          </RouterLink>
        </section>
      </div>

      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('releaseDetail.deployTimeline') }}</p>
            <h2>{{ $t('releaseDetail.environments') }}</h2>
          </div>
        </div>
        <ApiErrorPanel
          v-if="createDeploy.error.value"
          :error="createDeploy.error.value"
          :title="$t('releaseDetail.deployFailed')"
        />
        <form
          v-if="session.has('release:write')"
          class="inline-form release-deploy-form"
          @submit.prevent="createDeploy.mutate()"
        >
          <label>
            {{ $t('releaseDetail.environment') }}
            <input v-model.trim="environment" required maxlength="64" />
          </label>
          <label>
            {{ $t('releaseDetail.name') }}
            <span class="muted">{{ $t('releases.optional') }}</span>
            <input
              v-model.trim="deployName"
              maxlength="200"
              :placeholder="$t('releaseDetail.rolloutPlaceholder')"
            />
          </label>
          <button
            class="button button--primary"
            type="submit"
            :disabled="!environment || createDeploy.isPending.value"
          >
            <AppIcon name="deploy" :size="16" />
            {{
              createDeploy.isPending.value
                ? $t('releaseDetail.recording')
                : $t('releaseDetail.recordDeploy')
            }}
          </button>
        </form>
        <LoadingPanel v-if="deploys.isPending.value" :label="$t('releaseDetail.loadingDeploys')" />
        <EmptyState
          v-else-if="!deploys.data.value?.items.length"
          icon="deploy"
          :title="$t('releaseDetail.noDeploys')"
          :description="$t('releaseDetail.noDeploysDescription')"
        />
        <div v-else class="deploy-timeline">
          <article v-for="deploy in deploys.data.value?.items" :key="deploy.id">
            <span class="deploy-timeline__dot"><AppIcon name="deploy" :size="15" /></span>
            <div>
              <strong>{{ deploy.environment }}</strong>
              <span
                >{{ deploy.name || $t('releaseDetail.deployment') }} ·
                {{ timestamp(deploy.started_at) }}</span
              >
            </div>
          </article>
        </div>
      </section>

      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('releaseDetail.automation') }}</p>
            <h2>sentry-cli</h2>
          </div>
        </div>
        <CodeBlock :code="cli" language="powershell" :title="$t('releaseDetail.workflow')" />
      </section>
    </template>
  </section>
</template>
