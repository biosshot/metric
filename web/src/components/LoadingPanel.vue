<script setup lang="ts">
import AppIcon from './AppIcon.vue';

defineProps<{ label: string }>();
</script>

<template>
  <div class="loading-panel" role="status">
    <AppIcon class="spinner" name="loading" :size="18" />
    <span>{{ label }}</span>
  </div>
</template>
