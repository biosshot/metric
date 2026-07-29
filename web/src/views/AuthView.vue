<script setup lang="ts">
import { ref, watch } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import LogoMark from '../components/LogoMark.vue';
import { suggestedSlug } from '../lib/slug';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const router = useRouter();
const route = useRoute();
const invited = route.name === 'password-setup' || typeof route.query.setup_token === 'string';
const mode = ref<'login' | 'bootstrap' | 'setup'>(invited ? 'setup' : 'login');
const busy = ref(false);
const error = ref<unknown>(null);
const successNotice = ref('');

const email = ref(typeof route.query.email === 'string' ? route.query.email : '');
const password = ref('');
const organizationId = ref(
  typeof route.query.organization_id === 'string'
    ? route.query.organization_id
    : (session.organizationId ?? ''),
);

const setupToken = ref(typeof route.query.setup_token === 'string' ? route.query.setup_token : '');
const displayName = ref('');
const organizationSlug = ref('');
const organizationName = ref('');
const organizationSlugWasEdited = ref(false);

function updateOrganizationName(event: Event): void {
  if (!(event.target instanceof HTMLInputElement)) return;
  organizationName.value = event.target.value;
  if (!organizationSlugWasEdited.value) {
    organizationSlug.value = suggestedSlug(event.target.value);
  }
}

watch(
  () => [route.name, route.query.setup_token, route.query.organization_id],
  () => {
    const token = route.query.setup_token;
    const invitedOrganizationId = route.query.organization_id;
    if (route.name === 'password-setup' || typeof token === 'string') {
      mode.value = 'setup';
      if (typeof token === 'string') setupToken.value = token;
      if (typeof invitedOrganizationId === 'string') {
        organizationId.value = invitedOrganizationId;
      }
    }
  },
  { immediate: true },
);

async function login(): Promise<void> {
  busy.value = true;
  error.value = null;
  try {
    await session.login(email.value, password.value, organizationId.value);
    await router.replace('/dashboard');
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
    successNotice.value = `Organization created. Its ID is ${identity.organization_id}. Sign in to continue.`;
    mode.value = 'login';
  } catch (cause) {
    error.value = cause;
  } finally {
    busy.value = false;
  }
}

async function setupInvitedPassword(): Promise<void> {
  busy.value = true;
  error.value = null;
  try {
    await api.setupPassword(setupToken.value, password.value, organizationId.value);
    password.value = '';
    successNotice.value = 'Password set. Sign in with your invited email to continue.';
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
        <LogoMark :size="30" />
      </div>
      <p class="eyebrow">Metric</p>
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
        <button type="button" role="tab" :aria-selected="mode === 'setup'" @click="mode = 'setup'">
          Invitation
        </button>
      </div>
      <div v-if="successNotice" class="success-notice" role="status">{{ successNotice }}</div>
      <form v-if="mode === 'login'" @submit.prevent="login">
        <p class="eyebrow">Secure session</p>
        <h2 id="auth-title">Sign in to Metric</h2>
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
      <form v-else-if="mode === 'bootstrap'" @submit.prevent="bootstrap">
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
            <input
              :value="organizationName"
              autocomplete="organization"
              required
              @input="updateOrganizationName"
            />
          </label>
          <label>
            Slug
            <input
              v-model.trim="organizationSlug"
              autocomplete="off"
              pattern="[a-z0-9]+(?:-[a-z0-9]+)*"
              required
              @input="organizationSlugWasEdited = true"
            />
            <small
              >Generated from the organization name; edit it only if you need another ID.</small
            >
          </label>
        </div>
        <ApiErrorPanel v-if="error" :error="error" title="Setup failed" />
        <button class="button button--primary button--wide" type="submit" :disabled="busy">
          <AppIcon :name="busy ? 'loading' : 'plus'" :size="16" />
          {{ busy ? 'Creating…' : 'Create owner and organization' }}
        </button>
      </form>
      <form v-else @submit.prevent="setupInvitedPassword">
        <p class="eyebrow">Organization invitation</p>
        <h2 id="auth-title">Set your password</h2>
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
          Organization ID
          <input v-model.trim="organizationId" inputmode="numeric" pattern="[1-9][0-9]*" required />
        </label>
        <label>
          New password
          <input
            v-model="password"
            type="password"
            autocomplete="new-password"
            minlength="12"
            required
          />
        </label>
        <ApiErrorPanel v-if="error" :error="error" title="Password was not set" />
        <button class="button button--primary button--wide" type="submit" :disabled="busy">
          <AppIcon :name="busy ? 'loading' : 'key'" :size="16" />
          {{ busy ? 'Saving…' : 'Set password' }}
        </button>
      </form>
    </section>
  </main>
</template>
