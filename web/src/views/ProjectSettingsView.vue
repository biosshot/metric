<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import type { ProjectPolicy } from '../api/types';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const queryClient = useQueryClient();
const projectId = computed(() => session.selectedProjectId ?? '');
const newKeyLabel = ref('');
const notice = ref('');

const project = useQuery({
  queryKey: computed(() => ['project', projectId.value]),
  queryFn: () => api.project(projectId.value),
});
const keys = useQuery({
  queryKey: computed(() => ['project-keys', projectId.value]),
  queryFn: () => api.keys(projectId.value),
});

const policy = reactive<ProjectPolicy>({
  revision: 0,
  ip_policy: 'hmac',
  items: { error: true, client_report: true },
  limits: { max_event_bytes: 1_048_576, max_events_per_second: null, burst: null },
});

watch(
  () => project.data.value?.policy,
  (value) => {
    if (value) Object.assign(policy, structuredClone(value));
  },
  { immediate: true },
);

const savePolicy = useMutation({
  mutationFn: () => api.updatePolicy(projectId.value, policy),
  onSuccess: async (value) => {
    Object.assign(policy, value);
    notice.value = 'Project policy saved.';
    await queryClient.invalidateQueries({ queryKey: ['project', projectId.value] });
  },
});
const createKey = useMutation({
  mutationFn: () => api.createKey(projectId.value, newKeyLabel.value),
  onSuccess: async () => {
    newKeyLabel.value = '';
    notice.value = 'A new DSN key was created.';
    await queryClient.invalidateQueries({ queryKey: ['project-keys', projectId.value] });
  },
});
const disableKey = useMutation({
  mutationFn: (key: string) => api.disableKey(projectId.value, key),
  onSuccess: async () => {
    notice.value = 'The DSN key was disabled. SDKs using it can no longer ingest events.';
    await queryClient.invalidateQueries({ queryKey: ['project-keys', projectId.value] });
  },
});
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.display_name }}</p>
        <h1>Project settings</h1>
        <p>Accepted ingestion, privacy controls, limits, and DSN keys.</p>
      </div>
    </header>
    <p v-if="notice" class="success-notice" role="status">{{ notice }}</p>
    <div v-if="!session.has('project:admin')" class="permission-banner">
      You can inspect these settings, but only a project administrator can change them.
    </div>
    <LoadingPanel v-if="project.isPending.value" label="Loading accepted project policy…" />
    <ApiErrorPanel
      v-else-if="project.error.value"
      :error="project.error.value"
      @retry="project.refetch()"
    />
    <form v-else class="panel settings-form" @submit.prevent="savePolicy.mutate()">
      <div class="section-heading">
        <div>
          <p class="eyebrow">Revision {{ policy.revision }}</p>
          <h2>Privacy and ingestion</h2>
        </div>
      </div>
      <label>
        IP address handling
        <select v-model="policy.ip_policy" :disabled="!session.has('project:admin')">
          <option value="hmac">HMAC pseudonymization (recommended)</option>
          <option value="remove">Remove completely</option>
          <option value="truncate">Truncate address</option>
          <option value="keep">Keep original address</option>
        </select>
        <small>The policy is applied before durable Event storage.</small>
      </label>
      <div class="check-grid">
        <label class="check-control">
          <input
            v-model="policy.items.error"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span><strong>Error Events</strong><small>Accept supported error payloads.</small></span>
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.client_report"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span><strong>Client reports</strong><small>Accept SDK outcome reports.</small></span>
        </label>
      </div>
      <div class="form-grid form-grid--three">
        <label>
          Maximum Event bytes
          <input
            v-model.number="policy.limits.max_event_bytes"
            type="number"
            min="1"
            max="20971520"
            :disabled="!session.has('project:admin')"
          />
        </label>
        <label>
          Events per second
          <input
            v-model.number="policy.limits.max_events_per_second"
            type="number"
            min="1"
            placeholder="Unlimited"
            :disabled="!session.has('project:admin')"
          />
        </label>
        <label>
          Burst
          <input
            v-model.number="policy.limits.burst"
            type="number"
            min="1"
            placeholder="Automatic"
            :disabled="!session.has('project:admin')"
          />
        </label>
      </div>
      <ApiErrorPanel
        v-if="savePolicy.error.value"
        :error="savePolicy.error.value"
        title="Policy was not saved"
      />
      <button
        v-if="session.has('project:admin')"
        class="button button--primary"
        type="submit"
        :disabled="savePolicy.isPending.value"
      >
        {{ savePolicy.isPending.value ? 'Saving…' : 'Save policy' }}
      </button>
    </form>

    <section class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">SDK access</p>
          <h2>DSN keys</h2>
        </div>
      </div>
      <LoadingPanel v-if="keys.isPending.value" label="Loading DSN keys…" />
      <ApiErrorPanel
        v-else-if="keys.error.value"
        :error="keys.error.value"
        @retry="keys.refetch()"
      />
      <div v-else class="key-list">
        <article v-for="key in keys.data.value?.items" :key="key.dsn_key">
          <div>
            <strong>{{ key.label }}</strong>
            <code>{{ key.dsn_key }}</code>
          </div>
          <StatusBadge :status="key.state" />
          <button
            v-if="session.has('project:admin') && key.state === 'active'"
            class="button button--danger"
            type="button"
            :disabled="disableKey.isPending.value"
            @click="disableKey.mutate(key.dsn_key)"
          >
            Disable
          </button>
        </article>
      </div>
      <form
        v-if="session.has('project:admin')"
        class="inline-form"
        @submit.prevent="createKey.mutate()"
      >
        <label>
          New key label
          <input v-model.trim="newKeyLabel" maxlength="64" required />
        </label>
        <button
          class="button button--secondary"
          type="submit"
          :disabled="createKey.isPending.value"
        >
          Create key
        </button>
      </form>
      <ApiErrorPanel
        v-if="createKey.error.value || disableKey.error.value"
        :error="createKey.error.value || disableKey.error.value"
        title="Key operation failed"
      />
    </section>

    <section class="panel unavailable-setting">
      <div>
        <p class="eyebrow">Retention</p>
        <h2>Not available in this build</h2>
        <p>
          Automated retention is deferred to Phase 14. Faultkeep will not pretend that a retention
          value has been saved before the owning scheduler module exists.
        </p>
      </div>
      <StatusBadge status="unavailable" />
    </section>
  </section>
</template>
