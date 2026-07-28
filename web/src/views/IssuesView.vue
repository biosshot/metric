<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { useRoute } from 'vue-router';
import { useQuery } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import EmptyState from '../components/EmptyState.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';
import AppIcon from '../components/AppIcon.vue';
import BaseSelect, { type SelectOption } from '../components/BaseSelect.vue';
import SdkSetupButton from '../components/SdkSetupButton.vue';
import { useSessionStore } from '../stores/session';
import type { Event, Issue, Page } from '../api/types';

type InvestigationResult = Page<Issue> | (Page<Event> & { candidates_examined: number });

const session = useSessionStore();
const route = useRoute();
const status = ref('');
const submittedStatus = ref('');
const initialSearch = typeof route.query.q === 'string' ? route.query.q : '';
const search = ref(initialSearch);
const submittedSearch = ref(initialSearch);
const cursor = ref<string | null>(null);
const history = ref<(string | null)[]>([]);
const statusOptions: SelectOption[] = [
  { value: '', label: 'All statuses', icon: 'filter' },
  { value: 'open', label: 'Open', icon: 'status' },
  { value: 'resolved', label: 'Resolved', icon: 'success' },
  { value: 'ignored', label: 'Ignored', icon: 'blocked' },
];

const projectId = computed(() => session.selectedProjectId ?? '');
const queryKey = computed(() => [
  submittedSearch.value ? 'event-search' : 'issues',
  projectId.value,
  submittedStatus.value,
  submittedSearch.value,
  cursor.value,
]);

const result = useQuery<InvestigationResult>({
  queryKey,
  queryFn: () =>
    submittedSearch.value
      ? api.search(projectId.value, submittedSearch.value, cursor.value)
      : api.issues(projectId.value, submittedStatus.value || undefined, cursor.value),
  enabled: computed(() => Boolean(projectId.value)),
});
const issueItems = computed(() =>
  submittedSearch.value ? [] : ((result.data.value?.items ?? []) as Issue[]),
);
const eventItems = computed(() =>
  submittedSearch.value ? ((result.data.value?.items ?? []) as Event[]) : [],
);
const candidatesExamined = computed(() => {
  const value = result.data.value;
  return value && 'candidates_examined' in value ? value.candidates_examined : null;
});

watch(projectId, () => resetPage(false));

function submitSearch(): void {
  submittedSearch.value = search.value.trim();
  submittedStatus.value = status.value;
  resetPage(false);
}

function clearSearch(): void {
  search.value = '';
  submittedSearch.value = '';
  resetPage(false);
}

function resetFilters(): void {
  status.value = '';
  submittedStatus.value = '';
  search.value = '';
  submittedSearch.value = '';
  resetPage(false);
}

function nextPage(): void {
  const next = result.data.value?.next_cursor;
  if (!next) return;
  history.value.push(cursor.value);
  cursor.value = next;
}

function previousPage(): void {
  cursor.value = history.value.pop() ?? null;
}

function resetPage(clear = true): void {
  cursor.value = null;
  history.value = [];
  if (clear) {
    search.value = '';
    submittedSearch.value = '';
  }
}

function formatTime(value: string): string {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(new Date(value));
}
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">{{ session.selectedProject?.slug }} / issues</p>
        <h1>Issues</h1>
        <p>Errors grouped by their stable failure signature.</p>
      </div>
    </header>

    <form
      class="signal-toolbar signal-toolbar--issues"
      role="search"
      @submit.prevent="submitSearch"
    >
      <label class="search-field">
        <span class="sr-only">Search events</span>
        <input
          v-model="search"
          type="search"
          placeholder="Search events, for example: environment:production level:error"
          maxlength="4096"
        />
      </label>
      <BaseSelect
        v-if="!submittedSearch"
        v-model="status"
        class="status-filter"
        :options="statusOptions"
        aria-label="Issue status"
      />
      <div class="signal-toolbar__actions">
        <button class="button button--primary" type="submit">
          <AppIcon name="search" :size="16" />
          Search
        </button>
        <button
          v-if="submittedSearch || submittedStatus"
          class="button button--secondary"
          type="button"
          @click="submittedSearch ? clearSearch() : resetFilters()"
        >
          <AppIcon name="close" :size="16" />
          Reset
        </button>
      </div>
    </form>

    <div v-if="submittedSearch" class="search-context">
      Showing matching Events for <code>{{ submittedSearch }}</code>
      <span v-if="candidatesExamined !== null">
        · {{ candidatesExamined }} candidates examined
      </span>
    </div>

    <LoadingPanel v-if="result.isPending.value" label="Loading investigation data…" />
    <ApiErrorPanel
      v-else-if="result.error.value"
      :error="result.error.value"
      @retry="result.refetch()"
    />
    <EmptyState
      v-else-if="!result.data.value?.items.length"
      :title="submittedSearch ? 'No matching events' : 'No Issues in this view'"
      :description="
        submittedSearch
          ? 'Check the indexed field names and make the expression more specific.'
          : 'Events sent by your SDK will appear here after processing.'
      "
    >
      <SdkSetupButton v-if="!submittedSearch" />
    </EmptyState>

    <div v-else class="issue-table-wrap">
      <div class="issue-table-scroll">
        <table v-if="!submittedSearch" class="issue-table">
          <thead>
            <tr>
              <th scope="col">Issue</th>
              <th scope="col">Status</th>
              <th scope="col">Events</th>
              <th scope="col">Last seen</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="issue in issueItems" :key="issue.id">
              <td>
                <RouterLink :to="`/issues/${issue.id}`" class="issue-title">
                  {{ issue.title }}
                </RouterLink>
                <span>{{ issue.culprit || issue.grouping.summary }}</span>
              </td>
              <td><StatusBadge :status="issue.status" /></td>
              <td>
                {{ issue.occurrence_count.toLocaleString() }}
                <abbr v-if="issue.occurrence_count_approximate" title="Approximate count">~</abbr>
              </td>
              <td>{{ formatTime(issue.last_seen) }}</td>
            </tr>
          </tbody>
        </table>
        <div v-else class="event-results">
          <RouterLink
            v-for="event in eventItems"
            :key="event.event_id"
            :to="`/events/${event.event_id}`"
            class="event-row"
          >
            <span class="level-dot" :class="`level-dot--${event.level}`"></span>
            <strong>{{ event.level }}</strong>
            <span>{{ event.platform }}</span>
            <code>{{ event.event_id }}</code>
            <time :datetime="event.occurred_at">{{ formatTime(event.occurred_at) }}</time>
          </RouterLink>
        </div>
      </div>
      <nav class="pagination" aria-label="Results pages">
        <button
          class="button button--secondary"
          type="button"
          :disabled="history.length === 0"
          @click="previousPage"
        >
          Previous
        </button>
        <span>Page {{ history.length + 1 }}</span>
        <button
          class="button button--secondary"
          type="button"
          :disabled="!result.data.value.next_cursor"
          @click="nextPage"
        >
          Next
        </button>
      </nav>
    </div>
  </section>
</template>
