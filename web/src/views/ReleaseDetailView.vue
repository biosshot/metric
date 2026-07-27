<script setup lang="ts">
import { computed, ref } from 'vue';
import { useRoute } from 'vue-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import { useSessionStore } from '../stores/session';

const route = useRoute();
const session = useSessionStore();
const queryClient = useQueryClient();
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
  if (!value) return 'Not reported';
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <RouterLink class="back-link" to="/releases">
      <AppIcon name="back" :size="16" /> Releases
    </RouterLink>
    <LoadingPanel v-if="release.isPending.value" label="Loading release…" />
    <ApiErrorPanel
      v-else-if="release.error.value"
      :error="release.error.value"
      title="Release could not be loaded"
      @retry="release.refetch()"
    />
    <template v-else-if="release.data.value">
      <header class="page-header release-heading">
        <div>
          <p class="eyebrow">{{ session.selectedProject?.slug }} / release</p>
          <h1>{{ release.data.value.version }}</h1>
          <p>
            {{
              release.data.value.released_at
                ? `Finalized ${timestamp(release.data.value.released_at)}`
                : 'Open and accepting observations'
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
          {{ finalize.isPending.value ? 'Finalizing…' : 'Finalize' }}
        </button>
      </header>

      <ApiErrorPanel
        v-if="finalize.error.value"
        :error="finalize.error.value"
        title="Release was not finalized"
      />

      <div class="summary-grid">
        <article class="panel summary-card">
          <span>First Error</span><strong>{{ timestamp(release.data.value.first_seen) }}</strong>
        </article>
        <article class="panel summary-card">
          <span>Last Error</span><strong>{{ timestamp(release.data.value.last_seen) }}</strong>
        </article>
        <article class="panel summary-card">
          <span>Deployments</span><strong>{{ deploys.data.value?.items.length ?? 0 }}</strong>
        </article>
        <article class="panel summary-card summary-card--success">
          <span>Crash-free sessions</span>
          <strong>{{ healthSummary.crashFreeSessions.toFixed(2) }}%</strong>
        </article>
        <article class="panel summary-card summary-card--info">
          <span>Crash-free users</span>
          <strong>{{ healthSummary.crashFreeUsers.toFixed(2) }}%</strong>
        </article>
      </div>

      <nav class="release-signal-links" aria-label="Related signals">
        <RouterLink
          :to="{ path: '/issues', query: { q: `release:${release.data.value.version}` } }"
        >
          <AppIcon name="bug" :size="16" /> Errors
        </RouterLink>
        <RouterLink :to="{ path: '/logs', query: { release: release.data.value.version } }">
          <AppIcon name="logs" :size="16" /> Logs
        </RouterLink>
        <RouterLink :to="{ path: '/traces', query: { release: release.data.value.version } }">
          <AppIcon name="traces" :size="16" /> Spans
        </RouterLink>
      </nav>

      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Release Health</p>
            <h2>Application sessions</h2>
          </div>
          <span class="muted">{{ healthSummary.sessions }} sessions · users approximate</span>
        </div>
        <LoadingPanel v-if="health.isPending.value" label="Loading Release Health…" />
        <ApiErrorPanel
          v-else-if="health.error.value"
          :error="health.error.value"
          title="Release Health could not be loaded"
          @retry="health.refetch()"
        />
        <EmptyState
          v-else-if="!health.data.value?.items.length"
          icon="release"
          title="No application sessions"
          description="Send a Session lifecycle from an SDK configured with this Release."
        />
        <div v-else class="table-scroll">
          <table class="data-table">
            <thead>
              <tr>
                <th>Hour</th>
                <th>Environment</th>
                <th>Sessions</th>
                <th>Crashed</th>
                <th>Crash-free</th>
                <th>Crash-free users</th>
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
                <td>{{ bucket.sessions }}</td>
                <td>{{ bucket.crashed }}</td>
                <td>{{ bucket.crash_free_sessions.toFixed(2) }}%</td>
                <td>≈ {{ bucket.crash_free_users.toFixed(2) }}%</td>
              </tr>
            </tbody>
          </table>
        </div>
        <p v-if="health.data.value" class="muted health-footnote">
          User counts use a {{ health.data.value.user_sketch_bytes }} byte mergeable sketch (about
          {{ health.data.value.user_sketch_standard_error_percent }}% standard error, saturation
          near {{ health.data.value.user_sketch_saturation_estimate }} users per hour).
        </p>
      </section>

      <div class="release-columns">
        <section class="panel">
          <div class="section-heading">
            <div>
              <p class="eyebrow">Issues</p>
              <h2>New in this release</h2>
            </div>
          </div>
          <LoadingPanel v-if="newIssues.isPending.value" label="Loading new Issues…" />
          <ApiErrorPanel
            v-else-if="newIssues.error.value"
            :error="newIssues.error.value"
            title="New Issues could not be loaded"
          />
          <EmptyState
            v-else-if="!newIssues.data.value?.items.length"
            icon="success"
            title="No new Issues"
            description="No Issue currently has this as its first Release."
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
              <p class="eyebrow">Regressions</p>
              <h2>Regressed in this release</h2>
            </div>
          </div>
          <LoadingPanel v-if="regressedIssues.isPending.value" label="Loading regressions…" />
          <ApiErrorPanel
            v-else-if="regressedIssues.error.value"
            :error="regressedIssues.error.value"
            title="Regressions could not be loaded"
          />
          <EmptyState
            v-else-if="!regressedIssues.data.value?.items.length"
            icon="success"
            title="No latest regressions"
            description="No Issue's latest regression points at this Release."
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
            <p class="eyebrow">Deploy timeline</p>
            <h2>Environments</h2>
          </div>
        </div>
        <ApiErrorPanel
          v-if="createDeploy.error.value"
          :error="createDeploy.error.value"
          title="Deploy was not recorded"
        />
        <form
          v-if="session.has('release:write')"
          class="inline-form release-deploy-form"
          @submit.prevent="createDeploy.mutate()"
        >
          <label>
            Environment
            <input v-model.trim="environment" required maxlength="64" />
          </label>
          <label>
            Name <span class="muted">(optional)</span>
            <input v-model.trim="deployName" maxlength="200" placeholder="Production rollout" />
          </label>
          <button
            class="button button--primary"
            type="submit"
            :disabled="!environment || createDeploy.isPending.value"
          >
            <AppIcon name="deploy" :size="16" />
            {{ createDeploy.isPending.value ? 'Recording…' : 'Record deploy' }}
          </button>
        </form>
        <LoadingPanel v-if="deploys.isPending.value" label="Loading deploys…" />
        <EmptyState
          v-else-if="!deploys.data.value?.items.length"
          icon="deploy"
          title="No deploys recorded"
          description="Record when this Release reaches an environment."
        />
        <div v-else class="deploy-timeline">
          <article v-for="deploy in deploys.data.value?.items" :key="deploy.id">
            <span class="deploy-timeline__dot"><AppIcon name="deploy" :size="15" /></span>
            <div>
              <strong>{{ deploy.environment }}</strong>
              <span>{{ deploy.name || 'Deployment' }} · {{ timestamp(deploy.started_at) }}</span>
            </div>
          </article>
        </div>
      </section>

      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Automation</p>
            <h2>sentry-cli</h2>
          </div>
        </div>
        <CodeBlock :code="cli" language="powershell" title="Release workflow" />
      </section>
    </template>
  </section>
</template>
