<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useMutation } from '@tanstack/vue-query';
import { useRouter } from 'vue-router';
import { api } from '../api/client';
import type { CreateProjectInput } from '../api/types';
import { useSessionStore } from '../stores/session';
import ApiErrorPanel from './ApiErrorPanel.vue';
import AppIcon from './AppIcon.vue';
import BaseSelect, { type SelectOption } from './BaseSelect.vue';
import EmptyState from './EmptyState.vue';
import { suggestedSlug } from '../lib/slug';

const session = useSessionStore();
const router = useRouter();
const { t } = useI18n();
const canCreateProject = computed(() => session.has('organization:admin'));
const firstProject = computed(() => session.projects.length === 0);
const slugWasEdited = ref(false);
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

const project = reactive<CreateProjectInput>({
  display_name: '',
  slug: '',
  ip_policy: 'hmac',
  error_enabled: true,
  client_report_enabled: true,
  log_enabled: true,
  transaction_enabled: true,
  span_enabled: true,
  feedback_enabled: true,
  check_in_enabled: true,
  metric_enabled: true,
  replay_enabled: false,
  max_event_bytes: 1_048_576,
  max_events_per_second: null,
  burst: null,
});

function updateName(event: Event): void {
  if (!(event.target instanceof HTMLInputElement)) return;
  project.display_name = event.target.value;
  if (!slugWasEdited.value) project.slug = suggestedSlug(event.target.value);
}

function normalizeOptionalLimit(value: unknown): number | null {
  return typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : null;
}

function setIpPolicy(value: string): void {
  project.ip_policy = value as CreateProjectInput['ip_policy'];
}

const createProject = useMutation({
  mutationFn: () =>
    api.createProject({
      ...project,
      max_events_per_second: normalizeOptionalLimit(project.max_events_per_second),
      burst: normalizeOptionalLimit(project.burst),
    }),
  onSuccess: async (created) => {
    await session.refreshProjects();
    session.selectProject(created.project_id);
    await router.push('/project/setup');
  },
});
</script>

<template>
  <section v-if="!canCreateProject">
    <EmptyState
      icon="blocked"
      :title="$t('onboarding.restrictedTitle')"
      :description="$t('onboarding.restrictedDescription')"
    >
      <RouterLink class="button button--secondary" to="/dashboard">
        <AppIcon name="back" :size="16" />
        {{ $t('onboarding.backToDashboard') }}
      </RouterLink>
    </EmptyState>
  </section>
  <section v-else class="onboarding-layout" aria-labelledby="first-project-title">
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{
            firstProject
              ? $t('onboarding.organizationReady')
              : $t('onboarding.projectAdministration')
          }}
        </p>
        <h1 id="first-project-title">
          {{ firstProject ? $t('onboarding.firstProjectTitle') : $t('onboarding.newProjectTitle') }}
        </h1>
        <p>{{ $t('onboarding.projectDescription') }}</p>
        <p class="info-note">
          <AppIcon name="organization" :size="16" />
          <span>
            {{ $t('onboarding.targetOrganization') }}
            <strong>{{
              session.activeOrganization?.display_name ?? $t('organization.fallbackName')
            }}</strong>
          </span>
        </p>
      </div>
    </header>

    <div class="onboarding-grid">
      <form class="panel settings-form" @submit.prevent="createProject.mutate()">
        <div class="section-heading">
          <div class="section-heading__content">
            <span class="section-icon section-icon--info">
              <AppIcon name="server" :size="18" />
            </span>
            <div>
              <p class="eyebrow">{{ $t('onboarding.identity') }}</p>
              <h2>{{ $t('onboarding.nameService') }}</h2>
            </div>
          </div>
        </div>

        <div class="form-grid">
          <label>
            {{ $t('onboarding.projectName') }}
            <input
              :value="project.display_name"
              autocomplete="off"
              maxlength="128"
              placeholder="Payments API"
              required
              @input="updateName"
            />
          </label>
          <label>
            {{ $t('onboarding.slug') }}
            <input
              v-model.trim="project.slug"
              autocomplete="off"
              maxlength="64"
              pattern="[a-z0-9]+(?:-[a-z0-9]+)*"
              placeholder="payments-api"
              required
              @input="slugWasEdited = true"
            />
            <small>{{ $t('onboarding.slugHelp') }}</small>
          </label>
        </div>

        <div>
          <BaseSelect
            :model-value="project.ip_policy"
            :options="ipPolicyOptions"
            :label="$t('onboarding.ipHandling')"
            @update:model-value="setIpPolicy"
          />
          <small class="field-help">{{ $t('onboarding.ipHelp') }}</small>
        </div>

        <div class="check-grid">
          <label class="check-control">
            <input v-model="project.error_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="bug" :size="17" />
                <strong>{{ $t('onboarding.errorEvents') }}</strong>
              </span>
              <small>{{ $t('onboarding.errorEventsHelp') }}</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.client_report_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="activity" :size="17" />
                <strong>{{ $t('onboarding.clientReports') }}</strong>
              </span>
              <small>{{ $t('onboarding.clientReportsHelp') }}</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.log_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="logs" :size="17" />
                <strong>{{ $t('onboarding.structuredLogs') }}</strong>
              </span>
              <small>{{ $t('onboarding.structuredLogsHelp') }}</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.transaction_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="traces" :size="17" />
                <strong>{{ $t('onboarding.transactions') }}</strong>
              </span>
              <small>{{ $t('onboarding.transactionsHelp') }}</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.span_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="activity" :size="17" />
                <strong>{{ $t('onboarding.spans') }}</strong>
              </span>
              <small>{{ $t('onboarding.spansHelp') }}</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.feedback_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="message" :size="17" />
                <strong>{{ $t('onboarding.userFeedback') }}</strong>
              </span>
              <small>{{ $t('onboarding.userFeedbackHelp') }}</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.check_in_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="monitors" :size="17" />
                <strong>{{ $t('onboarding.cronCheckIns') }}</strong>
              </span>
              <small>{{ $t('onboarding.cronCheckInsHelp') }}</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.metric_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="gauge" :size="17" />
                <strong>{{ $t('onboarding.applicationMetrics') }}</strong>
              </span>
              <small>{{ $t('onboarding.applicationMetricsHelp') }}</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.replay_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="view" :size="17" />
                <strong>{{ $t('onboarding.sessionReplay') }}</strong>
              </span>
              <small>{{ $t('onboarding.sessionReplayHelp') }}</small>
            </span>
          </label>
        </div>

        <div class="form-grid form-grid--three form-grid--aligned-controls">
          <label>
            {{ $t('onboarding.maxEventBytes') }}
            <input
              v-model.number="project.max_event_bytes"
              type="number"
              min="1"
              max="20971520"
              required
            />
          </label>
          <label>
            {{ $t('onboarding.eventsPerSecond') }}
            <input
              v-model.number="project.max_events_per_second"
              type="number"
              min="1"
              :placeholder="$t('onboarding.unlimited')"
            />
          </label>
          <label>
            {{ $t('onboarding.burst') }}
            <input
              v-model.number="project.burst"
              type="number"
              min="1"
              :placeholder="$t('onboarding.automatic')"
            />
          </label>
        </div>

        <ApiErrorPanel
          v-if="createProject.error.value"
          :error="createProject.error.value"
          :title="$t('onboarding.createFailed')"
        />
        <button
          class="button button--primary"
          type="submit"
          :disabled="createProject.isPending.value"
        >
          <AppIcon :name="createProject.isPending.value ? 'loading' : 'plus'" :size="16" />
          {{
            createProject.isPending.value
              ? $t('onboarding.creating')
              : $t('onboarding.createProject')
          }}
        </button>
      </form>

      <aside class="panel onboarding-summary">
        <div class="onboarding-summary__heading">
          <span class="section-icon section-icon--success">
            <AppIcon name="connect" :size="18" />
          </span>
          <div>
            <p class="eyebrow">{{ $t('onboarding.next') }}</p>
            <h2>{{ $t('onboarding.connectTitle') }}</h2>
          </div>
        </div>
        <ol>
          <li><span>1</span>{{ $t('onboarding.stepProject') }}</li>
          <li><span>2</span>{{ $t('onboarding.stepContext') }}</li>
          <li><span>3</span>{{ $t('onboarding.stepInstructions') }}</li>
        </ol>
        <p class="info-note">
          {{ $t('onboarding.durableNote') }}
        </p>
      </aside>
    </div>
  </section>
</template>
