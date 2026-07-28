<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, useAttrs, watch } from 'vue';
import BaseSelect, { type SelectOption } from './BaseSelect.vue';
import {
  localDateTime,
  parseCustomTimeWindow,
  timeWindow,
  type TimeWindow,
} from '../lib/timeRange';

const props = defineProps<{
  modelValue: string;
  windowValue: TimeWindow;
  label?: string;
}>();
defineOptions({ inheritAttrs: false });
const attrs = useAttrs();
const emit = defineEmits<{
  'update:modelValue': [value: string];
  'update:windowValue': [value: TimeWindow];
}>();

const options: SelectOption[] = [
  { value: '1h', label: 'Last hour', icon: 'history' },
  { value: '24h', label: 'Last 24 hours', icon: 'history' },
  { value: '7d', label: 'Last 7 days', icon: 'history' },
  { value: '30d', label: 'Last 30 days', icon: 'history' },
  { value: 'custom', label: 'Custom range', icon: 'settings' },
];
const customFrom = ref(localDateTime(props.windowValue.from));
const customUntil = ref(localDateTime(props.windowValue.until));
const customError = ref('');
const customOpen = ref(false);
const root = ref<HTMLElement>();
const fromInput = ref<HTMLInputElement>();
const selectedLabel = computed(() => {
  if (props.modelValue !== 'custom') return undefined;
  const format = new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });
  return `${format.format(props.windowValue.from)} – ${format.format(props.windowValue.until)}`;
});

watch(
  () => props.windowValue,
  (value) => {
    if (props.modelValue !== 'custom') return;
    customFrom.value = localDateTime(value.from);
    customUntil.value = localDateTime(value.until);
  },
);

function selectRange(value: string): void {
  emit('update:modelValue', value);
  customError.value = '';
  if (value === 'custom') {
    customFrom.value = localDateTime(props.windowValue.from);
    customUntil.value = localDateTime(props.windowValue.until);
    customOpen.value = true;
    void nextTick(() => fromInput.value?.focus());
    return;
  }
  customOpen.value = false;
  emit('update:windowValue', timeWindow(value));
}

function applyCustom(): void {
  const value = parseCustomTimeWindow(customFrom.value, customUntil.value);
  if (!value) {
    customError.value = 'Choose a valid period up to 30 days.';
    return;
  }
  customError.value = '';
  emit('update:windowValue', value);
  customOpen.value = false;
}

function closeCustomRange(): void {
  customOpen.value = false;
  customError.value = '';
}

function onDocumentPointerDown(event: PointerEvent): void {
  if (!root.value?.contains(event.target as Node)) closeCustomRange();
}

onMounted(() => document.addEventListener('pointerdown', onDocumentPointerDown));
onBeforeUnmount(() => document.removeEventListener('pointerdown', onDocumentPointerDown));
</script>

<template>
  <div ref="root" class="time-range-control" @keydown.esc="closeCustomRange">
    <BaseSelect
      v-bind="attrs"
      class="time-range-select"
      :model-value="modelValue"
      :options="options"
      :label="label"
      :selected-label="selectedLabel"
      @update:model-value="selectRange"
    />
    <div
      v-if="modelValue === 'custom' && customOpen"
      class="time-range-custom"
      role="dialog"
      aria-label="Custom time range"
    >
      <label>
        <span>From</span>
        <input ref="fromInput" v-model="customFrom" type="datetime-local" required />
      </label>
      <label>
        <span>Until</span>
        <input v-model="customUntil" type="datetime-local" required />
      </label>
      <button class="button button--secondary button--fit" type="button" @click="applyCustom">
        Apply range
      </button>
      <small v-if="customError" class="field-error" role="alert">{{ customError }}</small>
    </div>
  </div>
</template>
