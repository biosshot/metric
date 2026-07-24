<script setup lang="ts">
import { computed } from 'vue';
import AppIcon, { type AppIconName } from './AppIcon.vue';

const props = defineProps<{ status: string }>();
const icon = computed<AppIconName>(() => {
  if (
    [
      'resolved',
      'healthy',
      'active',
      'success',
      'ready',
      'running',
      'available',
      'enabled',
    ].includes(props.status)
  )
    return 'success';
  if (['ignored', 'disabled', 'unavailable'].includes(props.status)) return 'blocked';
  if (['open', 'error', 'fatal', 'unresolved', 'degraded', 'warning'].includes(props.status))
    return 'alert';
  return 'status';
});
</script>

<template>
  <span class="status-badge" :class="`status-badge--${status}`">
    <AppIcon :name="icon" :size="13" />
    {{ status.replaceAll('_', ' ') }}
  </span>
</template>
