<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useQuery } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { useSessionStore } from '../stores/session';

type SdkId = 'browser' | 'node' | 'python' | 'java' | 'dotnet';

const session = useSessionStore();
const { t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const canAdministerProject = computed(() => session.has('project:admin'));
const copyNotice = ref('');
const copyError = ref('');
const selectedSdk = ref<SdkId>('browser');
const sdkOptions: SelectOption[] = [
  { value: 'browser', label: 'JavaScript — Browser', icon: 'fileCode' },
  { value: 'node', label: 'JavaScript — Node.js', icon: 'server' },
  { value: 'python', label: 'Python', icon: 'fileCode' },
  { value: 'java', label: 'Java', icon: 'fileCode' },
  { value: 'dotnet', label: 'C# / .NET', icon: 'fileCode' },
];
const keys = useQuery({
  queryKey: computed(() => ['project-keys', projectId.value]),
  queryFn: () => api.keys(projectId.value),
  enabled: canAdministerProject,
});
const activeKeys = computed(
  () => keys.data.value?.items.filter((item) => item.state === 'active') ?? [],
);

function dsn(key: string): string {
  return `${window.location.protocol}//${key}@${window.location.host}/${projectId.value}`;
}

const activeDsn = computed(() => {
  const key = activeKeys.value[0];
  return key ? dsn(key.dsn_key) : '';
});

const codeExample = computed(() => {
  const value = activeDsn.value;
  const examples: Record<SdkId, { language: string; title: string; code: string }> = {
    browser: {
      language: 'javascript',
      title: 'JavaScript — Browser',
      code: `import * as Sentry from "@sentry/browser";

Sentry.init({
  dsn: "${value}",
  tracesSampleRate: 0,
  integrations: [
    Sentry.replayIntegration({
      maskAllText: true,
      blockAllMedia: true
    })
  ],
  replaysSessionSampleRate: 0.1,
  replaysOnErrorSampleRate: 1.0
});`,
    },
    node: {
      language: 'javascript',
      title: 'JavaScript — Node.js',
      code: `import * as Sentry from "@sentry/node";

Sentry.init({
  dsn: "${value}",
  tracesSampleRate: 0
});`,
    },
    python: {
      language: 'python',
      title: 'Python',
      code: `import sentry_sdk

sentry_sdk.init(
    dsn="${value}",
    traces_sample_rate=0,
)`,
    },
    java: {
      language: 'java',
      title: 'Java',
      code: `import io.sentry.Sentry;

Sentry.init(options -> {
    options.setDsn("${value}");
    options.setTracesSampleRate(0.0);
});`,
    },
    dotnet: {
      language: 'csharp',
      title: 'C# / .NET',
      code: `using Sentry;

SentrySdk.Init(options =>
{
    options.Dsn = "${value}";
    options.TracesSampleRate = 0;
});`,
    },
  };
  return examples[selectedSdk.value];
});

function setSdk(value: string): void {
  selectedSdk.value = value as SdkId;
}

async function copy(value: string): Promise<void> {
  copyNotice.value = '';
  copyError.value = '';
  try {
    await navigator.clipboard.writeText(value);
    copyNotice.value = t('projectSetup.copied');
  } catch {
    copyError.value = t('projectSetup.copyDenied');
  }
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.display_name }}</p>
        <h1>{{ $t('projectSetup.title') }}</h1>
        <p>{{ $t('projectSetup.description') }}</p>
      </div>
    </header>

    <EmptyState
      v-if="!canAdministerProject"
      icon="blocked"
      :title="$t('projectSetup.restricted')"
      :description="$t('projectSetup.restrictedDescription')"
    />

    <div v-else class="setup-grid">
      <section class="panel setup-steps">
        <ol>
          <li>
            <span><AppIcon name="key" :size="18" /></span>
            <div>
              <h2>{{ $t('projectSetup.chooseDsn') }}</h2>
              <p>{{ $t('projectSetup.chooseDsnHelp') }}</p>
            </div>
          </li>
          <li>
            <span><AppIcon name="code" :size="18" /></span>
            <div>
              <h2>{{ $t('projectSetup.configure') }}</h2>
              <p>{{ $t('projectSetup.configureHelp') }}</p>
            </div>
          </li>
          <li>
            <span><AppIcon name="bug" :size="18" /></span>
            <div>
              <h2>{{ $t('projectSetup.testError') }}</h2>
              <p>{{ $t('projectSetup.testErrorHelp') }}</p>
            </div>
          </li>
        </ol>
      </section>

      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('projectSetup.credentials') }}</p>
            <h2>{{ $t('projectSetup.availableDsns') }}</h2>
          </div>
          <RouterLink
            v-if="session.has('project:admin')"
            class="button button--secondary"
            to="/settings/project#dsn-keys"
          >
            <AppIcon name="settings" :size="16" />
            {{ $t('projectSetup.manageKeys') }}
          </RouterLink>
        </div>
        <LoadingPanel v-if="keys.isPending.value" :label="$t('projectSetup.loadingKeys')" />
        <ApiErrorPanel
          v-else-if="keys.error.value"
          :error="keys.error.value"
          @retry="keys.refetch()"
        />
        <p v-if="copyNotice" class="success-notice" role="status">
          <AppIcon name="success" :size="16" />
          {{ copyNotice }}
        </p>
        <p v-if="copyError" class="permission-banner" role="alert">
          <AppIcon name="alert" :size="16" />
          {{ copyError }}
        </p>
        <EmptyState
          v-else-if="!activeKeys.length"
          icon="key"
          :title="$t('projectSetup.noDsn')"
          :description="$t('projectSetup.noDsnDescription')"
        />
        <div v-else class="dsn-list">
          <article v-for="key in activeKeys" :key="key.dsn_key">
            <div>
              <strong>{{ key.label }}</strong>
              <StatusBadge :status="key.state" />
            </div>
            <code>{{ dsn(key.dsn_key) }}</code>
            <button class="button button--secondary" type="button" @click="copy(dsn(key.dsn_key))">
              <AppIcon name="copy" :size="16" />
              {{ $t('projectSetup.copyDsn') }}
            </button>
          </article>
        </div>
      </section>
    </div>

    <section v-if="canAdministerProject && activeDsn" class="panel code-examples">
      <div class="code-examples__heading">
        <div>
          <p class="eyebrow">{{ $t('projectSetup.example') }}</p>
          <h2>{{ $t('projectSetup.initialize') }}</h2>
        </div>
        <BaseSelect
          :model-value="selectedSdk"
          :options="sdkOptions"
          :label="$t('projectSetup.sdk')"
          @update:model-value="setSdk"
        />
      </div>
      <CodeBlock
        :code="codeExample.code"
        :language="codeExample.language"
        :title="codeExample.title"
      />
      <p class="info-note">
        <AppIcon name="info" :size="16" />
        {{ $t('projectSetup.capabilities') }}
      </p>
    </section>
  </section>
</template>
