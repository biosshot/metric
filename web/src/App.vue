<script setup lang="ts">
import { onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import AuthView from './views/AuthView.vue';
import LoadingPanel from './components/LoadingPanel.vue';
import ApiErrorPanel from './components/ApiErrorPanel.vue';
import EmptyState from './components/EmptyState.vue';
import FirstProjectOnboarding from './components/FirstProjectOnboarding.vue';
import { useSessionStore } from './stores/session';

const session = useSessionStore();
const router = useRouter();
const navigationOpen = ref(false);
const logoutError = ref<unknown>(null);

onMounted(() => session.restore());

function changeProject(): void {
  session.selectProject(session.selectedProjectId);
  navigationOpen.value = false;
  router.push('/issues');
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
      Open sign-in instead
    </button>
  </div>
  <AuthView v-else-if="!session.authenticated" />
  <div v-else class="app-shell">
    <header class="mobile-header">
      <button
        class="icon-button"
        type="button"
        aria-label="Toggle navigation"
        :aria-expanded="navigationOpen"
        @click="navigationOpen = !navigationOpen"
      >
        ☰
      </button>
      <strong>Faultkeep</strong>
    </header>
    <aside class="sidebar" :class="{ 'sidebar--open': navigationOpen }">
      <div class="sidebar__brand">
        <span class="brand-mark brand-mark--small" aria-hidden="true">F</span>
        <strong>faultkeep</strong>
      </div>
      <label class="project-switcher">
        <span>Project</span>
        <select v-model="session.selectedProjectId" @change="changeProject">
          <option v-for="project in session.projects" :key="project.id" :value="project.id">
            {{ project.display_name }}
          </option>
        </select>
      </label>
      <nav aria-label="Primary">
        <RouterLink to="/issues" @click="navigationOpen = false">
          <span aria-hidden="true">◆</span> Issues
        </RouterLink>
        <RouterLink to="/project/setup" @click="navigationOpen = false">
          <span aria-hidden="true">⌁</span> SDK setup
        </RouterLink>
        <RouterLink to="/project/settings" @click="navigationOpen = false">
          <span aria-hidden="true">⚙</span> Project settings
        </RouterLink>
        <RouterLink to="/system" @click="navigationOpen = false">
          <span aria-hidden="true">●</span> System status
        </RouterLink>
      </nav>
      <div class="sidebar__account">
        <div>
          <strong>{{ session.identity?.role }}</strong>
          <span>Org {{ session.organizationId }}</span>
        </div>
        <button type="button" @click="logout">Sign out</button>
      </div>
    </aside>
    <main id="main-content" class="main-content">
      <ApiErrorPanel v-if="logoutError" :error="logoutError" title="Sign out failed" />
      <FirstProjectOnboarding
        v-if="
          !session.selectedProject && $route.name !== 'system' && session.has('organization:admin')
        "
      />
      <EmptyState
        v-else-if="!session.selectedProject && $route.name !== 'system'"
        title="No accessible projects"
        description="Your account is valid, but an organization administrator must grant access to a project."
      />
      <RouterView v-else />
    </main>
  </div>
</template>
