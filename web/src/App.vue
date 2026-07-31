<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRouter } from 'vue-router';
import ApiErrorPanel from './components/ApiErrorPanel.vue';
import AppIcon from './components/AppIcon.vue';
import BaseSelect, { type SelectOption } from './components/BaseSelect.vue';
import EmptyState from './components/EmptyState.vue';
import FirstProjectOnboarding from './components/FirstProjectOnboarding.vue';
import LoadingPanel from './components/LoadingPanel.vue';
import LocaleSelect from './components/LocaleSelect.vue';
import LogoMark from './components/LogoMark.vue';
import { useSessionStore } from './stores/session';
import AuthView from './views/AuthView.vue';

const session = useSessionStore();
const router = useRouter();
const { t } = useI18n();
const navigationOpen = ref(false);
const logoutError = ref<unknown>(null);
const createProjectAction = '__create_project__';
const routesWithoutProject = new Set(['settings-system', 'settings-organization', 'not-found']);
const projectOptions = computed<SelectOption[]>(() => [
  ...session.projects.map((project) => ({
    value: project.id,
    label: project.display_name,
    icon: 'bug' as const,
  })),
  ...(session.has('organization:admin')
    ? [
        {
          value: createProjectAction,
          label: t('app.newProject'),
          description: t('app.newProjectDescription'),
          icon: 'plus' as const,
          action: true,
        },
      ]
    : []),
]);

onMounted(() => session.restore());

function changeProject(projectId: string): void {
  if (projectId === createProjectAction) {
    navigationOpen.value = false;
    void router.push('/projects/new');
    return;
  }
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
  <a class="skip-link" href="#main-content">{{ $t('app.skipToContent') }}</a>
  <div v-if="session.restoring" class="app-loading">
    <LoadingPanel :label="$t('app.restoringSession')" />
  </div>
  <div v-else-if="session.restoreError" class="app-loading restore-failure">
    <ApiErrorPanel
      :error="session.restoreError"
      :title="$t('app.sessionCheckFailed')"
      @retry="session.restore()"
    />
    <button class="button button--secondary" type="button" @click="session.dismissRestoreError()">
      <AppIcon name="back" :size="16" />
      {{ $t('app.openSignIn') }}
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
        :aria-label="$t('app.toggleNavigation')"
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
      :aria-label="$t('app.closeNavigation')"
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
          :label="$t('app.project')"
          @update:model-value="changeProject"
        />
        <LocaleSelect class="sidebar__locale" />
      </div>

      <nav :aria-label="$t('app.primaryNavigation')">
        <div class="sidebar__nav-group">
          <span class="sidebar__nav-label">{{ $t('app.overview') }}</span>
          <RouterLink to="/dashboard" @click="navigationOpen = false">
            <AppIcon name="dashboard" :size="18" />
            {{ $t('app.dashboard') }}
          </RouterLink>
        </div>

        <div class="sidebar__nav-group">
          <span class="sidebar__nav-label">{{ $t('app.observe') }}</span>
          <RouterLink to="/issues" @click="navigationOpen = false">
            <AppIcon name="clipboard" :size="18" />
            {{ $t('app.issues') }}
          </RouterLink>
          <RouterLink to="/logs" @click="navigationOpen = false">
            <AppIcon name="logs" :size="18" />
            {{ $t('app.logs') }}
          </RouterLink>
          <RouterLink
            to="/metrics"
            :class="{ 'router-link-active': $route.name === 'metrics-query' }"
            @click="navigationOpen = false"
          >
            <AppIcon name="gauge" :size="18" />
            {{ $t('app.metrics') }}
          </RouterLink>
          <RouterLink
            to="/traces"
            :class="{ 'router-link-active': $route.name === 'performance' }"
            @click="navigationOpen = false"
          >
            <AppIcon name="traces" :size="18" />
            {{ $t('app.traces') }}
          </RouterLink>
          <RouterLink to="/replays" @click="navigationOpen = false">
            <AppIcon name="replay" :size="18" />
            {{ $t('app.replays') }}
          </RouterLink>
          <RouterLink to="/monitors" @click="navigationOpen = false">
            <AppIcon name="monitors" :size="18" />
            {{ $t('app.monitors') }}
          </RouterLink>
          <RouterLink to="/feedback" @click="navigationOpen = false">
            <AppIcon name="message" :size="18" />
            {{ $t('app.feedback') }}
          </RouterLink>
        </div>

        <div class="sidebar__nav-group">
          <span class="sidebar__nav-label">{{ $t('app.delivery') }}</span>
          <RouterLink to="/releases" @click="navigationOpen = false">
            <AppIcon name="release" :size="18" />
            {{ $t('app.releases') }}
          </RouterLink>
        </div>

        <div class="sidebar__nav-group">
          <span class="sidebar__nav-label">{{ $t('app.configure') }}</span>
          <RouterLink
            v-if="session.has('project:admin')"
            to="/project/setup"
            @click="navigationOpen = false"
          >
            <AppIcon name="connect" :size="18" />
            {{ $t('app.sdkSetup') }}
          </RouterLink>
          <RouterLink to="/settings" @click="navigationOpen = false">
            <AppIcon name="settings" :size="18" />
            {{ $t('app.settings') }}
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
          <span>{{ $t('app.organizationShort', { id: session.organizationId }) }}</span>
        </RouterLink>
        <button type="button" @click="logout">
          <AppIcon name="signOut" :size="16" />
          {{ $t('app.signOut') }}
        </button>
      </div>
    </aside>

    <main id="main-content" class="main-content">
      <ApiErrorPanel v-if="logoutError" :error="logoutError" :title="$t('app.signOutFailed')" />
      <FirstProjectOnboarding
        v-if="
          !session.selectedProject &&
          !routesWithoutProject.has(String($route.name)) &&
          session.has('organization:admin')
        "
      />
      <EmptyState
        v-else-if="!session.selectedProject && !routesWithoutProject.has(String($route.name))"
        icon="blocked"
        :title="$t('app.noProjects')"
        :description="$t('app.noProjectsDescription')"
      />
      <RouterView v-else />
    </main>
  </div>
</template>
