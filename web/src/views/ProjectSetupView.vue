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
import { copyText } from '../lib/clipboard';

type SdkId = 'browser' | 'node' | 'python' | 'java' | 'dotnet' | 'go' | 'rust';

interface SdkSetup {
  id: SdkId;
  label: string;
  language: string;
  install: string;
  initialize(dsn: string): string;
  documentationUrl: string;
}

const sdkSetups: SdkSetup[] = [
  {
    id: 'browser',
    label: 'JavaScript Browser',
    language: 'javascript',
    install: 'npm install @sentry/browser@10.66.0',
    initialize: (dsn) => `import * as Sentry from "@sentry/browser";

Sentry.init({
  dsn: "${dsn}"
});

Sentry.captureMessage("Metric test event");`,
    documentationUrl: 'https://docs.sentry.io/platforms/javascript/',
  },
  {
    id: 'node',
    label: 'Node.js',
    language: 'javascript',
    install: 'npm install @sentry/node@10.66.0',
    initialize: (dsn) => `import * as Sentry from "@sentry/node";

Sentry.init({
  dsn: "${dsn}"
});

Sentry.captureMessage("Metric test event");`,
    documentationUrl: 'https://docs.sentry.io/platforms/javascript/guides/node/',
  },
  {
    id: 'python',
    label: 'Python',
    language: 'python',
    install: 'pip install sentry-sdk==2.32.0',
    initialize: (dsn) => `import sentry_sdk

sentry_sdk.init(dsn="${dsn}")
sentry_sdk.capture_message("Metric test event")`,
    documentationUrl: 'https://docs.sentry.io/platforms/python/',
  },
  {
    id: 'java',
    label: 'Java',
    language: 'java',
    install: 'implementation("io.sentry:sentry:8.50.1")',
    initialize: (dsn) => `import io.sentry.Sentry;

Sentry.init(options -> options.setDsn("${dsn}"));
Sentry.captureMessage("Metric test event");`,
    documentationUrl: 'https://docs.sentry.io/platforms/java/',
  },
  {
    id: 'dotnet',
    label: '.NET',
    language: 'csharp',
    install: 'dotnet add package Sentry --version 6.7.0',
    initialize: (dsn) => `using Sentry;

using var sdk = SentrySdk.Init(options =>
{
    options.Dsn = "${dsn}";
});

SentrySdk.CaptureMessage("Metric test event");`,
    documentationUrl: 'https://docs.sentry.io/platforms/dotnet/',
  },
  {
    id: 'go',
    label: 'Go',
    language: 'go',
    install: 'go get github.com/getsentry/sentry-go@v0.48.0',
    initialize: (dsn) => `package main

import (
    "time"

    "github.com/getsentry/sentry-go"
)

func main() {
    sentry.Init(sentry.ClientOptions{Dsn: "${dsn}"})
    defer sentry.Flush(2 * time.Second)
    sentry.CaptureMessage("Metric test event")
}`,
    documentationUrl: 'https://docs.sentry.io/platforms/go/',
  },
  {
    id: 'rust',
    label: 'Rust',
    language: 'rust',
    install: 'cargo add sentry@0.48.5',
    initialize: (dsn) => `fn main() {
    let _guard = sentry::init(("${dsn}", sentry::ClientOptions::default()));
    sentry::capture_message("Metric test event", sentry::Level::Info);
}`,
    documentationUrl: 'https://docs.sentry.io/platforms/rust/',
  },
];

const session = useSessionStore();
const { t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const canAdministerProject = computed(() => session.has('project:admin'));
const copyNotice = ref('');
const copyError = ref('');
const selectedSdk = ref<SdkId>('browser');
const sdkOptions: SelectOption[] = sdkSetups.map((sdk) => ({
  value: sdk.id,
  label: sdk.label,
  icon: sdk.id === 'node' ? 'server' : 'fileCode',
}));
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
const selectedSetup = computed(
  () => sdkSetups.find((sdk) => sdk.id === selectedSdk.value) ?? sdkSetups[0]!,
);
const codeExample = computed(() => selectedSetup.value.initialize(activeDsn.value));

function setSdk(value: string): void {
  selectedSdk.value = value as SdkId;
}

async function copy(value: string): Promise<void> {
  copyNotice.value = '';
  copyError.value = '';
  try {
    await copyText(value);
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
      <div class="code-examples__blocks">
        <CodeBlock
          :code="selectedSetup.install"
          language="shell"
          :title="$t('projectSetup.installation')"
        />
        <CodeBlock
          :code="codeExample"
          :language="selectedSetup.language"
          :title="$t('projectSetup.minimalExample')"
        />
      </div>
      <nav class="code-examples__links" :aria-label="$t('projectSetup.nextSteps')">
        <RouterLink to="/issues">{{ $t('projectSetup.openIssues') }}</RouterLink>
        <a :href="selectedSetup.documentationUrl" target="_blank" rel="noopener noreferrer">
          {{ $t('projectSetup.documentation') }}
        </a>
      </nav>
    </section>
  </section>
</template>
