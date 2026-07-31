<script setup lang="ts">
import { computed, nextTick, reactive, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { useRoute } from 'vue-router';
import { api } from '../api/client';
import type {
  InboundFilterField,
  InboundFilterOperation,
  InboundFilterRule,
  InboundFilterSignal,
  ProjectPolicy,
} from '../api/types';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const route = useRoute();
const queryClient = useQueryClient();
const { locale, t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const canAdministerProject = computed(() => session.has('project:admin'));
const newKeyLabel = ref('');
const notice = ref('');
const deleteConfirmation = ref('');
const ipPolicyOptions = computed<SelectOption[]>(() => [
  {
    value: 'hmac',
    label: t('onboarding.ipHmac'),
    description: t('onboarding.ipHmacDescription'),
    icon: 'shield',
  },
  { value: 'remove', label: t('onboarding.ipRemove'), icon: 'blocked' },
  { value: 'truncate', label: t('onboarding.ipTruncate'), icon: 'shield' },
  { value: 'keep', label: t('onboarding.ipKeep'), icon: 'view' },
]);
const filterSignalOptions = computed<SelectOption[]>(() => [
  { value: 'error', label: t('projectSettings.errorEvent'), icon: 'bug' },
  { value: 'log', label: t('projectSettings.structuredLog'), icon: 'logs' },
  { value: 'transaction', label: t('projectSettings.transaction'), icon: 'activity' },
  { value: 'span', label: t('projectSettings.span'), icon: 'traces' },
]);
const filterOperationOptions = computed<SelectOption[]>(() => [
  { value: 'exact', label: t('projectSettings.equals') },
  { value: 'prefix', label: t('projectSettings.startsWith') },
  { value: 'suffix', label: t('projectSettings.endsWith') },
  { value: 'contains', label: t('projectSettings.contains') },
  { value: 'glob', label: t('projectSettings.glob') },
]);
const signalFilterFields = computed<Record<InboundFilterSignal, SelectOption[]>>(() => {
  const common: SelectOption[] = [
    { value: 'release', label: t('projectSettings.release') },
    { value: 'environment', label: t('projectSettings.environment') },
    { value: 'service', label: t('projectSettings.service') },
  ];
  const spanFields: SelectOption[] = [
    ...common,
    { value: 'name', label: t('projectSettings.name') },
    { value: 'operation', label: t('projectSettings.operation') },
    { value: 'status', label: t('projectSettings.status') },
    { value: 'duration', label: t('projectSettings.duration') },
  ];
  return {
    error: [
      ...common,
      { value: 'message', label: t('projectSettings.normalizedMessage') },
      { value: 'exception_type', label: t('projectSettings.exceptionType') },
      { value: 'logger', label: t('projectSettings.logger') },
      { value: 'request_host', label: t('projectSettings.requestHost') },
      { value: 'request_path', label: t('projectSettings.requestPath') },
    ],
    log: [
      ...common,
      { value: 'message', label: t('projectSettings.normalizedMessage') },
      { value: 'severity', label: t('projectSettings.severity') },
    ],
    transaction: spanFields,
    span: spanFields,
  };
});

const project = useQuery({
  queryKey: computed(() => ['project', projectId.value]),
  queryFn: () => api.project(projectId.value),
});
const keys = useQuery({
  queryKey: computed(() => ['project-keys', projectId.value]),
  queryFn: () => api.keys(projectId.value),
  enabled: canAdministerProject,
});
const capabilities = useQuery({
  queryKey: ['capabilities'],
  queryFn: api.capabilities,
});
watch(
  [() => route.hash, () => project.data.value],
  async ([hash, projectData]) => {
    if (!hash || !projectData) return;
    await nextTick();
    await new Promise<void>((resolve) => window.requestAnimationFrame(() => resolve()));
    document.querySelector(hash)?.scrollIntoView({ block: 'start' });
  },
  { immediate: true, flush: 'post' },
);
const deletion = useQuery({
  queryKey: computed(() => ['project-deletion', projectId.value]),
  queryFn: () => api.projectDeletionStatus(projectId.value),
  enabled: computed(
    () =>
      canAdministerProject.value &&
      ['pending_delete', 'purging'].includes(project.data.value?.state ?? ''),
  ),
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
    replay: false,
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
  return signalFilterFields.value[signal];
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
  return field === 'duration'
    ? filterOperationOptions.value.slice(0, 1)
    : filterOperationOptions.value;
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
    notice.value = t('projectSettings.policySaved');
    await queryClient.invalidateQueries({ queryKey: ['project', projectId.value] });
  },
});
const createKey = useMutation({
  mutationFn: () => api.createKey(projectId.value, newKeyLabel.value),
  onSuccess: async () => {
    newKeyLabel.value = '';
    notice.value = t('projectSettings.keyCreated');
    await queryClient.invalidateQueries({ queryKey: ['project-keys', projectId.value] });
  },
});
const disableKey = useMutation({
  mutationFn: (key: string) => api.disableKey(projectId.value, key),
  onSuccess: async () => {
    notice.value = t('projectSettings.keyDisabled');
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
    notice.value = t('projectSettings.deletionScheduled');
    await Promise.all([project.refetch(), keys.refetch(), session.refreshProjects()]);
  },
});
const cancelDeletion = useMutation({
  mutationFn: () => {
    const operationId = deletion.data.value?.operation_id;
    if (!operationId) throw new Error(t('projectSettings.deletionNotLoaded'));
    return api.cancelProjectDeletion(projectId.value, operationId);
  },
  onSuccess: async (value) => {
    queryClient.setQueryData(['project-deletion', projectId.value], value);
    deleteConfirmation.value = '';
    notice.value = t('projectSettings.deletionCancelled');
    await Promise.all([project.refetch(), keys.refetch(), session.refreshProjects()]);
  },
});
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.display_name }}</p>
        <h1>{{ $t('projectSettings.title') }}</h1>
        <p>{{ $t('projectSettings.description') }}</p>
      </div>
    </header>
    <nav class="settings-anchor-nav" :aria-label="$t('projectSettings.sections')">
      <a href="#data-policy">
        <AppIcon name="shield" :size="15" />
        {{ $t('projectSettings.dataPolicy') }}
      </a>
      <a v-if="canAdministerProject" href="#dsn-keys">
        <AppIcon name="key" :size="15" />
        {{ $t('projectSettings.dsnKeys') }}
      </a>
      <a href="#retention">
        <AppIcon name="history" :size="15" />
        {{ $t('projectSettings.retention') }}
      </a>
      <a v-if="canAdministerProject" href="#delete-project">
        <AppIcon name="delete" :size="15" />
        {{ $t('projectSettings.deleteProject') }}
      </a>
    </nav>
    <p v-if="notice" class="success-notice" role="status">{{ notice }}</p>
    <div v-if="!canAdministerProject" class="permission-banner">
      {{ $t('projectSettings.readOnly') }}
    </div>
    <LoadingPanel v-if="project.isPending.value" :label="$t('projectSettings.loadingPolicy')" />
    <ApiErrorPanel
      v-else-if="project.error.value"
      :error="project.error.value"
      @retry="project.refetch()"
    />
    <form
      v-else
      id="data-policy"
      class="panel settings-form settings-anchor"
      @submit.prevent="savePolicy.mutate()"
    >
      <div class="section-heading">
        <div>
          <p class="eyebrow">{{ $t('projectSettings.revision', { revision: policy.revision }) }}</p>
          <h2>{{ $t('projectSettings.privacy') }}</h2>
        </div>
      </div>
      <div>
        <BaseSelect
          :model-value="policy.ip_policy"
          :options="ipPolicyOptions"
          :label="$t('onboarding.ipHandling')"
          :disabled="!session.has('project:admin')"
          @update:model-value="setIpPolicy"
        />
        <small class="field-help">{{ $t('projectSettings.ipHelp') }}</small>
      </div>
      <div class="check-grid">
        <label class="check-control">
          <input
            v-model="policy.items.error"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span
            ><strong>{{ $t('projectSettings.errorEvents') }}</strong
            ><small>{{ $t('projectSettings.errorEventsHelp') }}</small></span
          >
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.client_report"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span
            ><strong>{{ $t('projectSettings.clientReports') }}</strong
            ><small>{{ $t('projectSettings.clientReportsHelp') }}</small></span
          >
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.log"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span
            ><strong>{{ $t('projectSettings.structuredLogs') }}</strong
            ><small>{{ $t('projectSettings.structuredLogsHelp') }}</small></span
          >
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.transaction"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span
            ><strong>{{ $t('projectSettings.transactions') }}</strong
            ><small>{{ $t('projectSettings.transactionsHelp') }}</small></span
          >
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.span"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span
            ><strong>{{ $t('projectSettings.spans') }}</strong
            ><small>{{ $t('projectSettings.spansHelp') }}</small></span
          >
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.feedback"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span
            ><strong>{{ $t('projectSettings.feedback') }}</strong
            ><small>{{ $t('projectSettings.feedbackHelp') }}</small></span
          >
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.check_in"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span
            ><strong>{{ $t('projectSettings.checkIns') }}</strong
            ><small>{{ $t('projectSettings.checkInsHelp') }}</small></span
          >
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.metric"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span>
            <strong>{{ $t('projectSettings.metrics') }}</strong>
            <small>{{ $t('projectSettings.metricsHelp') }}</small>
          </span>
        </label>
        <label class="check-control">
          <input
            v-model="policy.items.replay"
            type="checkbox"
            :disabled="!session.has('project:admin')"
          />
          <span>
            <strong>{{ $t('projectSettings.replay') }}</strong>
            <small>{{ $t('projectSettings.replayHelp') }}</small>
          </span>
        </label>
      </div>
      <section class="inbound-filter-section">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('projectSettings.beforeStorage') }}</p>
            <h3>{{ $t('projectSettings.inboundFilters') }}</h3>
            <p>{{ $t('projectSettings.inboundFiltersHelp') }}</p>
          </div>
          <button
            v-if="session.has('project:admin')"
            class="button button--secondary"
            type="button"
            :disabled="policy.inbound_filters.length >= 32"
            @click="addInboundFilter"
          >
            <AppIcon name="plus" :size="16" />
            {{ $t('projectSettings.addFilter') }}
          </button>
        </div>
        <div v-if="policy.inbound_filters.length === 0" class="empty-inline">
          <AppIcon name="filter" :size="18" />
          <span>{{ $t('projectSettings.noFilters') }}</span>
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
              :label="$t('projectSettings.signal')"
              :disabled="!session.has('project:admin')"
              @update:model-value="setFilterSignal(rule, $event)"
            />
            <BaseSelect
              :model-value="rule.field"
              :options="filterFields(rule.signal)"
              :label="$t('projectSettings.field')"
              :disabled="!session.has('project:admin')"
              @update:model-value="setFilterField(rule, $event)"
            />
            <BaseSelect
              :model-value="rule.operation"
              :options="filterOperations(rule.field)"
              :label="$t('projectSettings.match')"
              :disabled="!session.has('project:admin')"
              @update:model-value="setFilterOperation(rule, $event)"
            />
            <label>
              {{ $t('projectSettings.pattern') }}
              <input
                v-model="rule.pattern"
                maxlength="256"
                required
                autocomplete="off"
                :placeholder="
                  rule.field === 'duration'
                    ? $t('projectSettings.durationPlaceholder')
                    : $t('projectSettings.valuePlaceholder')
                "
                :disabled="!session.has('project:admin')"
              />
            </label>
            <button
              v-if="session.has('project:admin')"
              class="icon-button inbound-filter-rule__remove"
              type="button"
              :aria-label="$t('projectSettings.removeFilter')"
              @click="removeInboundFilter(index)"
            >
              <AppIcon name="delete" :size="16" />
            </button>
          </article>
        </div>
        <small class="field-help">
          {{ $t('projectSettings.filterLimits') }}
        </small>
      </section>
      <div class="form-grid form-grid--three">
        <label>
          {{ $t('projectSettings.maxEventBytes') }}
          <input
            v-model.number="policy.limits.max_event_bytes"
            type="number"
            min="1"
            max="20971520"
            :disabled="!session.has('project:admin')"
          />
        </label>
        <label>
          {{ $t('projectSettings.eventsPerSecond') }}
          <input
            v-model.number="policy.limits.max_events_per_second"
            type="number"
            min="1"
            :placeholder="$t('projectSettings.unlimited')"
            :disabled="!session.has('project:admin')"
          />
        </label>
        <label>
          {{ $t('projectSettings.burst') }}
          <input
            v-model.number="policy.limits.burst"
            type="number"
            min="1"
            :placeholder="$t('projectSettings.automatic')"
            :disabled="!session.has('project:admin')"
          />
        </label>
      </div>
      <ApiErrorPanel
        v-if="savePolicy.error.value"
        :error="savePolicy.error.value"
        :title="$t('projectSettings.policyFailed')"
      />
      <button
        v-if="session.has('project:admin')"
        class="button button--primary"
        type="submit"
        :disabled="savePolicy.isPending.value"
      >
        <AppIcon :name="savePolicy.isPending.value ? 'loading' : 'save'" :size="16" />
        {{
          savePolicy.isPending.value
            ? $t('projectSettings.saving')
            : $t('projectSettings.savePolicy')
        }}
      </button>
    </form>

    <section id="dsn-keys" class="panel settings-anchor">
      <div class="section-heading">
        <div>
          <p class="eyebrow">{{ $t('projectSettings.sdkAccess') }}</p>
          <h2>{{ $t('projectSettings.dsnKeys') }}</h2>
        </div>
      </div>
      <EmptyState
        v-if="!canAdministerProject"
        icon="blocked"
        :title="$t('projectSettings.dsnRestricted')"
        :description="$t('projectSettings.dsnRestrictedDescription')"
      />
      <template v-else>
        <LoadingPanel v-if="keys.isPending.value" :label="$t('projectSettings.loadingKeys')" />
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
              {{ $t('projectSettings.disable') }}
            </button>
          </article>
        </div>
        <form
          v-if="session.has('project:admin')"
          class="inline-form"
          @submit.prevent="createKey.mutate()"
        >
          <label>
            {{ $t('projectSettings.newKeyLabel') }}
            <input v-model.trim="newKeyLabel" maxlength="64" required />
          </label>
          <button
            class="button button--secondary"
            type="submit"
            :disabled="createKey.isPending.value"
          >
            <AppIcon name="plus" :size="16" />
            {{ $t('projectSettings.createKey') }}
          </button>
        </form>
        <ApiErrorPanel
          v-if="createKey.error.value || disableKey.error.value"
          :error="createKey.error.value || disableKey.error.value"
          :title="$t('projectSettings.keyFailed')"
        />
      </template>
    </section>

    <section id="retention" class="panel unavailable-setting settings-anchor">
      <div>
        <p class="eyebrow">{{ $t('projectSettings.retention') }}</p>
        <h2>{{ $t('projectSettings.automatedRetention') }}</h2>
        <LoadingPanel
          v-if="capabilities.isPending.value"
          :label="$t('projectSettings.loadingRetention')"
        />
        <ApiErrorPanel
          v-else-if="capabilities.error.value"
          :error="capabilities.error.value"
          :title="$t('projectSettings.retentionFailed')"
          @retry="capabilities.refetch()"
        />
        <div v-else-if="capabilities.data.value?.retention">
          <i18n-t keypath="projectSettings.retentionDurations" tag="p" scope="global">
            <template #events>
              <strong>{{
                capabilities.data.value.retention.events_days.toLocaleString(locale)
              }}</strong>
            </template>
            <template #statistics>
              <strong>{{
                capabilities.data.value.retention.issue_stats_hourly_days.toLocaleString(locale)
              }}</strong>
            </template>
          </i18n-t>
          <p>{{ $t('projectSettings.retentionHelp') }}</p>
        </div>
        <p v-else>{{ $t('projectSettings.retentionDisabled') }}</p>
      </div>
      <StatusBadge :status="capabilities.data.value?.retention ? 'active' : 'unavailable'" />
    </section>

    <section
      v-if="session.has('project:admin')"
      id="delete-project"
      class="panel danger-zone settings-anchor"
    >
      <div class="section-heading">
        <div>
          <p class="eyebrow">{{ $t('projectSettings.dangerZone') }}</p>
          <h2>{{ $t('projectSettings.deleteProject') }}</h2>
        </div>
        <StatusBadge :status="project.data.value?.state ?? 'unavailable'" />
      </div>
      <template
        v-if="project.data.value?.state === 'active' || project.data.value?.state === 'disabled'"
      >
        <p>{{ $t('projectSettings.deletionHelp') }}</p>
        <div class="destructive-confirmation">
          <label>
            <i18n-t keypath="projectSettings.typeToConfirm" tag="span" scope="global">
              <template #slug
                ><code>{{ project.data.value.slug }}</code></template
              >
            </i18n-t>
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
            {{
              requestDeletion.isPending.value
                ? $t('projectSettings.scheduling')
                : $t('projectSettings.scheduleDeletion')
            }}
          </button>
        </div>
      </template>
      <template v-else-if="project.data.value?.state === 'pending_delete'">
        <LoadingPanel
          v-if="deletion.isPending.value"
          :label="$t('projectSettings.loadingDeletion')"
        />
        <ApiErrorPanel
          v-else-if="deletion.error.value"
          :error="deletion.error.value"
          :title="$t('projectSettings.deletionLoadFailed')"
          @retry="deletion.refetch()"
        />
        <div v-else-if="deletion.data.value">
          <i18n-t keypath="projectSettings.purgeScheduled" tag="p" scope="global">
            <template #time>
              <strong>{{
                new Date(deletion.data.value.purge_after).toLocaleString(locale)
              }}</strong>
            </template>
            <template #operation>
              <code>
                {{ deletion.data.value.operation_id }}
              </code>
            </template>
          </i18n-t>
          <button
            class="button button--secondary"
            type="button"
            :disabled="cancelDeletion.isPending.value"
            @click="cancelDeletion.mutate()"
          >
            <AppIcon name="back" :size="16" />
            {{
              cancelDeletion.isPending.value
                ? $t('projectSettings.cancelling')
                : $t('projectSettings.cancelDeletion')
            }}
          </button>
        </div>
      </template>
      <p v-else-if="project.data.value?.state === 'purging'">
        {{ $t('projectSettings.purging') }}
      </p>
      <ApiErrorPanel
        v-if="requestDeletion.error.value || cancelDeletion.error.value"
        :error="requestDeletion.error.value || cancelDeletion.error.value"
        :title="$t('projectSettings.deletionFailed')"
      />
    </section>
  </section>
</template>
