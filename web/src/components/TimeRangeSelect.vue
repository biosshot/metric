<script setup lang="ts">
import BaseSelect, { type SelectOption } from './BaseSelect.vue';

defineProps<{ modelValue: string; label?: string }>();
defineEmits<{ 'update:modelValue': [value: string] }>();

const options: SelectOption[] = [
  { value: '1h', label: 'Last hour', icon: 'history' },
  { value: '24h', label: 'Last 24 hours', icon: 'history' },
  { value: '7d', label: 'Last 7 days', icon: 'history' },
  { value: '30d', label: 'Last 30 days', icon: 'history' },
];
</script>

<template>
  <BaseSelect
    class="time-range-select"
    :model-value="modelValue"
    :options="options"
    :label="label"
    @update:model-value="$emit('update:modelValue', $event)"
  />
</template>
