<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import AppIcon, { type AppIconName } from './AppIcon.vue';

const props = defineProps<{ status: string }>();
const { t, te } = useI18n();
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
  if (['ignored', 'disabled', 'unavailable', 'spam'].includes(props.status)) return 'blocked';
  if (['open', 'error', 'fatal', 'unresolved', 'degraded', 'warning'].includes(props.status))
    return 'alert';
  return 'status';
});
const label = computed(() => {
  const key = `status.${props.status}`;
  return te(key) ? t(key) : props.status.replaceAll('_', ' ');
});
</script>

<template>
  <span class="status-badge" :class="`status-badge--${status}`">
    <AppIcon :name="icon" :size="13" />
    {{ label }}
  </span>
</template>
