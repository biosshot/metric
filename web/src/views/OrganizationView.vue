<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { computed, reactive, ref, watch } from 'vue';
import { api } from '../api/client';
import type { CreatedInvitation, OrganizationMember, OrganizationRole } from '../api/types';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import CodeBlock from '../components/CodeBlock.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import { useSessionStore } from '../stores/session';
import ApiTokensView from './ApiTokensView.vue';
import { organizationInvitationUrl } from './organizationInvitation';

const session = useSessionStore();
const queryClient = useQueryClient();
const inviteEmail = ref('');
const inviteName = ref('');
const inviteRole = ref<OrganizationRole>('member');
const invitation = ref<CreatedInvitation | null>(null);
const activeMemberId = ref<string | null>(null);
const roleDrafts = reactive<Record<string, OrganizationRole>>({});

const organization = useQuery({
  queryKey: ['organization'],
  queryFn: api.organization,
});
const members = useQuery({
  queryKey: ['organization-members'],
  queryFn: api.organizationMembers,
  enabled: computed(() => session.has('organization:admin')),
});
const audit = useQuery({
  queryKey: ['organization-audit'],
  queryFn: api.organizationAudit,
  enabled: computed(() => session.has('organization:admin')),
});

watch(
  () => members.data.value?.items,
  (items) => {
    for (const member of items ?? []) roleDrafts[member.user_id] = member.role;
  },
  { immediate: true },
);

const roleOptions = computed<SelectOption[]>(() => {
  const options: SelectOption[] = [
    { value: 'viewer', label: 'Viewer', description: 'Read-only investigation access.' },
    { value: 'member', label: 'Member', description: 'Can update ordinary Issues.' },
    { value: 'admin', label: 'Admin', description: 'Can manage projects and members.' },
  ];
  if (session.has('organization:owner')) {
    options.push({ value: 'owner', label: 'Owner', description: 'Full organization control.' });
  }
  return options;
});

function roleOptionsFor(member: OrganizationMember): SelectOption[] {
  if (member.role === 'owner' && !roleOptions.value.some((option) => option.value === 'owner')) {
    return [
      ...roleOptions.value,
      { value: 'owner', label: 'Owner', description: 'Only another owner can change this role.' },
    ];
  }
  return roleOptions.value;
}

function canManage(member: OrganizationMember): boolean {
  return member.role !== 'owner' || session.has('organization:owner');
}

const invite = useMutation({
  mutationFn: () =>
    api.inviteOrganizationMember(inviteEmail.value, inviteName.value, inviteRole.value),
  onSuccess: async (result) => {
    invitation.value = result;
    inviteEmail.value = '';
    inviteName.value = '';
    inviteRole.value = 'member';
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ['organization-members'] }),
      queryClient.invalidateQueries({ queryKey: ['organization-audit'] }),
    ]);
  },
});

const updateMember = useMutation({
  mutationFn: ({
    userId,
    action,
    role,
  }: {
    userId: string;
    action: 'change_role' | 'disable' | 'enable' | 'remove';
    role?: OrganizationRole;
  }) => api.updateOrganizationMember(userId, action, role),
  onMutate: ({ userId }) => {
    activeMemberId.value = userId;
  },
  onSuccess: async () => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ['organization-members'] }),
      queryClient.invalidateQueries({ queryKey: ['organization-audit'] }),
    ]);
  },
  onSettled: () => {
    activeMemberId.value = null;
  },
});

const setupLink = computed(() => {
  if (!invitation.value) return '';
  return organizationInvitationUrl(window.location.origin, invitation.value);
});

function saveRole(member: OrganizationMember): void {
  const role = roleDrafts[member.user_id];
  if (!role || role === member.role) return;
  updateMember.mutate({ userId: member.user_id, action: 'change_role', role });
}

function removeMember(member: OrganizationMember): void {
  if (!window.confirm(`Remove ${member.display_name} from this organization?`)) return;
  updateMember.mutate({ userId: member.user_id, action: 'remove' });
}

function formatTimestamp(value: string): string {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}

function actionLabel(action: string): string {
  return action.replaceAll('.', ' · ').replaceAll('_', ' ');
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">Organization</p>
        <h1>{{ organization.data.value?.display_name ?? 'Organization' }}</h1>
        <p>Identity, access, personal automation credentials, and administrative history.</p>
      </div>
    </header>

    <LoadingPanel v-if="organization.isPending.value" label="Loading organization…" />
    <ApiErrorPanel
      v-else-if="organization.error.value"
      :error="organization.error.value"
      title="Organization could not be loaded"
      @retry="organization.refetch()"
    />
    <section v-else-if="organization.data.value" class="organization-summary">
      <article class="summary-card summary-card--info">
        <AppIcon name="organization" :size="20" />
        <span>Name</span>
        <strong>{{ organization.data.value.display_name }}</strong>
        <small>{{ organization.data.value.slug }}</small>
      </article>
      <article class="summary-card">
        <AppIcon name="shield" :size="20" />
        <span>Your access</span>
        <strong>{{ session.identity?.role }}</strong>
        <small>{{ session.identity?.permissions.length }} permissions</small>
      </article>
      <article class="summary-card">
        <AppIcon name="bug" :size="20" />
        <span>Projects</span>
        <strong>{{ session.projects.length }}</strong>
        <small>Organization-wide access</small>
      </article>
      <article class="summary-card">
        <AppIcon name="history" :size="20" />
        <span>Created</span>
        <strong>{{ formatTimestamp(organization.data.value.created_at) }}</strong>
        <small>ID {{ organization.data.value.id }}</small>
      </article>
    </section>

    <template v-if="session.has('organization:admin')">
      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Access control</p>
            <h2>Members</h2>
            <p class="muted">Roles apply to every project in this organization.</p>
          </div>
          <span class="section-icon section-icon--success" aria-hidden="true">
            <AppIcon name="users" :size="20" />
          </span>
        </div>

        <ApiErrorPanel
          v-if="updateMember.error.value"
          :error="updateMember.error.value"
          title="Member was not updated"
        />
        <LoadingPanel v-if="members.isPending.value" label="Loading members…" />
        <ApiErrorPanel
          v-else-if="members.error.value"
          :error="members.error.value"
          title="Members could not be loaded"
          @retry="members.refetch()"
        />
        <div v-else class="member-list">
          <article v-for="member in members.data.value?.items" :key="member.user_id">
            <div class="member-identity">
              <div>
                <strong>{{ member.display_name }}</strong>
                <span>{{ member.email }}</span>
              </div>
              <StatusBadge :status="member.disabled_at ? 'disabled' : 'active'" />
            </div>
            <div class="member-controls">
              <BaseSelect
                v-model="roleDrafts[member.user_id]"
                :options="roleOptionsFor(member)"
                :aria-label="`Role for ${member.display_name}`"
                :disabled="activeMemberId === member.user_id || !canManage(member)"
              />
              <button
                class="button button--secondary"
                type="button"
                :disabled="
                  activeMemberId === member.user_id ||
                  roleDrafts[member.user_id] === member.role ||
                  !canManage(member)
                "
                @click="saveRole(member)"
              >
                <AppIcon name="save" :size="15" />
                Save role
              </button>
              <button
                class="button button--secondary"
                type="button"
                :disabled="activeMemberId === member.user_id || !canManage(member)"
                @click="
                  updateMember.mutate({
                    userId: member.user_id,
                    action: member.disabled_at ? 'enable' : 'disable',
                  })
                "
              >
                <AppIcon :name="member.disabled_at ? 'success' : 'blocked'" :size="15" />
                {{ member.disabled_at ? 'Enable' : 'Disable' }}
              </button>
              <button
                class="button button--danger"
                type="button"
                :disabled="activeMemberId === member.user_id || !canManage(member)"
                @click="removeMember(member)"
              >
                <AppIcon name="delete" :size="15" />
                Remove
              </button>
            </div>
          </article>
        </div>
      </section>

      <form class="panel settings-form" @submit.prevent="invite.mutate()">
        <div class="section-heading">
          <div>
            <p class="eyebrow">One-time setup</p>
            <h2>Invite member</h2>
            <p class="muted">Metric shows the password-setup link once. Send it securely.</p>
          </div>
          <AppIcon name="userPlus" :size="20" />
        </div>
        <ApiErrorPanel
          v-if="invite.error.value"
          :error="invite.error.value"
          title="Invitation was not created"
        />
        <div class="form-grid form-grid--three">
          <label>
            Display name
            <input v-model.trim="inviteName" required maxlength="128" autocomplete="off" />
          </label>
          <label>
            Email
            <input v-model.trim="inviteEmail" required type="email" autocomplete="off" />
          </label>
          <BaseSelect v-model="inviteRole" :options="roleOptions" label="Role" />
        </div>
        <button
          class="button button--primary"
          type="submit"
          :disabled="invite.isPending.value || !inviteName || !inviteEmail"
        >
          <AppIcon name="userPlus" :size="16" />
          {{ invite.isPending.value ? 'Creating…' : 'Create invitation' }}
        </button>
      </form>

      <section v-if="invitation" class="panel token-secret-panel" aria-live="polite">
        <div class="section-heading">
          <div>
            <p class="eyebrow">Copy now</p>
            <h2>Password-setup link</h2>
            <p class="muted">It expires according to the server setup-token policy.</p>
          </div>
          <button
            class="icon-button"
            type="button"
            aria-label="Hide invitation"
            @click="invitation = null"
          >
            <AppIcon name="close" :size="18" />
          </button>
        </div>
        <CodeBlock :code="setupLink" language="text" title="Invitation URL" />
      </section>
    </template>

    <section v-else class="panel">
      <EmptyState
        icon="shield"
        title="Member administration is restricted"
        description="Your role can inspect projects, but organization membership and security history require organization:admin."
      />
    </section>

    <section class="organization-section">
      <div class="section-heading organization-section__heading">
        <div>
          <p class="eyebrow">Personal access</p>
          <h2>API tokens</h2>
        </div>
        <span class="section-icon section-icon--warning" aria-hidden="true">
          <AppIcon name="key" :size="20" />
        </span>
      </div>
      <ApiTokensView embedded />
    </section>

    <section v-if="session.has('organization:admin')" class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">Security history</p>
          <h2>Recent audit log</h2>
          <p class="muted">The latest 100 bounded administrative and security records.</p>
        </div>
        <span class="section-icon section-icon--info" aria-hidden="true">
          <AppIcon name="history" :size="20" />
        </span>
      </div>
      <LoadingPanel v-if="audit.isPending.value" label="Loading audit log…" />
      <ApiErrorPanel
        v-else-if="audit.error.value"
        :error="audit.error.value"
        title="Audit log could not be loaded"
        @retry="audit.refetch()"
      />
      <EmptyState
        v-else-if="!audit.data.value?.items.length"
        icon="history"
        title="No audit records"
        description="Administrative actions will appear here with their request IDs."
      />
      <div v-else class="audit-list">
        <article v-for="record in audit.data.value?.items" :key="record.request_id">
          <span class="audit-list__marker" aria-hidden="true"></span>
          <div>
            <strong>{{ actionLabel(record.action) }}</strong>
            <span>{{ record.target_kind }} {{ record.target_id }}</span>
            <small>
              {{ formatTimestamp(record.timestamp) }} · actor {{ record.actor_user_id }} · request
              {{ record.request_id }}
            </small>
          </div>
        </article>
      </div>
    </section>
  </section>
</template>
