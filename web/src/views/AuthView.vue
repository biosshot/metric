<script setup lang="ts">
import { ref } from 'vue';
import { useRouter } from 'vue-router';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const router = useRouter();
const mode = ref<'login' | 'bootstrap'>('login');
const busy = ref(false);
const error = ref<unknown>(null);
const bootstrapResult = ref<{ organizationId: string } | null>(null);

const email = ref('');
const password = ref('');
const organizationId = ref(session.organizationId ?? '');

const setupToken = ref('');
const displayName = ref('');
const organizationSlug = ref('');
const organizationName = ref('');

async function login(): Promise<void> {
  busy.value = true;
  error.value = null;
  try {
    await session.login(email.value, password.value, organizationId.value);
    await router.replace('/issues');
  } catch (cause) {
    error.value = cause;
  } finally {
    busy.value = false;
  }
}

async function bootstrap(): Promise<void> {
  busy.value = true;
  error.value = null;
  try {
    const identity = await api.bootstrap({
      setup_token: setupToken.value,
      email: email.value,
      display_name: displayName.value,
      password: password.value,
      organization_slug: organizationSlug.value,
      organization_name: organizationName.value,
    });
    organizationId.value = identity.organization_id;
    bootstrapResult.value = { organizationId: identity.organization_id };
    mode.value = 'login';
  } catch (cause) {
    error.value = cause;
  } finally {
    busy.value = false;
  }
}
</script>

<template>
  <main id="main-content" class="auth-layout">
    <section class="auth-brand">
      <div class="brand-mark" aria-hidden="true">
        <AppIcon name="bug" :size="30" />
      </div>
      <p class="eyebrow">Faultkeep</p>
      <h1>Understand failures.<br />Keep the signal.</h1>
      <p>
        A focused error investigation workspace for teams that need clear evidence, stable
        workflows, and no hidden failure states.
      </p>
      <ul>
        <li>Exact event and Issue history</li>
        <li>Project-isolated access</li>
        <li>Request IDs on every server error</li>
      </ul>
    </section>
    <section class="auth-card" aria-labelledby="auth-title">
      <div class="auth-tabs" role="tablist" aria-label="Authentication mode">
        <button type="button" role="tab" :aria-selected="mode === 'login'" @click="mode = 'login'">
          Sign in
        </button>
        <button
          type="button"
          role="tab"
          :aria-selected="mode === 'bootstrap'"
          @click="mode = 'bootstrap'"
        >
          First setup
        </button>
      </div>
      <div v-if="bootstrapResult" class="success-notice" role="status">
        Organization created. Its ID is <strong>{{ bootstrapResult.organizationId }}</strong
        >. Sign in to continue.
      </div>
      <form v-if="mode === 'login'" @submit.prevent="login">
        <p class="eyebrow">Secure session</p>
        <h2 id="auth-title">Sign in to Faultkeep</h2>
        <label>
          Email
          <input v-model.trim="email" type="email" autocomplete="username" required />
        </label>
        <label>
          Password
          <input
            v-model="password"
            type="password"
            autocomplete="current-password"
            minlength="12"
            required
          />
        </label>
        <label>
          Organization ID
          <input v-model.trim="organizationId" inputmode="numeric" pattern="[1-9][0-9]*" required />
          <small>Shown after first setup or provided by your administrator.</small>
        </label>
        <ApiErrorPanel v-if="error" :error="error" title="Sign in failed" />
        <button class="button button--primary button--wide" type="submit" :disabled="busy">
          <AppIcon :name="busy ? 'loading' : 'signOut'" :size="16" />
          {{ busy ? 'Signing in…' : 'Sign in' }}
        </button>
      </form>
      <form v-else @submit.prevent="bootstrap">
        <p class="eyebrow">One-time initialization</p>
        <h2 id="auth-title">Create the first owner</h2>
        <label>
          Setup token
          <input
            v-model.trim="setupToken"
            autocomplete="off"
            minlength="64"
            maxlength="64"
            required
          />
        </label>
        <label>
          Your name
          <input v-model.trim="displayName" autocomplete="name" required />
        </label>
        <label>
          Email
          <input v-model.trim="email" type="email" autocomplete="username" required />
        </label>
        <label>
          Password
          <input
            v-model="password"
            type="password"
            autocomplete="new-password"
            minlength="12"
            required
          />
        </label>
        <div class="form-grid">
          <label>
            Organization
            <input v-model.trim="organizationName" required />
          </label>
          <label>
            Slug
            <input v-model.trim="organizationSlug" pattern="[a-z0-9]+(?:-[a-z0-9]+)*" required />
          </label>
        </div>
        <ApiErrorPanel v-if="error" :error="error" title="Setup failed" />
        <button class="button button--primary button--wide" type="submit" :disabled="busy">
          <AppIcon :name="busy ? 'loading' : 'plus'" :size="16" />
          {{ busy ? 'Creating…' : 'Create owner and organization' }}
        </button>
      </form>
    </section>
  </main>
</template>
