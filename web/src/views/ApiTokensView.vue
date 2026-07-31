<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { api } from '../api/client';
import type { CreatedApiToken } from '../api/types';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import { useSessionStore } from '../stores/session';

withDefaults(defineProps<{ embedded?: boolean }>(), { embedded: false });

const queryClient = useQueryClient();
const session = useSessionStore();
const { locale, t } = useI18n();
const tokenProfile = ref('');
const tokenName = ref('');
const expiresOn = ref(new Date(Date.now() + 30 * 24 * 60 * 60 * 1_000).toISOString().slice(0, 10));
const createdToken = ref<CreatedApiToken | null>(null);
const revokingTokenId = ref<string | null>(null);

interface TokenProfile {
  value: string;
  label: string;
  icon: SelectOption['icon'];
  title: string;
  defaultName: string;
  scopes: string[];
}

const tokenProfiles = computed<TokenProfile[]>(() => [
  {
    value: 'releases',
    label: t('apiTokens.releases'),
    icon: 'release',
    title: t('apiTokens.releaseTitle'),
    defaultName: 'sentry-cli releases',
    scopes: ['release:read', 'release:write'],
  },
  {
    value: 'debug-files',
    label: t('apiTokens.debugFiles'),
    icon: 'fileCode',
    title: t('apiTokens.debugTitle'),
    defaultName: 'sentry-cli debug files',
    scopes: ['debug_file:read', 'debug_file:write'],
  },
  {
    value: 'issues',
    label: t('apiTokens.issues'),
    icon: 'bug',
    title: t('apiTokens.issueTitle'),
    defaultName: 'issue automation',
    scopes: ['event:read', 'issue:read', 'issue:write', 'project:read'],
  },
  {
    value: 'read-only',
    label: t('apiTokens.readOnly'),
    icon: 'view',
    title: t('apiTokens.readOnlyTitle'),
    defaultName: 'read-only API',
    scopes: [
      'event:read',
      'issue:read',
      'project:read',
      'debug_file:read',
      'artifact:read',
      'release:read',
    ],
  },
]);

const availableProfiles = computed(() =>
  tokenProfiles.value.filter((profile) => profile.scopes.every((scope) => session.has(scope))),
);
const profileOptions = computed<SelectOption[]>(() =>
  availableProfiles.value.map(({ value, label, icon }) => ({ value, label, icon })),
);
const selectedProfile = computed(() =>
  availableProfiles.value.find((profile) => profile.value === tokenProfile.value),
);
const tokenScopes = computed(() => selectedProfile.value?.scopes ?? []);
const profileTitle = computed(() => selectedProfile.value?.title ?? t('apiTokens.createTitle'));

watch(
  availableProfiles,
  (profiles) => {
    if (profiles.some((profile) => profile.value === tokenProfile.value)) return;
    const preferred =
      profiles.find((profile) => profile.value === 'releases') ??
      profiles.find((profile) => profile.value === 'issues') ??
      profiles[0];
    tokenProfile.value = preferred?.value ?? '';
    tokenName.value = preferred?.defaultName ?? '';
  },
  { immediate: true },
);

watch(tokenProfile, (value, previous) => {
  if (!value || value === previous) return;
  const profile = availableProfiles.value.find((candidate) => candidate.value === value);
  if (profile) tokenName.value = profile.defaultName;
});

const tokens = useQuery({
  queryKey: ['api-tokens'],
  queryFn: api.tokens,
});

const createToken = useMutation({
  mutationFn: () =>
    api.createToken(tokenName.value, tokenScopes.value, `${expiresOn.value}T23:59:59Z`),
  onSuccess: async (token) => {
    createdToken.value = token;
    await queryClient.invalidateQueries({ queryKey: ['api-tokens'] });
  },
});

const revokeToken = useMutation({
  mutationFn: api.revokeToken,
  onMutate: (tokenId) => {
    revokingTokenId.value = tokenId;
  },
  onSuccess: async () => {
    await queryClient.invalidateQueries({ queryKey: ['api-tokens'] });
  },
  onSettled: () => {
    revokingTokenId.value = null;
  },
});

function formatTimestamp(value: string | null): string {
  if (!value) return t('apiTokens.never');
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <header v-if="!embedded" class="page-header">
      <div>
        <p class="eyebrow">{{ $t('apiTokens.eyebrow') }}</p>
        <h1>{{ $t('apiTokens.title') }}</h1>
        <p>{{ $t('apiTokens.description') }}</p>
      </div>
    </header>

    <ApiErrorPanel
      v-if="createToken.error.value"
      :error="createToken.error.value"
      :title="$t('apiTokens.createFailed')"
    />

    <section v-if="createdToken" class="panel token-secret-panel" aria-live="polite">
      <div class="section-heading">
        <div>
          <p class="eyebrow">{{ $t('apiTokens.copyNow') }}</p>
          <h2>{{ $t('apiTokens.newToken') }}</h2>
          <p>{{ $t('apiTokens.secretHelp') }}</p>
        </div>
        <button
          class="icon-button"
          type="button"
          :aria-label="$t('apiTokens.hide')"
          @click="createdToken = null"
        >
          <AppIcon name="close" :size="18" />
        </button>
      </div>
      <CodeBlock :code="createdToken.token" language="text" title="SENTRY_AUTH_TOKEN" />
    </section>

    <form class="panel settings-form" @submit.prevent="createToken.mutate()">
      <div class="section-heading">
        <div>
          <p class="eyebrow">sentry-cli</p>
          <h2>{{ profileTitle }}</h2>
          <p class="muted">
            {{ $t('apiTokens.grants') }} <code>{{ tokenScopes.join(', ') }}</code
            >.
          </p>
          <p v-if="!session.has('release:write') || !session.has('debug_file:write')" class="muted">
            {{ $t('apiTokens.limitedProfiles') }}
          </p>
        </div>
      </div>
      <div class="form-grid">
        <BaseSelect
          v-model="tokenProfile"
          :options="profileOptions"
          :label="$t('apiTokens.capability')"
        />
        <label>
          {{ $t('apiTokens.name') }}
          <input v-model.trim="tokenName" required maxlength="120" autocomplete="off" />
        </label>
        <label>
          {{ $t('apiTokens.expiresOn') }}
          <input v-model="expiresOn" required type="date" />
        </label>
      </div>
      <button
        class="button button--primary"
        type="submit"
        :disabled="createToken.isPending.value || !tokenName || !expiresOn || !tokenScopes.length"
      >
        <AppIcon name="key" :size="16" />
        {{ createToken.isPending.value ? $t('apiTokens.creating') : $t('apiTokens.create') }}
      </button>
    </form>

    <LoadingPanel v-if="tokens.isPending.value" :label="$t('apiTokens.loading')" />
    <ApiErrorPanel
      v-else-if="tokens.error.value"
      :error="tokens.error.value"
      :title="$t('apiTokens.loadFailed')"
      @retry="tokens.refetch()"
    />
    <EmptyState
      v-else-if="!tokens.data.value?.items.length"
      icon="key"
      :title="$t('apiTokens.empty')"
      :description="$t('apiTokens.emptyDescription')"
    />
    <section v-else class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">{{ $t('apiTokens.active') }}</p>
          <h2>{{ $t('apiTokens.issued') }}</h2>
        </div>
      </div>
      <ApiErrorPanel
        v-if="revokeToken.error.value"
        :error="revokeToken.error.value"
        :title="$t('apiTokens.revokeFailed')"
      />
      <div class="token-list">
        <article v-for="token in tokens.data.value?.items" :key="token.id">
          <div>
            <strong>{{ token.name }}</strong>
            <span class="token-scopes">{{ token.scopes.join(' · ') }}</span>
            <small>{{
              $t('apiTokens.usage', {
                expires: formatTimestamp(token.expires_at),
                used: formatTimestamp(token.last_used_at),
              })
            }}</small>
          </div>
          <button
            class="button button--danger"
            type="button"
            :disabled="revokeToken.isPending.value"
            @click="revokeToken.mutate(token.id)"
          >
            <AppIcon name="delete" :size="16" />
            {{ revokingTokenId === token.id ? $t('apiTokens.revoking') : $t('apiTokens.revoke') }}
          </button>
        </article>
      </div>
    </section>
  </section>
</template>
