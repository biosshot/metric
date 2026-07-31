<script setup lang="ts">
import { ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute, useRouter } from 'vue-router';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import LogoMark from '../components/LogoMark.vue';
import LocaleSelect from '../components/LocaleSelect.vue';
import { suggestedSlug } from '../lib/slug';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
const router = useRouter();
const route = useRoute();
const { t } = useI18n();
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
    successNotice.value = t('auth.organizationCreated', { id: identity.organization_id });
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
    successNotice.value = t('auth.passwordSet');
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
      <h1>{{ $t('auth.taglineLead') }}<br />{{ $t('auth.taglineEnd') }}</h1>
      <p>{{ $t('auth.introduction') }}</p>
      <ul>
        <li>{{ $t('auth.exactHistory') }}</li>
        <li>{{ $t('auth.isolatedAccess') }}</li>
        <li>{{ $t('auth.requestIds') }}</li>
      </ul>
    </section>
    <section class="auth-card" aria-labelledby="auth-title">
      <LocaleSelect class="auth-card__locale" />
      <div class="auth-tabs" role="tablist" :aria-label="$t('auth.mode')">
        <button type="button" role="tab" :aria-selected="mode === 'login'" @click="mode = 'login'">
          {{ $t('auth.signIn') }}
        </button>
        <button
          type="button"
          role="tab"
          :aria-selected="mode === 'bootstrap'"
          @click="mode = 'bootstrap'"
        >
          {{ $t('auth.firstSetup') }}
        </button>
        <button type="button" role="tab" :aria-selected="mode === 'setup'" @click="mode = 'setup'">
          {{ $t('auth.invitation') }}
        </button>
      </div>
      <div v-if="successNotice" class="success-notice" role="status">{{ successNotice }}</div>
      <form v-if="mode === 'login'" @submit.prevent="login">
        <p class="eyebrow">{{ $t('auth.secureSession') }}</p>
        <h2 id="auth-title">{{ $t('auth.signInTitle') }}</h2>
        <label>
          {{ $t('auth.email') }}
          <input v-model.trim="email" type="email" autocomplete="username" required />
        </label>
        <label>
          {{ $t('auth.password') }}
          <input
            v-model="password"
            type="password"
            autocomplete="current-password"
            minlength="12"
            required
          />
        </label>
        <label>
          {{ $t('auth.organizationId') }}
          <input v-model.trim="organizationId" inputmode="numeric" pattern="[1-9][0-9]*" required />
          <small>{{ $t('auth.organizationIdHelp') }}</small>
        </label>
        <ApiErrorPanel v-if="error" :error="error" :title="$t('auth.signInFailed')" />
        <button class="button button--primary button--wide" type="submit" :disabled="busy">
          <AppIcon :name="busy ? 'loading' : 'signOut'" :size="16" />
          {{ busy ? $t('auth.signingIn') : $t('auth.signIn') }}
        </button>
      </form>
      <form v-else-if="mode === 'bootstrap'" @submit.prevent="bootstrap">
        <p class="eyebrow">{{ $t('auth.initialization') }}</p>
        <h2 id="auth-title">{{ $t('auth.createOwner') }}</h2>
        <label>
          {{ $t('auth.setupToken') }}
          <input
            v-model.trim="setupToken"
            autocomplete="off"
            minlength="64"
            maxlength="64"
            required
          />
        </label>
        <label>
          {{ $t('auth.yourName') }}
          <input v-model.trim="displayName" autocomplete="name" required />
        </label>
        <label>
          {{ $t('auth.email') }}
          <input v-model.trim="email" type="email" autocomplete="username" required />
        </label>
        <label>
          {{ $t('auth.password') }}
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
            {{ $t('auth.organization') }}
            <input
              :value="organizationName"
              autocomplete="organization"
              required
              @input="updateOrganizationName"
            />
          </label>
          <label>
            {{ $t('auth.slug') }}
            <input
              v-model.trim="organizationSlug"
              autocomplete="off"
              pattern="[a-z0-9]+(?:-[a-z0-9]+)*"
              required
              @input="organizationSlugWasEdited = true"
            />
            <small>{{ $t('auth.slugHelp') }}</small>
          </label>
        </div>
        <ApiErrorPanel v-if="error" :error="error" :title="$t('auth.setupFailed')" />
        <button class="button button--primary button--wide" type="submit" :disabled="busy">
          <AppIcon :name="busy ? 'loading' : 'plus'" :size="16" />
          {{ busy ? $t('auth.creating') : $t('auth.createOwnerAndOrganization') }}
        </button>
      </form>
      <form v-else @submit.prevent="setupInvitedPassword">
        <p class="eyebrow">{{ $t('auth.organizationInvitation') }}</p>
        <h2 id="auth-title">{{ $t('auth.setPasswordTitle') }}</h2>
        <label>
          {{ $t('auth.setupToken') }}
          <input
            v-model.trim="setupToken"
            autocomplete="off"
            minlength="64"
            maxlength="64"
            required
          />
        </label>
        <label>
          {{ $t('auth.organizationId') }}
          <input v-model.trim="organizationId" inputmode="numeric" pattern="[1-9][0-9]*" required />
        </label>
        <label>
          {{ $t('auth.newPassword') }}
          <input
            v-model="password"
            type="password"
            autocomplete="new-password"
            minlength="12"
            required
          />
        </label>
        <ApiErrorPanel v-if="error" :error="error" :title="$t('auth.passwordNotSet')" />
        <button class="button button--primary button--wide" type="submit" :disabled="busy">
          <AppIcon :name="busy ? 'loading' : 'key'" :size="16" />
          {{ busy ? $t('auth.saving') : $t('auth.setPassword') }}
        </button>
      </form>
    </section>
  </main>
</template>
