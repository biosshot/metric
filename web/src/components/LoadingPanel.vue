<script setup lang="ts">
defineProps<{ label: string }>();
</script>

<template>
  <div class="loading-panel" role="status">
    <span class="spinner" aria-hidden="true"></span>
    <span>{{ label }}</span>
  </div>
</template>
