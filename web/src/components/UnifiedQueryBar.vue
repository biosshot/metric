<script setup lang="ts">
import { computed, onBeforeUnmount, ref, useId, watch } from 'vue';
import { useI18n } from 'vue-i18n';
import { useRoute, useRouter } from 'vue-router';
import { api } from '../api/client';
import type { QuerySource } from '../api/types';
import { useSessionStore } from '../stores/session';
import AppIcon from './AppIcon.vue';

const props = withDefaults(
  defineProps<{
    modelValue: string;
    source: QuerySource;
    placeholder?: string;
    showSubmit?: boolean;
    showReset?: boolean;
    syncUrl?: boolean;
    disabled?: boolean;
  }>(),
  { placeholder: '', showSubmit: true, showReset: false, syncUrl: true, disabled: false },
);

const emit = defineEmits<{
  'update:modelValue': [value: string];
  submit: [];
  reset: [];
}>();

interface Suggestion {
  value: string;
  label: string;
  detail: string;
}

const route = useRoute();
const router = useRouter();
const session = useSessionStore();
const { t } = useI18n();
const inputId = useId();
const focused = ref(false);
const highlighted = ref(0);
const dynamicValues = ref<string[]>([]);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;
let valuesAbort: AbortController | null = null;
let requestGeneration = 0;

const fields: Record<QuerySource, Array<[string, string]>> = {
  issues: [
    ['status', 'status'],
    ['issue', 'issue_id'],
    ['title', 'title'],
    ['timestamp', 'timestamp'],
  ],
  errors: [
    ['level', 'level'],
    ['env', 'environment'],
    ['rel', 'release'],
    ['platform', 'platform'],
    ['issue', 'issue_id'],
    ['user', 'user.id'],
    ['timestamp', 'timestamp'],
  ],
  logs: [
    ['level', 'level'],
    ['msg', 'message'],
    ['svc', 'service'],
    ['env', 'environment'],
    ['rel', 'release'],
    ['trace', 'trace_id'],
    ['span', 'span_id'],
    ['timestamp', 'timestamp'],
  ],
  traces: [
    ['svc', 'service'],
    ['env', 'environment'],
    ['rel', 'release'],
    ['dur', 'duration_ms'],
    ['op', 'operation'],
    ['status', 'status'],
    ['trace', 'trace_id'],
    ['span', 'span_id'],
    ['timestamp', 'timestamp'],
  ],
  metrics: [
    ['metric', 'metric_name'],
    ['kind', 'metric_kind'],
    ['unit', 'unit'],
    ['trace', 'trace_id'],
    ['timestamp', 'timestamp'],
  ],
  replays: [
    ['replay', 'replay_id'],
    ['url', 'url'],
    ['env', 'environment'],
    ['rel', 'release'],
    ['timestamp', 'timestamp'],
  ],
  feedback: [
    ['feedback', 'feedback_id'],
    ['status', 'status'],
    ['msg', 'message'],
    ['replay', 'replay_id'],
    ['timestamp', 'timestamp'],
  ],
  releases: [
    ['rel', 'release'],
    ['timestamp', 'timestamp'],
  ],
};

const enumValues: Record<string, string[]> = {
  'issues:status': ['open', 'resolved', 'ignored'],
  'feedback:status': ['open', 'resolved', 'spam'],
  'errors:level': ['debug', 'info', 'warning', 'error', 'fatal'],
  'logs:level': ['trace', 'debug', 'info', 'warning', 'error', 'fatal'],
  'metrics:metric_kind': ['counter', 'gauge', 'distribution'],
};
const orderedFields = new Set([
  'timestamp',
  'received_at',
  'duration_ms',
  'metric_count',
  'metric_sum',
  'metric_min',
  'metric_max',
]);

const activeToken = computed(() => props.modelValue.match(/(?:^|\s)([^\s()]*)$/)?.[1] ?? '');
const activeField = computed(() => activeToken.value.split(':', 1)[0] ?? '');
const activeValue = computed(() => activeToken.value.split(':').slice(1).join(':'));
const activeCanonicalField = computed(
  () =>
    fields[props.source].find(
      ([alias, canonical]) => alias === activeField.value || canonical === activeField.value,
    )?.[1] ?? '',
);
const chips = computed(
  () =>
    props.modelValue
      .match(/(?:[^\s"]+|"[^"]*")+/g)
      ?.filter((value) => value.includes(':') || value === 'AND' || value === 'OR') ?? [],
);
const suggestions = computed<Suggestion[]>(() => {
  const token = activeToken.value.toLowerCase();
  if (!token.includes(':')) {
    return fields[props.source]
      .filter(([alias, canonical]) => alias.startsWith(token) || canonical.startsWith(token))
      .slice(0, 20)
      .map(([alias, canonical]) => ({ value: `${alias}:`, label: alias, detail: canonical }));
  }
  const key = `${props.source}:${activeCanonicalField.value}`;
  const operators: Suggestion[] =
    !activeValue.value && orderedFields.has(activeCanonicalField.value)
      ? ['>', '>=', '<', '<=', '='].map((operator) => ({
          value: `${activeField.value}:${operator}`,
          label: operator,
          detail: 'operator',
        }))
      : [];
  const values = [...(enumValues[key] ?? []), ...dynamicValues.value]
    .filter((value, index, values) => values.indexOf(value) === index)
    .filter((value) => value.toLowerCase().startsWith(activeValue.value.toLowerCase()))
    .slice(0, 20)
    .map((value) => ({
      value: `${activeField.value}:${value}`,
      label: value,
      detail: activeField.value,
    }));
  return [...operators, ...values].slice(0, 20);
});
const open = computed(() => focused.value && suggestions.value.length > 0);

watch(activeToken, () => {
  highlighted.value = 0;
  dynamicValues.value = [];
  if (debounceTimer) clearTimeout(debounceTimer);
  valuesAbort?.abort();
  valuesAbort = null;
  const canonical = activeCanonicalField.value;
  if (!canonical || !['environment', 'release', 'metric_name'].includes(canonical)) return;
  const generation = ++requestGeneration;
  debounceTimer = setTimeout(async () => {
    const projectId = session.selectedProjectId;
    if (!projectId) return;
    valuesAbort = new AbortController();
    try {
      const result = await api.query<string>(
        projectId,
        {
          source: props.source,
          query: '',
          result: { kind: 'values', field: canonical },
          limit: 20,
        },
        valuesAbort.signal,
      );
      if (generation === requestGeneration) dynamicValues.value = result.items;
    } catch {
      if (generation === requestGeneration) dynamicValues.value = [];
    }
  }, 180);
});

watch(
  () => route.query.q,
  (value) => {
    if (!props.syncUrl) return;
    const query = typeof value === 'string' ? value : '';
    if (query !== props.modelValue) {
      emit('update:modelValue', query);
      queueMicrotask(() => emit('submit'));
    }
  },
);

onBeforeUnmount(() => {
  requestGeneration += 1;
  if (debounceTimer) clearTimeout(debounceTimer);
  valuesAbort?.abort();
});

function replaceActiveToken(value: string): void {
  const start = props.modelValue.length - activeToken.value.length;
  emit('update:modelValue', `${props.modelValue.slice(0, start)}${value}`);
}

function selectSuggestion(suggestion: Suggestion): void {
  replaceActiveToken(suggestion.value);
  highlighted.value = 0;
}

function keydown(event: KeyboardEvent): void {
  if (event.key === 'Enter' && !open.value && props.showSubmit) {
    event.preventDefault();
    void submit();
    return;
  }
  if (!open.value) return;
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    highlighted.value = (highlighted.value + 1) % suggestions.value.length;
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    highlighted.value =
      (highlighted.value - 1 + suggestions.value.length) % suggestions.value.length;
  } else if (event.key === 'Enter' && suggestions.value[highlighted.value]) {
    event.preventDefault();
    selectSuggestion(suggestions.value[highlighted.value]);
  } else if (event.key === 'Escape') focused.value = false;
}

function blur(): void {
  setTimeout(() => (focused.value = false), 120);
}

async function submit(): Promise<void> {
  if (props.syncUrl) {
    const query = props.modelValue.trim();
    const next = { ...route.query };
    if (query) next.q = query;
    else delete next.q;
    await router.replace({ query: next });
  }
  emit('submit');
}

async function reset(): Promise<void> {
  emit('update:modelValue', '');
  if (props.syncUrl) {
    const next = { ...route.query };
    delete next.q;
    await router.replace({ query: next });
  }
  emit('reset');
}
</script>

<template>
  <div class="unified-query-bar" role="search">
    <div class="unified-query-bar__input-wrap">
      <AppIcon name="search" :size="17" />
      <label class="sr-only" :for="inputId">Query</label>
      <input
        :id="inputId"
        :value="modelValue"
        type="search"
        :placeholder="placeholder"
        maxlength="32768"
        autocomplete="off"
        spellcheck="false"
        :disabled="disabled"
        @input="emit('update:modelValue', ($event.target as HTMLInputElement).value)"
        @focus="focused = true"
        @blur="blur"
        @keydown="keydown"
      />
      <ul v-if="open" class="unified-query-bar__suggestions" role="listbox">
        <li
          v-for="(suggestion, index) in suggestions"
          :key="`${suggestion.value}:${index}`"
          :class="{ 'is-active': index === highlighted }"
          role="option"
          :aria-selected="index === highlighted"
          @mousedown.prevent="selectSuggestion(suggestion)"
        >
          <code>{{ suggestion.label }}</code>
          <span>{{ suggestion.detail }}</span>
        </li>
      </ul>
    </div>
    <slot name="actions" />
    <button
      v-if="showSubmit"
      class="button button--primary"
      type="button"
      :disabled="disabled"
      @click="submit"
    >
      <AppIcon name="search" :size="16" />
      {{ t('common.search') }}
    </button>
    <button
      v-if="showReset"
      class="button button--secondary"
      type="button"
      :disabled="disabled"
      @click="reset"
    >
      <AppIcon name="close" :size="16" />
      {{ t('common.reset') }}
    </button>
  </div>
  <div v-if="chips.length" class="unified-query-bar__chips" aria-label="Query conditions">
    <code v-for="chip in chips" :key="chip">{{ chip }}</code>
  </div>
</template>
