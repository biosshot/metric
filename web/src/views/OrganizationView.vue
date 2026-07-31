<script setup lang="ts">
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { computed, reactive, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
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
const { locale, t } = useI18n();
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
    { value: 'viewer', label: t('organization.viewer'), description: t('organization.viewerHelp') },
    { value: 'member', label: t('organization.member'), description: t('organization.memberHelp') },
    { value: 'admin', label: t('organization.admin'), description: t('organization.adminHelp') },
  ];
  if (session.has('organization:owner')) {
    options.push({
      value: 'owner',
      label: t('organization.owner'),
      description: t('organization.ownerHelp'),
    });
  }
  return options;
});

function roleOptionsFor(member: OrganizationMember): SelectOption[] {
  if (member.role === 'owner' && !roleOptions.value.some((option) => option.value === 'owner')) {
    return [
      ...roleOptions.value,
      {
        value: 'owner',
        label: t('organization.owner'),
        description: t('organization.ownerLocked'),
      },
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
  if (!window.confirm(t('organization.removeConfirm', { name: member.display_name }))) return;
  updateMember.mutate({ userId: member.user_id, action: 'remove' });
}

function formatTimestamp(value: string): string {
  return new Intl.DateTimeFormat(locale.value, {
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
        <p class="eyebrow">{{ $t('organization.eyebrow') }}</p>
        <h1>{{ organization.data.value?.display_name ?? $t('organization.fallbackName') }}</h1>
        <p>{{ $t('organization.description') }}</p>
      </div>
    </header>

    <LoadingPanel v-if="organization.isPending.value" :label="$t('organization.loading')" />
    <ApiErrorPanel
      v-else-if="organization.error.value"
      :error="organization.error.value"
      :title="$t('organization.loadFailed')"
      @retry="organization.refetch()"
    />
    <section v-else-if="organization.data.value" class="organization-summary">
      <article class="summary-card summary-card--info">
        <AppIcon name="organization" :size="20" />
        <span>{{ $t('organization.name') }}</span>
        <strong>{{ organization.data.value.display_name }}</strong>
        <small>{{ organization.data.value.slug }}</small>
      </article>
      <article class="summary-card">
        <AppIcon name="shield" :size="20" />
        <span>{{ $t('organization.yourAccess') }}</span>
        <strong>{{
          session.identity?.role ? $t(`organization.${session.identity.role}`) : '—'
        }}</strong>
        <small>{{
          $t('organization.permissions', session.identity?.permissions.length ?? 0)
        }}</small>
      </article>
      <article class="summary-card">
        <AppIcon name="bug" :size="20" />
        <span>{{ $t('organization.projects') }}</span>
        <strong>{{ session.projects.length.toLocaleString(locale) }}</strong>
        <small>{{ $t('organization.organizationWide') }}</small>
      </article>
      <article class="summary-card">
        <AppIcon name="history" :size="20" />
        <span>{{ $t('organization.created') }}</span>
        <strong>{{ formatTimestamp(organization.data.value.created_at) }}</strong>
        <small>ID {{ organization.data.value.id }}</small>
      </article>
    </section>

    <template v-if="session.has('organization:admin')">
      <section class="panel">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('organization.accessControl') }}</p>
            <h2>{{ $t('organization.members') }}</h2>
            <p class="muted">{{ $t('organization.rolesHelp') }}</p>
          </div>
          <span class="section-icon section-icon--success" aria-hidden="true">
            <AppIcon name="users" :size="20" />
          </span>
        </div>

        <ApiErrorPanel
          v-if="updateMember.error.value"
          :error="updateMember.error.value"
          :title="$t('organization.updateFailed')"
        />
        <LoadingPanel v-if="members.isPending.value" :label="$t('organization.loadingMembers')" />
        <ApiErrorPanel
          v-else-if="members.error.value"
          :error="members.error.value"
          :title="$t('organization.membersFailed')"
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
                :aria-label="$t('organization.roleFor', { name: member.display_name })"
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
                {{ $t('organization.saveRole') }}
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
                {{ member.disabled_at ? $t('organization.enable') : $t('organization.disable') }}
              </button>
              <button
                class="button button--danger"
                type="button"
                :disabled="activeMemberId === member.user_id || !canManage(member)"
                @click="removeMember(member)"
              >
                <AppIcon name="delete" :size="15" />
                {{ $t('organization.remove') }}
              </button>
            </div>
          </article>
        </div>
      </section>

      <form class="panel settings-form" @submit.prevent="invite.mutate()">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('organization.oneTimeSetup') }}</p>
            <h2>{{ $t('organization.invite') }}</h2>
            <p class="muted">{{ $t('organization.inviteHelp') }}</p>
          </div>
          <AppIcon name="userPlus" :size="20" />
        </div>
        <ApiErrorPanel
          v-if="invite.error.value"
          :error="invite.error.value"
          :title="$t('organization.invitationFailed')"
        />
        <div class="form-grid form-grid--three">
          <label>
            {{ $t('organization.displayName') }}
            <input v-model.trim="inviteName" required maxlength="128" autocomplete="off" />
          </label>
          <label>
            {{ $t('organization.email') }}
            <input v-model.trim="inviteEmail" required type="email" autocomplete="off" />
          </label>
          <BaseSelect
            v-model="inviteRole"
            :options="roleOptions"
            :label="$t('organization.role')"
          />
        </div>
        <button
          class="button button--primary"
          type="submit"
          :disabled="invite.isPending.value || !inviteName || !inviteEmail"
        >
          <AppIcon name="userPlus" :size="16" />
          {{
            invite.isPending.value
              ? $t('organization.creating')
              : $t('organization.createInvitation')
          }}
        </button>
      </form>

      <section v-if="invitation" class="panel token-secret-panel" aria-live="polite">
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('organization.copyNow') }}</p>
            <h2>{{ $t('organization.setupLink') }}</h2>
            <p class="muted">{{ $t('organization.setupLinkHelp') }}</p>
          </div>
          <button
            class="icon-button"
            type="button"
            :aria-label="$t('organization.hideInvitation')"
            @click="invitation = null"
          >
            <AppIcon name="close" :size="18" />
          </button>
        </div>
        <CodeBlock :code="setupLink" language="text" :title="$t('organization.invitationUrl')" />
      </section>
    </template>

    <section v-else class="panel">
      <EmptyState
        icon="shield"
        :title="$t('organization.restricted')"
        :description="$t('organization.restrictedDescription')"
      />
    </section>

    <section class="organization-section">
      <div class="section-heading organization-section__heading">
        <div>
          <p class="eyebrow">{{ $t('organization.personalAccess') }}</p>
          <h2>{{ $t('organization.apiTokens') }}</h2>
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
          <p class="eyebrow">{{ $t('organization.securityHistory') }}</p>
          <h2>{{ $t('organization.auditLog') }}</h2>
          <p class="muted">{{ $t('organization.auditHelp') }}</p>
        </div>
        <span class="section-icon section-icon--info" aria-hidden="true">
          <AppIcon name="history" :size="20" />
        </span>
      </div>
      <LoadingPanel v-if="audit.isPending.value" :label="$t('organization.loadingAudit')" />
      <ApiErrorPanel
        v-else-if="audit.error.value"
        :error="audit.error.value"
        :title="$t('organization.auditFailed')"
        @retry="audit.refetch()"
      />
      <EmptyState
        v-else-if="!audit.data.value?.items.length"
        icon="history"
        :title="$t('organization.noAudit')"
        :description="$t('organization.noAuditDescription')"
      />
      <div v-else class="audit-list">
        <article v-for="record in audit.data.value?.items" :key="record.request_id">
          <span class="audit-list__marker" aria-hidden="true"></span>
          <div>
            <strong>{{ actionLabel(record.action) }}</strong>
            <span>{{ record.target_kind }} {{ record.target_id }}</span>
            <small>{{
              $t('organization.auditRecord', {
                time: formatTimestamp(record.timestamp),
                actor: record.actor_user_id,
                request: record.request_id,
              })
            }}</small>
          </div>
        </article>
      </div>
    </section>
  </section>
</template>
