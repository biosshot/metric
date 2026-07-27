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
  void router.push('/issues');
}

async function logout(): Promise<void> {
  logoutError.value = null;
  try {
    await session.logout();
    await router.replace('/issues');
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

      <BaseSelect
        class="project-switcher"
        :model-value="session.selectedProjectId ?? ''"
        :options="projectOptions"
        label="Project"
        @update:model-value="changeProject"
      />

      <nav aria-label="Primary">
        <RouterLink to="/issues" @click="navigationOpen = false">
          <AppIcon name="clipboard" :size="18" />
          Issues
        </RouterLink>
        <RouterLink to="/logs" @click="navigationOpen = false">
          <AppIcon name="logs" :size="18" />
          Logs
        </RouterLink>
        <RouterLink to="/traces" @click="navigationOpen = false">
          <AppIcon name="traces" :size="18" />
          Traces
        </RouterLink>
        <RouterLink to="/performance" @click="navigationOpen = false">
          <AppIcon name="gauge" :size="18" />
          Performance
        </RouterLink>
        <RouterLink to="/releases" @click="navigationOpen = false">
          <AppIcon name="release" :size="18" />
          Releases
        </RouterLink>
        <RouterLink to="/project/setup" @click="navigationOpen = false">
          <AppIcon name="connect" :size="18" />
          SDK setup
        </RouterLink>
        <RouterLink to="/project/settings" @click="navigationOpen = false">
          <AppIcon name="settings" :size="18" />
          Project settings
        </RouterLink>
        <RouterLink to="/system" @click="navigationOpen = false">
          <AppIcon name="activity" :size="18" />
          System status
        </RouterLink>
        <RouterLink to="/organization" @click="navigationOpen = false">
          <AppIcon name="organization" :size="18" />
          Organization
        </RouterLink>
      </nav>

      <div class="sidebar__account">
        <div>
          <strong>{{ session.identity?.role }}</strong>
          <span>Org {{ session.organizationId }}</span>
        </div>
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
          !['system', 'organization'].includes(String($route.name)) &&
          session.has('organization:admin')
        "
      />
      <EmptyState
        v-else-if="
          !session.selectedProject && !['system', 'organization'].includes(String($route.name))
        "
        icon="blocked"
        title="No accessible projects"
        description="Your account is valid, but an organization administrator must grant access to a project."
      />
      <RouterView v-else />
    </main>
  </div>
</template>
