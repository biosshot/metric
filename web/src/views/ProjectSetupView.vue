<script setup lang="ts">
import { computed, ref } from 'vue';
import { useQuery } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const projectId = computed(() => session.selectedProjectId ?? '');
const copyNotice = ref('');
const copyError = ref('');
const keys = useQuery({
  queryKey: computed(() => ['project-keys', projectId.value]),
  queryFn: () => api.keys(projectId.value),
});

function dsn(key: string): string {
  return `${window.location.protocol}//${key}@${window.location.host}/${projectId.value}`;
}

async function copy(value: string): Promise<void> {
  copyNotice.value = '';
  copyError.value = '';
  try {
    await navigator.clipboard.writeText(value);
    copyNotice.value = 'DSN copied to the clipboard.';
  } catch {
    copyError.value = 'The browser denied clipboard access. Select and copy the DSN manually.';
  }
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.display_name }}</p>
        <h1>Connect an SDK</h1>
        <p>Use an official Sentry SDK and send Error Events to this Faultkeep project.</p>
      </div>
    </header>
    <div class="setup-grid">
      <section class="panel setup-steps">
        <ol>
          <li>
            <span>1</span>
            <div>
              <h2>Choose an active DSN</h2>
              <p>DSN keys identify this project. They are not personal API tokens.</p>
            </div>
          </li>
          <li>
            <span>2</span>
            <div>
              <h2>Configure your SDK</h2>
              <p>Set the DSN in the official SDK initialization for your language.</p>
            </div>
          </li>
          <li>
            <span>3</span>
            <div>
              <h2>Send a test error</h2>
              <p>
                Processing is asynchronous. The Issue will appear after normalization and grouping.
              </p>
            </div>
          </li>
        </ol>
      </section>
      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Project credentials</p>
            <h2>Available DSNs</h2>
          </div>
          <RouterLink
            v-if="session.has('project:admin')"
            class="button button--secondary"
            to="/project/settings"
          >
            Manage keys
          </RouterLink>
        </div>
        <LoadingPanel v-if="keys.isPending.value" label="Loading project keys…" />
        <ApiErrorPanel
          v-else-if="keys.error.value"
          :error="keys.error.value"
          @retry="keys.refetch()"
        />
        <p v-if="copyNotice" class="success-notice" role="status">{{ copyNotice }}</p>
        <p v-if="copyError" class="permission-banner" role="alert">{{ copyError }}</p>
        <EmptyState
          v-else-if="!keys.data.value?.items.some((key) => key.state === 'active')"
          title="No active DSN"
          description="A project administrator must create or enable a key before an SDK can send events."
        />
        <div v-else class="dsn-list">
          <article
            v-for="key in keys.data.value?.items.filter((item) => item.state === 'active')"
            :key="key.dsn_key"
          >
            <div>
              <strong>{{ key.label }}</strong>
              <StatusBadge :status="key.state" />
            </div>
            <code>{{ dsn(key.dsn_key) }}</code>
            <button class="button button--secondary" type="button" @click="copy(dsn(key.dsn_key))">
              Copy DSN
            </button>
          </article>
        </div>
      </section>
    </div>
    <section class="panel code-examples">
      <p class="eyebrow">Example</p>
      <h2>JavaScript browser SDK</h2>
      <pre><code>import * as Sentry from "@sentry/browser";

Sentry.init({
  dsn: "PASTE_DSN_HERE",
  tracesSampleRate: 0
});</code></pre>
      <p class="info-note">
        Faultkeep currently supports the Error Event path. Transactions, replays, profiles, and
        metrics remain disabled and are reported through capabilities.
      </p>
    </section>
  </section>
</template>
