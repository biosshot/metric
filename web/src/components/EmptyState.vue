<script setup lang="ts">
import AppIcon, { type AppIconName } from './AppIcon.vue';

withDefaults(defineProps<{ title: string; description: string; icon?: AppIconName }>(), {
  icon: 'clipboard',
});
</script>

<template>
  <section class="empty-state">
    <div class="empty-state__icon" aria-hidden="true">
      <AppIcon :name="icon" :size="24" />
    </div>
    <h2>{{ title }}</h2>
    <p>{{ description }}</p>
    <slot />
  </section>
</template>
