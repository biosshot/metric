import { computed, ref } from 'vue';
import { defineStore } from 'pinia';
import { ApiError, api, configureSession } from '../api/client';
import type { Identity, Project } from '../api/types';

const ORG_KEY = 'metric.organization';
const CSRF_KEY = 'metric.csrf';
const PROJECT_KEY = 'metric.project';

export const useSessionStore = defineStore('session', () => {
  const legacyTabCsrf = sessionStorage.getItem(CSRF_KEY);
  const persistedCsrf = localStorage.getItem(CSRF_KEY) ?? legacyTabCsrf;
  if (persistedCsrf && !localStorage.getItem(CSRF_KEY)) {
    localStorage.setItem(CSRF_KEY, persistedCsrf);
  }
  sessionStorage.removeItem(CSRF_KEY);

  const identity = ref<Identity | null>(null);
  const organizationId = ref(localStorage.getItem(ORG_KEY));
  const csrfToken = ref(persistedCsrf);
  const selectedProjectId = ref(localStorage.getItem(PROJECT_KEY));
  const projects = ref<Project[]>([]);
  const restoring = ref(true);
  const restoreError = ref<unknown>(null);

  configureSession(() => ({
    organizationId: organizationId.value,
    csrfToken: csrfToken.value,
  }));

  const authenticated = computed(() => identity.value !== null);
  const selectedProject = computed(
    () => projects.value.find((project) => project.id === selectedProjectId.value) ?? null,
  );

  function has(permission: string): boolean {
    return identity.value?.permissions.includes(permission) ?? false;
  }

  async function restore(): Promise<void> {
    restoring.value = true;
    restoreError.value = null;
    if (!organizationId.value) {
      restoring.value = false;
      return;
    }
    if (!csrfToken.value) {
      identity.value = null;
      projects.value = [];
      restoring.value = false;
      return;
    }
    try {
      identity.value = await api.me();
      await refreshProjects();
    } catch (error) {
      if (error instanceof ApiError && (error.status === 401 || error.status === 403)) {
        clear();
      } else {
        restoreError.value = error;
      }
    } finally {
      restoring.value = false;
    }
  }

  async function login(email: string, password: string, orgId: string): Promise<void> {
    organizationId.value = orgId;
    localStorage.setItem(ORG_KEY, orgId);
    try {
      const issued = await api.login(email, password, orgId);
      csrfToken.value = issued.csrf_token;
      localStorage.setItem(CSRF_KEY, issued.csrf_token);
      identity.value = await api.me();
      await refreshProjects();
    } catch (error) {
      clear();
      throw error;
    }
  }

  async function logout(): Promise<void> {
    if (csrfToken.value) await api.logout();
    clear();
  }

  async function refreshProjects(): Promise<void> {
    const response = await api.projects();
    projects.value = response.items;
    if (
      !selectedProjectId.value ||
      !projects.value.some((item) => item.id === selectedProjectId.value)
    ) {
      selectProject(projects.value[0]?.id ?? null);
    }
  }

  function selectProject(projectId: string | null): void {
    selectedProjectId.value = projectId;
    if (projectId) localStorage.setItem(PROJECT_KEY, projectId);
    else localStorage.removeItem(PROJECT_KEY);
  }

  function clear(): void {
    identity.value = null;
    csrfToken.value = null;
    projects.value = [];
    localStorage.removeItem(CSRF_KEY);
    sessionStorage.removeItem(CSRF_KEY);
  }

  function dismissRestoreError(): void {
    restoreError.value = null;
  }

  return {
    identity,
    organizationId,
    csrfToken,
    projects,
    selectedProjectId,
    selectedProject,
    authenticated,
    restoring,
    restoreError,
    has,
    restore,
    login,
    logout,
    refreshProjects,
    selectProject,
    clear,
    dismissRestoreError,
  };
});
