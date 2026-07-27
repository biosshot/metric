<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { computed, ref } from 'vue';
import { api } from '../api/client';
import type { CreatedApiToken } from '../api/types';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';

withDefaults(defineProps<{ embedded?: boolean }>(), { embedded: false });

const queryClient = useQueryClient();
const tokenProfile = ref('releases');
const tokenName = ref('sentry-cli releases');
const expiresOn = ref(new Date(Date.now() + 30 * 24 * 60 * 60 * 1_000).toISOString().slice(0, 10));
const createdToken = ref<CreatedApiToken | null>(null);
const revokingTokenId = ref<string | null>(null);
const profileOptions: SelectOption[] = [
  { value: 'releases', label: 'Releases and deploys', icon: 'release' },
  { value: 'debug-files', label: 'Debug files', icon: 'fileCode' },
];
const tokenScopes = computed(() =>
  tokenProfile.value === 'releases'
    ? ['release:read', 'release:write']
    : ['debug_file:read', 'debug_file:write'],
);
const profileTitle = computed(() =>
  tokenProfile.value === 'releases' ? 'Create Release CLI token' : 'Create debug-file token',
);

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
  if (!value) return 'Never';
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <header v-if="!embedded" class="page-header">
      <div>
        <p class="eyebrow">Account security</p>
        <h1>API tokens</h1>
        <p>
          Personal credentials for command-line tools. Token values are shown once and are never
          stored in readable form.
        </p>
      </div>
    </header>

    <ApiErrorPanel
      v-if="createToken.error.value"
      :error="createToken.error.value"
      title="API token was not created"
    />

    <section v-if="createdToken" class="panel token-secret-panel" aria-live="polite">
      <div class="section-heading">
        <div>
          <p class="eyebrow">Copy now</p>
          <h2>New token</h2>
          <p>This value disappears when you leave or refresh this page.</p>
        </div>
        <button
          class="icon-button"
          type="button"
          aria-label="Hide token"
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
            Grants only <code>{{ tokenScopes.join(', ') }}</code
            >.
          </p>
        </div>
      </div>
      <div class="form-grid">
        <BaseSelect v-model="tokenProfile" :options="profileOptions" label="CLI capability" />
        <label>
          Token name
          <input v-model.trim="tokenName" required maxlength="120" autocomplete="off" />
        </label>
        <label>
          Expires on
          <input v-model="expiresOn" required type="date" />
        </label>
      </div>
      <button
        class="button button--primary"
        type="submit"
        :disabled="createToken.isPending.value || !tokenName || !expiresOn"
      >
        <AppIcon name="key" :size="16" />
        {{ createToken.isPending.value ? 'Creating…' : 'Create token' }}
      </button>
    </form>

    <LoadingPanel v-if="tokens.isPending.value" label="Loading API tokens…" />
    <ApiErrorPanel
      v-else-if="tokens.error.value"
      :error="tokens.error.value"
      title="API tokens could not be loaded"
      @retry="tokens.refetch()"
    />
    <EmptyState
      v-else-if="!tokens.data.value?.items.length"
      icon="key"
      title="No API tokens"
      description="Create a short-lived token when a CLI or automation needs authenticated access."
    />
    <section v-else class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">Active credentials</p>
          <h2>Issued tokens</h2>
        </div>
      </div>
      <ApiErrorPanel
        v-if="revokeToken.error.value"
        :error="revokeToken.error.value"
        title="API token was not revoked"
      />
      <div class="token-list">
        <article v-for="token in tokens.data.value?.items" :key="token.id">
          <div>
            <strong>{{ token.name }}</strong>
            <span class="token-scopes">{{ token.scopes.join(' · ') }}</span>
            <small>
              Expires {{ formatTimestamp(token.expires_at) }} · Last used
              {{ formatTimestamp(token.last_used_at) }}
            </small>
          </div>
          <button
            class="button button--danger"
            type="button"
            :disabled="revokeToken.isPending.value"
            @click="revokeToken.mutate(token.id)"
          >
            <AppIcon name="delete" :size="16" />
            {{ revokingTokenId === token.id ? 'Revoking…' : 'Revoke' }}
          </button>
        </article>
      </div>
    </section>
  </section>
</template>
