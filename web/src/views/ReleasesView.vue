<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute } from 'vue-router';
import { useMutation, useQuery, useQueryClient } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import UnifiedQueryBar from '../components/UnifiedQueryBar.vue';
import { useSessionStore } from '../stores/session';
import type { ReleaseSummary } from '../api/types';

const session = useSessionStore();
const route = useRoute();
const queryClient = useQueryClient();
const { locale, t } = useI18n();
const projectId = computed(() => session.selectedProjectId ?? '');
const initialQuery = typeof route.query.q === 'string' ? route.query.q : '';
const query = ref(initialQuery);
const appliedQuery = ref(initialQuery);
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const version = ref('');
const url = ref('');

const releases = useQuery({
  queryKey: computed(() => [
    'unified-query',
    'releases',
    projectId.value,
    appliedQuery.value,
    cursor.value,
  ]),
  queryFn: () =>
    api.query<ReleaseSummary>(projectId.value, {
      source: 'releases',
      query: appliedQuery.value,
      result: { kind: 'records' },
      cursor: cursor.value,
      limit: 50,
    }),
  enabled: computed(() => Boolean(projectId.value)),
});

const createRelease = useMutation({
  mutationFn: () => api.createRelease(projectId.value, version.value.trim(), url.value.trim()),
  onSuccess: async () => {
    version.value = '';
    url.value = '';
    await queryClient.invalidateQueries({
      queryKey: ['unified-query', 'releases', projectId.value],
    });
  },
});

watch(projectId, resetPage);

function submitQuery(): void {
  appliedQuery.value = query.value.trim();
  resetPage();
}

function resetQuery(): void {
  query.value = '';
  appliedQuery.value = '';
  resetPage();
}

function resetPage(): void {
  cursor.value = null;
  history.value = [];
}

function nextPage(): void {
  const next = releases.data.value?.next_cursor;
  if (!next) return;
  history.value.push(cursor.value);
  cursor.value = next;
}

function previousPage(): void {
  cursor.value = history.value.pop() ?? null;
}

function timestamp(value: string | null): string {
  if (!value) return t('releases.noError');
  return new Intl.DateTimeFormat(locale.value, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section class="releases-page">
    <header class="page-header">
      <div>
        <p class="eyebrow">
          {{ $t('releases.eyebrow', { project: session.selectedProject?.slug }) }}
        </p>
        <h1>{{ $t('releases.title') }}</h1>
        <p>{{ $t('releases.description') }}</p>
      </div>
    </header>

    <div class="release-workspace">
      <UnifiedQueryBar
        v-model="query"
        source="releases"
        :show-reset="Boolean(query || appliedQuery)"
        exportable
        @submit="submitQuery"
        @reset="resetQuery"
      />

      <ApiErrorPanel
        v-if="createRelease.error.value"
        :error="createRelease.error.value"
        :title="$t('releases.createFailed')"
      />

      <LoadingPanel v-if="releases.isPending.value" :label="$t('releases.loading')" />
      <ApiErrorPanel
        v-else-if="releases.error.value"
        :error="releases.error.value"
        :title="$t('releases.loadFailed')"
        @retry="releases.refetch()"
      />
      <EmptyState
        v-else-if="!releases.data.value?.items.length"
        icon="release"
        :title="$t('releases.empty')"
        :description="$t('releases.emptyDescription')"
      />
      <div v-else class="release-list">
        <nav class="pagination" aria-label="Release pages">
          <button
            class="button button--secondary"
            type="button"
            :disabled="!history.length"
            @click="previousPage"
          >
            {{ $t('common.previous') }}
          </button>
          <span>{{ $t('common.page', { page: history.length + 1 }) }}</span>
          <button
            class="button button--secondary"
            type="button"
            :disabled="!releases.data.value.next_cursor"
            @click="nextPage"
          >
            {{ $t('common.next') }}
          </button>
        </nav>
        <RouterLink
          v-for="release in releases.data.value.items"
          :key="release.id"
          class="panel release-card"
          :to="`/releases/${release.id}`"
        >
          <span class="release-card__icon"><AppIcon name="release" :size="20" /></span>
          <span>
            <strong>{{ release.version }}</strong>
            <small>
              {{
                release.released_at
                  ? $t('releases.finalized', { time: timestamp(release.released_at) })
                  : $t('releases.open')
              }}
            </small>
          </span>
          <span class="release-card__seen">
            {{ $t('releases.lastError') }}
            <strong>{{ timestamp(release.last_seen) }}</strong>
          </span>
          <AppIcon name="view" :size="18" />
        </RouterLink>
      </div>

      <form
        v-if="session.has('release:write')"
        class="panel release-create"
        @submit.prevent="createRelease.mutate()"
      >
        <div class="section-heading">
          <div>
            <p class="eyebrow">{{ $t('releases.explicit') }}</p>
            <h2>{{ $t('releases.createBeforeDeploy') }}</h2>
          </div>
        </div>
        <label>
          {{ $t('releases.exactVersion') }}
          <input
            v-model.trim="version"
            required
            maxlength="200"
            placeholder="backend@2.4.0"
            autocomplete="off"
          />
        </label>
        <label>
          {{ $t('releases.releaseUrl') }}
          <span class="muted">{{ $t('releases.optional') }}</span>
          <input v-model.trim="url" maxlength="2048" placeholder="https://ci.example/build/1042" />
        </label>
        <button
          class="button button--primary"
          type="submit"
          :disabled="!version || createRelease.isPending.value"
        >
          <AppIcon name="plus" :size="16" />
          {{ createRelease.isPending.value ? $t('releases.creating') : $t('releases.create') }}
        </button>
      </form>
    </div>
  </section>
</template>
