<script setup lang="ts">
import { reactive, ref } from 'vue';
import { useMutation } from '@tanstack/vue-query';
import { useRouter } from 'vue-router';
import { api } from '../api/client';
import type { CreateProjectInput } from '../api/types';
import { useSessionStore } from '../stores/session';
import ApiErrorPanel from './ApiErrorPanel.vue';
import AppIcon from './AppIcon.vue';
import BaseSelect, { type SelectOption } from './BaseSelect.vue';

const session = useSessionStore();
const router = useRouter();
const slugWasEdited = ref(false);
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
  max_event_bytes: 1_048_576,
  max_events_per_second: null,
  burst: null,
});

function suggestedSlug(value: string): string {
  return value
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64);
}

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
  <section class="onboarding-layout" aria-labelledby="first-project-title">
    <header class="page-header">
      <div>
        <p class="eyebrow">Organization ready</p>
        <h1 id="first-project-title">Create your first project</h1>
        <p>A project isolates its Events, Issues, privacy policy, limits, and SDK credentials.</p>
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
              <p class="eyebrow">Project identity</p>
              <h2>Name the service you want to monitor</h2>
            </div>
          </div>
        </div>

        <div class="form-grid">
          <label>
            Project name
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
            Slug
            <input
              v-model.trim="project.slug"
              autocomplete="off"
              maxlength="64"
              pattern="[a-z0-9]+(?:-[a-z0-9]+)*"
              placeholder="payments-api"
              required
              @input="slugWasEdited = true"
            />
            <small>Stable lowercase identifier used in project administration.</small>
          </label>
        </div>

        <div>
          <BaseSelect
            :model-value="project.ip_policy"
            :options="ipPolicyOptions"
            label="IP address handling"
            @update:model-value="setIpPolicy"
          />
          <small class="field-help"
            >This policy is applied before an Event reaches durable storage.</small
          >
        </div>

        <div class="check-grid">
          <label class="check-control">
            <input v-model="project.error_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="bug" :size="17" />
                <strong>Error Events</strong>
              </span>
              <small>Required for Issue investigation.</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.client_report_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="activity" :size="17" />
                <strong>Client reports</strong>
              </span>
              <small>Accept SDK delivery outcome reports.</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.log_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="logs" :size="17" />
                <strong>Structured Logs</strong>
              </span>
              <small>Accept SDK log records.</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.transaction_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="traces" :size="17" />
                <strong>Transactions</strong>
              </span>
              <small>Accept root performance segments.</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.span_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="activity" :size="17" />
                <strong>Spans</strong>
              </span>
              <small>Accept child and standalone spans.</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.feedback_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="message" :size="17" />
                <strong>User Feedback</strong>
              </span>
              <small>Accept bounded reports from the Feedback SDK.</small>
            </span>
          </label>
          <label class="check-control">
            <input v-model="project.check_in_enabled" type="checkbox" />
            <span class="check-control__copy">
              <span class="check-control__title">
                <AppIcon name="monitors" :size="17" />
                <strong>Cron check-ins</strong>
              </span>
              <small>Track scheduled jobs and missed executions.</small>
            </span>
          </label>
        </div>

        <div class="form-grid form-grid--three">
          <label>
            Maximum Event bytes
            <input
              v-model.number="project.max_event_bytes"
              type="number"
              min="1"
              max="20971520"
              required
            />
          </label>
          <label>
            Events per second
            <input
              v-model.number="project.max_events_per_second"
              type="number"
              min="1"
              placeholder="Unlimited"
            />
          </label>
          <label>
            Burst
            <input v-model.number="project.burst" type="number" min="1" placeholder="Automatic" />
          </label>
        </div>

        <ApiErrorPanel
          v-if="createProject.error.value"
          :error="createProject.error.value"
          title="Project was not created"
        />
        <button
          class="button button--primary"
          type="submit"
          :disabled="createProject.isPending.value"
        >
          <AppIcon :name="createProject.isPending.value ? 'loading' : 'plus'" :size="16" />
          {{ createProject.isPending.value ? 'Creating…' : 'Create project and DSN' }}
        </button>
      </form>

      <aside class="panel onboarding-summary">
        <div class="onboarding-summary__heading">
          <span class="section-icon section-icon--success">
            <AppIcon name="connect" :size="18" />
          </span>
          <div>
            <p class="eyebrow">What happens next</p>
            <h2>Connect without hidden setup</h2>
          </div>
        </div>
        <ol>
          <li><span>1</span>Metric creates the project and its first DSN key.</li>
          <li><span>2</span>The new project becomes the active investigation context.</li>
          <li><span>3</span>You receive exact Sentry SDK configuration instructions.</li>
        </ol>
        <p class="info-note">
          No Event is accepted before its project policy and DSN are durably stored.
        </p>
      </aside>
    </div>
  </section>
</template>
