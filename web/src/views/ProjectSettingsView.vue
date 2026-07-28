<script setup lang="ts">
import { computed, reactive, ref, watch } from 'vue';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import type {
  InboundFilterField,
  InboundFilterOperation,
  InboundFilterRule,
  InboundFilterSignal,
  ProjectPolicy,
} from '../api/types';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const queryClient = useQueryClient();
const projectId = computed(() => session.selectedProjectId ?? '');
const newKeyLabel = ref('');
const notice = ref('');
const deleteConfirmation = ref('');
const ipPolicyOptions: SelectOption[] = [
  {
    value: 'hmac',
    label: 'HMAC pseudonymization',
    description: 'Recommended for investigation without retaining the original address.',
    icon: 'shield',
  },
  { value: 'remove', label: 'Remove completely', icon: 'blocked' },
  { value: 'truncate', label: 'Truncate address', icon: 'shield' },
  { value: 'keep', label: 'Keep original address', icon: 'view' },
];
const filterSignalOptions: SelectOption[] = [
  { value: 'error', label: 'Error Event', icon: 'bug' },
  { value: 'log', label: 'Structured Log', icon: 'logs' },
  { value: 'transaction', label: 'Transaction', icon: 'activity' },
  { value: 'span', label: 'Span', icon: 'traces' },
];
const filterOperationOptions: SelectOption[] = [
  { value: 'exact', label: 'Equals' },
  { value: 'prefix', label: 'Starts with' },
  { value: 'suffix', label: 'Ends with' },
  { value: 'contains', label: 'Contains' },
  { value: 'glob', label: 'Glob' },
];
const commonFilterFields: SelectOption[] = [
  { value: 'release', label: 'Release' },
  { value: 'environment', label: 'Environment' },
  { value: 'service', label: 'Service' },
];
const signalFilterFields: Record<InboundFilterSignal, SelectOption[]> = {
  error: [
    ...commonFilterFields,
    { value: 'message', label: 'Normalized message' },
    { value: 'exception_type', label: 'Exception type' },
    { value: 'logger', label: 'Logger' },
    { value: 'request_host', label: 'Request host' },
    { value: 'request_path', label: 'Request path' },
  ],
  log: [
    ...commonFilterFields,
    { value: 'message', label: 'Normalized message' },
    { value: 'severity', label: 'Severity' },
  ],
  transaction: [
    ...commonFilterFields,
    { value: 'name', label: 'Name' },
    { value: 'operation', label: 'Operation' },
    { value: 'status', label: 'Status' },
    { value: 'duration', label: 'Duration (ms)' },
  ],
  span: [
    ...commonFilterFields,
    { value: 'name', label: 'Name' },
    { value: 'operation', label: 'Operation' },
    { value: 'status', label: 'Status' },
    { value: 'duration', label: 'Duration (ms)' },
  ],
};

const project = useQuery({
  queryKey: computed(() => ['project', projectId.value]),
  queryFn: () => api.project(projectId.value),
});
const keys = useQuery({
  queryKey: computed(() => ['project-keys', projectId.value]),
  queryFn: () => api.keys(projectId.value),
});
const capabilities = useQuery({
  queryKey: ['capabilities'],
  queryFn: api.capabilities,
});
const deletion = useQuery({
  queryKey: computed(() => ['project-deletion', projectId.value]),
  queryFn: () => api.projectDeletionStatus(projectId.value),
  enabled: computed(() => ['pending_delete', 'purging'].includes(project.data.value?.state ?? '')),
  refetchInterval: (query) =>
    query.state.data?.phase === 'pending_grace' || query.state.data?.phase === 'purging'
      ? 2_000
      : false,
});

const policy = reactive<ProjectPolicy>({
  revision: 0,
  ip_policy: 'hmac',
  items: {
    error: true,
    client_report: true,
    log: true,
    transaction: true,
    span: true,
    feedback: true,
    check_in: true,
    metric: true,
  },
  limits: { max_event_bytes: 1_048_576, max_events_per_second: null, burst: null },
  inbound_filters: [],
});

watch(
  () => project.data.value?.policy,
  (value) => {
    if (!value) return;
    policy.revision = value.revision;
    policy.ip_policy = value.ip_policy;
    policy.items = { ...value.items };
    policy.limits = { ...value.limits };
    policy.inbound_filters = value.inbound_filters.map((rule) => ({ ...rule }));
  },
  { immediate: true },
);

function setIpPolicy(value: string): void {
  policy.ip_policy = value as ProjectPolicy['ip_policy'];
}

function filterFields(signal: InboundFilterSignal): SelectOption[] {
  return signalFilterFields[signal];
}

function setFilterSignal(rule: InboundFilterRule, value: string): void {
  rule.signal = value as InboundFilterSignal;
  const accepted = filterFields(rule.signal);
  if (!accepted.some((field) => field.value === rule.field)) {
    rule.field = accepted[0].value as InboundFilterField;
  }
}

function setFilterField(rule: InboundFilterRule, value: string): void {
  rule.field = value as InboundFilterField;
  if (rule.field === 'duration') rule.operation = 'exact';
}

function setFilterOperation(rule: InboundFilterRule, value: string): void {
  rule.operation = value as InboundFilterOperation;
}

function filterOperations(field: InboundFilterField): SelectOption[] {
  return field === 'duration' ? filterOperationOptions.slice(0, 1) : filterOperationOptions;
}

function addInboundFilter(): void {
  policy.inbound_filters.push({
    signal: 'error',
    field: 'message',
    operation: 'contains',
    pattern: '',
  });
}

function removeInboundFilter(index: number): void {
  policy.inbound_filters.splice(index, 1);
}

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
const requestDeletion = useMutation({
  mutationFn: () =>
    api.requestProjectDeletion(
      projectId.value,
      deleteConfirmation.value,
      crypto.randomUUID().replaceAll('-', ''),
    ),
  onSuccess: async (value) => {
    queryClient.setQueryData(['project-deletion', projectId.value], value);
    notice.value = 'Deletion is scheduled. Ingestion is fenced immediately.';
    await Promise.all([project.refetch(), keys.refetch(), session.refreshProjects()]);
  },
});
const cancelDeletion = useMutation({
  mutationFn: () => {
    const operationId = deletion.data.value?.operation_id;
    if (!operationId) throw new Error('Deletion operation is not loaded.');
    return api.cancelProjectDeletion(projectId.value, operationId);
  },
  onSuccess: async (value) => {
    queryClient.setQueryData(['project-deletion', projectId.value], value);
    deleteConfirmation.value = '';
    notice.value = 'Deletion was cancelled. Previously active DSN keys are active again.';
    await Promise.all([project.refetch(), keys.refetch(), session.refreshProjects()]);
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
      <div>
        <BaseSelect
          :model-value="policy.ip_policy"
          :options="ipPolicyOptions"
          label="IP address handling"
          :disabled="!session.has('project:admin')"
          @update:model-value="setIpPolicy"
        />
        <small class="field-help">The policy is applied before durable Event storage.</small>
      </div>
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
        <label class="check-control">
          <input
            v-model="policy.items.log"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span><strong>Structured Logs</strong><small>Accept SDK log records.</small></span>
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.transaction"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span><strong>Transactions</strong><small>Accept root performance segments.</small></span>
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.span"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span><strong>Spans</strong><small>Accept child and standalone spans.</small></span>
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.feedback"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span><strong>User Feedback</strong><small>Accept Feedback SDK reports.</small></span>
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.check_in"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span><strong>Cron check-ins</strong><small>Accept scheduled-job check-ins.</small></span>
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.metric"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span>
            <strong>Application Metrics</strong>
            <small>Accept bounded SDK counters, gauges, and distributions.</small>
          </span>
        </label>
      </div>
      <section class="inbound-filter-section">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Before durable storage</p>
            <h3>Inbound filters</h3>
            <p>
              Matching signals are acknowledged and discarded before MongoDB, attachments, or
              BlobStore writes.
            </p>
          </div>
          <button
            v-if="session.has('project:admin')"
            class="button button--secondary"
            type="button"
            :disabled="policy.inbound_filters.length >= 32"
            @click="addInboundFilter"
          >
            <AppIcon name="plus" :size="16" />
            Add filter
          </button>
        </div>
        <div v-if="policy.inbound_filters.length === 0" class="empty-inline">
          <AppIcon name="filter" :size="18" />
          <span>No inbound filters. Every enabled signal follows its normal durable path.</span>
        </div>
        <div v-else class="inbound-filter-list">
          <article
            v-for="(rule, index) in policy.inbound_filters"
            :key="index"
            class="inbound-filter-rule"
          >
            <BaseSelect
              :model-value="rule.signal"
              :options="filterSignalOptions"
              label="Signal"
              :disabled="!session.has('project:admin')"
              @update:model-value="setFilterSignal(rule, $event)"
            />
            <BaseSelect
              :model-value="rule.field"
              :options="filterFields(rule.signal)"
              label="Field"
              :disabled="!session.has('project:admin')"
              @update:model-value="setFilterField(rule, $event)"
            />
            <BaseSelect
              :model-value="rule.operation"
              :options="filterOperations(rule.field)"
              label="Match"
              :disabled="!session.has('project:admin')"
              @update:model-value="setFilterOperation(rule, $event)"
            />
            <label>
              Pattern
              <input
                v-model="rule.pattern"
                maxlength="256"
                required
                autocomplete="off"
                :placeholder="
                  rule.field === 'duration' ? 'Milliseconds, for example 5000' : 'Value'
                "
                :disabled="!session.has('project:admin')"
              />
            </label>
            <button
              v-if="session.has('project:admin')"
              class="icon-button inbound-filter-rule__remove"
              type="button"
              aria-label="Remove inbound filter"
              @click="removeInboundFilter(index)"
            >
              <AppIcon name="delete" :size="16" />
            </button>
          </article>
        </div>
        <small class="field-help">
          Up to 32 rules, 256 bytes per pattern. Matching is case-sensitive; glob supports
          <code>*</code> and <code>?</code>. Duration uses exact integer milliseconds.
        </small>
      </section>
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
        <AppIcon :name="savePolicy.isPending.value ? 'loading' : 'save'" :size="16" />
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
            <AppIcon name="blocked" :size="16" />
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
          <AppIcon name="plus" :size="16" />
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
        <h2>Automated retention</h2>
        <LoadingPanel
          v-if="capabilities.isPending.value"
          label="Loading effective retention policy…"
        />
        <ApiErrorPanel
          v-else-if="capabilities.error.value"
          :error="capabilities.error.value"
          title="Retention policy could not be loaded"
          @retry="capabilities.refetch()"
        />
        <div v-else-if="capabilities.data.value?.retention">
          <p>
            Raw Events are retained for
            <strong>{{ capabilities.data.value.retention.events_days }} days</strong>. Hourly Issue
            statistics are retained for
            <strong>{{ capabilities.data.value.retention.issue_stats_hourly_days }} days</strong>.
          </p>
          <p>
            Event age uses server receipt time. Policy reductions are applied gradually in bounded
            maintenance batches, and pending Events are protected from retention deletion.
          </p>
        </div>
        <p v-else>Automated retention is disabled in this build.</p>
      </div>
      <StatusBadge :status="capabilities.data.value?.retention ? 'active' : 'unavailable'" />
    </section>

    <section v-if="session.has('project:admin')" class="panel danger-zone">
      <div class="section-heading">
        <div>
          <p class="eyebrow">Danger zone</p>
          <h2>Delete project</h2>
        </div>
        <StatusBadge :status="project.data.value?.state ?? 'unavailable'" />
      </div>
      <template
        v-if="project.data.value?.state === 'active' || project.data.value?.state === 'disabled'"
      >
        <p>
          Deletion immediately blocks every active DSN. Purge starts after the grace period and then
          cannot be cancelled. Audit records are retained.
        </p>
        <div class="destructive-confirmation">
          <label>
            <span
              >Type <code>{{ project.data.value.slug }}</code> to confirm</span
            >
            <input
              v-model.trim="deleteConfirmation"
              autocomplete="off"
              :placeholder="project.data.value.slug"
            />
          </label>
          <button
            class="button button--danger"
            type="button"
            :disabled="
              requestDeletion.isPending.value || deleteConfirmation !== project.data.value.slug
            "
            @click="requestDeletion.mutate()"
          >
            <AppIcon :name="requestDeletion.isPending.value ? 'loading' : 'delete'" :size="16" />
            {{ requestDeletion.isPending.value ? 'Scheduling…' : 'Schedule project deletion' }}
          </button>
        </div>
      </template>
      <template v-else-if="project.data.value?.state === 'pending_delete'">
        <LoadingPanel v-if="deletion.isPending.value" label="Loading deletion status…" />
        <ApiErrorPanel
          v-else-if="deletion.error.value"
          :error="deletion.error.value"
          title="Deletion status could not be loaded"
          @retry="deletion.refetch()"
        />
        <div v-else-if="deletion.data.value">
          <p>
            Purge is scheduled for
            <strong>{{ new Date(deletion.data.value.purge_after).toLocaleString() }}</strong
            >. Operation <code>{{ deletion.data.value.operation_id }}</code
            >.
          </p>
          <button
            class="button button--secondary"
            type="button"
            :disabled="cancelDeletion.isPending.value"
            @click="cancelDeletion.mutate()"
          >
            <AppIcon name="back" :size="16" />
            {{ cancelDeletion.isPending.value ? 'Cancelling…' : 'Cancel deletion' }}
          </button>
        </div>
      </template>
      <p v-else-if="project.data.value?.state === 'purging'">
        Purge is running in bounded batches and can no longer be cancelled.
      </p>
      <ApiErrorPanel
        v-if="requestDeletion.error.value || cancelDeletion.error.value"
        :error="requestDeletion.error.value || cancelDeletion.error.value"
        title="Project deletion operation failed"
      />
    </section>
  </section>
</template>
