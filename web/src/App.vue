<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import ApiErrorPanel from './components/ApiErrorPanel.vue';
import AppIcon from './components/AppIcon.vue';
import BaseSelect, { type SelectOption } from './components/BaseSelect.vue';
import EmptyState from './components/EmptyState.vue';
import FirstProjectOnboarding from './components/FirstProjectOnboarding.vue';
import LoadingPanel from './components/LoadingPanel.vue';
import LogoMark from './components/LogoMark.vue';
import { useSessionStore } from './stores/session';
import AuthView from './views/AuthView.vue';

const session = useSessionStore();
const router = useRouter();
const navigationOpen = ref(false);
const logoutError = ref<unknown>(null);
const projectOptions = computed<SelectOption[]>(() =>
  session.projects.map((project) => ({
    value: project.id,
    label: project.display_name,
    icon: 'bug',
  })),
);

onMounted(() => session.restore());

function changeProject(projectId: string): void {
  session.selectProject(projectId);
  navigationOpen.value = false;
  void router.push('/dashboard');
}

async function logout(): Promise<void> {
  logoutError.value = null;
  try {
    await session.logout();
    await router.replace('/dashboard');
  } catch (error) {
    logoutError.value = error;
  }
}
</script>

<template>
  <a class="skip-link" href="#main-content">Skip to content</a>
  <div v-if="session.restoring" class="app-loading">
    <LoadingPanel label="Restoring your secure session…" />
  </div>
  <div v-else-if="session.restoreError" class="app-loading restore-failure">
    <ApiErrorPanel
      :error="session.restoreError"
      title="Session could not be checked"
      @retry="session.restore()"
    />
    <button class="button button--secondary" type="button" @click="session.dismissRestoreError()">
      <AppIcon name="back" :size="16" />
      Open sign-in instead
    </button>
  </div>
  <AuthView
    v-else-if="
      !session.authenticated ||
      $route.name === 'password-setup' ||
      typeof $route.query.setup_token === 'string'
    "
  />
  <div v-else class="app-shell">
    <header class="mobile-header">
      <button
        class="icon-button"
        type="button"
        aria-label="Toggle navigation"
        :aria-expanded="navigationOpen"
        @click="navigationOpen = !navigationOpen"
      >
        <AppIcon :name="navigationOpen ? 'close' : 'menu'" :size="20" />
      </button>
      <span class="mobile-header__brand">
        <LogoMark :size="18" />
        <strong>Metric</strong>
      </span>
    </header>

    <button
      v-if="navigationOpen"
      class="navigation-backdrop"
      type="button"
      aria-label="Close navigation"
      @click="navigationOpen = false"
    ></button>

    <aside class="sidebar" :class="{ 'sidebar--open': navigationOpen }">
      <div class="sidebar__brand">
        <span class="brand-mark brand-mark--small" aria-hidden="true">
          <LogoMark :size="18" />
        </span>
        <strong>Metric</strong>
      </div>

      <div class="sidebar__project">
        <BaseSelect
          class="project-switcher"
          :model-value="session.selectedProjectId ?? ''"
          :options="projectOptions"
          label="Project"
          @update:model-value="changeProject"
        />
        <RouterLink
          v-if="session.has('organization:admin')"
          class="project-create-link"
          to="/projects/new"
          @click="navigationOpen = false"
        >
          <AppIcon name="plus" :size="16" />
          New project
        </RouterLink>
      </div>

      <nav aria-label="Primary">
        <div class="sidebar__nav-group">
          <span class="sidebar__nav-label">Overview</span>
          <RouterLink to="/dashboard" @click="navigationOpen = false">
            <AppIcon name="dashboard" :size="18" />
            Dashboard
          </RouterLink>
        </div>

        <div class="sidebar__nav-group">
          <span class="sidebar__nav-label">Observe</span>
          <RouterLink to="/issues" @click="navigationOpen = false">
            <AppIcon name="clipboard" :size="18" />
            Issues
          </RouterLink>
          <RouterLink to="/logs" @click="navigationOpen = false">
            <AppIcon name="logs" :size="18" />
            Logs
          </RouterLink>
          <RouterLink
            to="/traces"
            :class="{ 'router-link-active': $route.name === 'performance' }"
            @click="navigationOpen = false"
          >
            <AppIcon name="traces" :size="18" />
            Traces
          </RouterLink>
          <RouterLink to="/replays" @click="navigationOpen = false">
            <AppIcon name="replay" :size="18" />
            Replays
          </RouterLink>
          <RouterLink to="/monitors" @click="navigationOpen = false">
            <AppIcon name="monitors" :size="18" />
            Monitors
          </RouterLink>
          <RouterLink to="/feedback" @click="navigationOpen = false">
            <AppIcon name="message" :size="18" />
            Feedback
          </RouterLink>
        </div>

        <div class="sidebar__nav-group">
          <span class="sidebar__nav-label">Delivery</span>
          <RouterLink to="/releases" @click="navigationOpen = false">
            <AppIcon name="release" :size="18" />
            Releases
          </RouterLink>
        </div>

        <div class="sidebar__nav-group">
          <span class="sidebar__nav-label">Configure</span>
          <RouterLink to="/project/setup" @click="navigationOpen = false">
            <AppIcon name="connect" :size="18" />
            SDK setup
          </RouterLink>
          <RouterLink to="/settings" @click="navigationOpen = false">
            <AppIcon name="settings" :size="18" />
            Settings
          </RouterLink>
        </div>
      </nav>

      <div class="sidebar__account">
        <RouterLink
          class="sidebar__identity"
          to="/settings/organization"
          @click="navigationOpen = false"
        >
          <strong>{{ session.identity?.role }}</strong>
          <span>Org {{ session.organizationId }}</span>
        </RouterLink>
        <button type="button" @click="logout">
          <AppIcon name="signOut" :size="16" />
          Sign out
        </button>
      </div>
    </aside>

    <main id="main-content" class="main-content">
      <ApiErrorPanel v-if="logoutError" :error="logoutError" title="Sign out failed" />
      <FirstProjectOnboarding
        v-if="
          !session.selectedProject &&
          !['settings-system', 'settings-organization'].includes(String($route.name)) &&
          session.has('organization:admin')
        "
      />
      <EmptyState
        v-else-if="
          !session.selectedProject &&
          !['settings-system', 'settings-organization'].includes(String($route.name))
        "
        icon="blocked"
        title="No accessible projects"
        description="Your account is valid, but an organization administrator must grant access to a project."
      />
      <RouterView v-else />
    </main>
  </div>
</template>
