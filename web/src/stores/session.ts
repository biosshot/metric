import { computed, ref } from 'vue';
import { defineStore } from 'pinia';
import { ApiError, api, configureSession } from '../api/client';
import type { Identity, Project, UserOrganization } from '../api/types';

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
  const organizations = ref<UserOrganization[]>([]);
  const projectsByOrganization = ref<Record<string, Project[]>>({});
  const restoring = ref(true);
  const restoreError = ref<unknown>(null);

  configureSession(
    () => ({
      organizationId: organizationId.value,
      csrfToken: csrfToken.value,
    }),
    clear,
  );

  const authenticated = computed(() => identity.value !== null);
  const selectedProject = computed(
    () => projects.value.find((project) => project.id === selectedProjectId.value) ?? null,
  );
  const activeOrganization = computed(
    () =>
      organizations.value.find((organization) => organization.id === organizationId.value) ?? null,
  );
  const workspaces = computed(() =>
    organizations.value.map((organization) => ({
      organization,
      projects: projectsByOrganization.value[organization.id] ?? [],
    })),
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
      await refreshOrganizations();
    } catch (error) {
      if (isInvalidSession(error)) {
        clear();
      } else {
        restoreError.value = error;
      }
    } finally {
      restoring.value = false;
    }
  }

  async function login(email: string, password: string): Promise<void> {
    const preferredOrganizationId = organizationId.value;
    const preferredProjectId = selectedProjectId.value;
    try {
      const issued = await api.login(email, password);
      organizationId.value = issued.organization_id;
      localStorage.setItem(ORG_KEY, issued.organization_id);
      csrfToken.value = issued.csrf_token;
      localStorage.setItem(CSRF_KEY, issued.csrf_token);
      identity.value = await api.me();
      await refreshOrganizations();
      if (
        preferredOrganizationId &&
        preferredOrganizationId !== issued.organization_id &&
        organizations.value.some((organization) => organization.id === preferredOrganizationId)
      ) {
        const preferredProjects = projectsByOrganization.value[preferredOrganizationId] ?? [];
        const projectId = preferredProjects.some((project) => project.id === preferredProjectId)
          ? preferredProjectId
          : (preferredProjects[0]?.id ?? null);
        if (projectId) await selectWorkspaceProject(projectId, preferredOrganizationId);
        else await selectOrganization(preferredOrganizationId);
      }
    } catch (error) {
      clear();
      throw error;
    }
  }

  async function logout(): Promise<void> {
    try {
      if (csrfToken.value) await api.logout();
    } catch (error) {
      if (!isInvalidSession(error)) throw error;
    } finally {
      clear();
    }
  }

  async function refreshProjects(): Promise<void> {
    const response = await api.projects();
    projects.value = response.items;
    if (organizationId.value) {
      projectsByOrganization.value = {
        ...projectsByOrganization.value,
        [organizationId.value]: response.items,
      };
    }
    if (
      !selectedProjectId.value ||
      !projects.value.some((item) => item.id === selectedProjectId.value)
    ) {
      selectProject(projects.value[0]?.id ?? null);
    }
  }

  async function refreshOrganizations(): Promise<void> {
    const response = await api.organizations();
    organizations.value = response.items;
    const entries = await Promise.all(
      response.items.map(async (organization) => {
        const result = await api.projects(organization.id);
        return [organization.id, result.items] as const;
      }),
    );
    projectsByOrganization.value = Object.fromEntries(entries);
    projects.value = organizationId.value
      ? (projectsByOrganization.value[organizationId.value] ?? [])
      : [];
    if (
      !selectedProjectId.value ||
      !projects.value.some((item) => item.id === selectedProjectId.value)
    ) {
      selectProject(projects.value[0]?.id ?? null);
    }
  }

  async function selectWorkspaceProject(
    projectId: string,
    nextOrganizationId: string,
  ): Promise<void> {
    const previousOrganizationId = organizationId.value;
    const previousProjectId = selectedProjectId.value;
    const previousProjects = projects.value;
    const previousIdentity = identity.value;
    organizationId.value = nextOrganizationId;
    localStorage.setItem(ORG_KEY, nextOrganizationId);
    projects.value = projectsByOrganization.value[nextOrganizationId] ?? [];
    selectProject(projectId);
    try {
      identity.value = await api.me();
    } catch (error) {
      if (csrfToken.value) {
        organizationId.value = previousOrganizationId;
        selectedProjectId.value = previousProjectId;
        projects.value = previousProjects;
        identity.value = previousIdentity;
        persistSelection(previousOrganizationId, previousProjectId);
      }
      throw error;
    }
  }

  async function selectOrganization(nextOrganizationId: string): Promise<void> {
    const previousOrganizationId = organizationId.value;
    const previousProjectId = selectedProjectId.value;
    const previousProjects = projects.value;
    const previousIdentity = identity.value;
    organizationId.value = nextOrganizationId;
    localStorage.setItem(ORG_KEY, nextOrganizationId);
    projects.value = projectsByOrganization.value[nextOrganizationId] ?? [];
    selectProject(projects.value[0]?.id ?? null);
    try {
      identity.value = await api.me();
    } catch (error) {
      if (csrfToken.value) {
        organizationId.value = previousOrganizationId;
        selectedProjectId.value = previousProjectId;
        projects.value = previousProjects;
        identity.value = previousIdentity;
        persistSelection(previousOrganizationId, previousProjectId);
      }
      throw error;
    }
  }

  function persistSelection(organization: string | null, project: string | null): void {
    if (organization) localStorage.setItem(ORG_KEY, organization);
    else localStorage.removeItem(ORG_KEY);
    if (project) localStorage.setItem(PROJECT_KEY, project);
    else localStorage.removeItem(PROJECT_KEY);
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
    organizations.value = [];
    projectsByOrganization.value = {};
    localStorage.removeItem(CSRF_KEY);
    sessionStorage.removeItem(CSRF_KEY);
  }

  function dismissRestoreError(): void {
    restoreError.value = null;
  }

  function isInvalidSession(error: unknown): boolean {
    return (
      error instanceof ApiError &&
      (error.status === 401 ||
        error.code === 'invalid_credentials' ||
        error.code === 'csrf_failed' ||
        error.code === 'csrf_missing')
    );
  }

  return {
    identity,
    organizationId,
    csrfToken,
    projects,
    organizations,
    activeOrganization,
    workspaces,
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
    refreshOrganizations,
    selectProject,
    selectWorkspaceProject,
    selectOrganization,
    clear,
    dismissRestoreError,
  };
});
