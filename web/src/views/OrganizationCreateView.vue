<script setup lang="ts">
import { computed, reactive, ref } from 'vue';
import { useMutation } from '@tanstack/vue-query';
import { useRouter } from 'vue-router';
import { api } from '../api/client';
import { suggestedSlug } from '../lib/slug';
import { useSessionStore } from '../stores/session';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';

const session = useSessionStore();
const router = useRouter();
const canCreateOrganization = computed(() => session.has('organization:admin'));
const slugWasEdited = ref(false);
const organization = reactive({ display_name: '', slug: '' });

function updateName(event: Event): void {
  if (!(event.target instanceof HTMLInputElement)) return;
  organization.display_name = event.target.value;
  if (!slugWasEdited.value) organization.slug = suggestedSlug(event.target.value);
}

const createOrganization = useMutation({
  mutationFn: () => api.createOrganization(organization.display_name, organization.slug),
  onSuccess: async (created) => {
    await session.refreshOrganizations();
    await session.selectOrganization(created.id);
    await router.replace('/projects/new');
  },
});
</script>

<template>
  <section v-if="!canCreateOrganization">
    <EmptyState
      icon="blocked"
      :title="$t('organization.createRestricted')"
      :description="$t('organization.createRestrictedDescription')"
    />
  </section>
  <section v-else class="onboarding-layout" aria-labelledby="organization-create-title">
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ $t('organization.createEyebrow') }}</p>
        <h1 id="organization-create-title">{{ $t('organization.createTitle') }}</h1>
        <p>{{ $t('organization.createDescription') }}</p>
      </div>
    </header>

    <ApiErrorPanel
      v-if="createOrganization.error.value"
      :error="createOrganization.error.value"
      :title="$t('organization.createFailed')"
      @retry="createOrganization.mutate()"
    />

    <form class="panel settings-form" @submit.prevent="createOrganization.mutate()">
      <div class="section-heading">
        <div class="section-heading__content">
          <span class="section-icon section-icon--info">
            <AppIcon name="organization" :size="18" />
          </span>
          <div>
            <p class="eyebrow">{{ $t('organization.eyebrow') }}</p>
            <h2>{{ $t('organization.createTitle') }}</h2>
          </div>
        </div>
      </div>

      <div class="form-grid">
        <label>
          {{ $t('organization.organizationName') }}
          <input
            :value="organization.display_name"
            autocomplete="organization"
            maxlength="128"
            placeholder="Acme"
            required
            @input="updateName"
          />
        </label>
        <label>
          {{ $t('organization.organizationSlug') }}
          <input
            v-model.trim="organization.slug"
            autocomplete="off"
            maxlength="64"
            pattern="[a-z0-9]+(?:-[a-z0-9]+)*"
            placeholder="acme"
            required
            @input="slugWasEdited = true"
          />
          <small>{{ $t('organization.organizationSlugHelp') }}</small>
        </label>
      </div>

      <button
        class="button button--primary"
        type="submit"
        :disabled="createOrganization.isPending.value"
      >
        <AppIcon :name="createOrganization.isPending.value ? 'loading' : 'plus'" :size="16" />
        {{
          createOrganization.isPending.value
            ? $t('organization.creatingOrganization')
            : $t('organization.createOrganization')
        }}
      </button>
    </form>
  </section>
</template>
