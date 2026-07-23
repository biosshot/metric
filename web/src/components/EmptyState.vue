<script setup lang="ts">
defineProps<{ title: string; description: string }>();
</script>

<template>
  <section class="empty-state">
    <div class="empty-state__icon" aria-hidden="true">✓</div>
    <h2>{{ title }}</h2>
    <p>{{ description }}</p>
    <slot />
  </section>
</template>
