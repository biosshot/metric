<script setup lang="ts">
import AppIcon from './AppIcon.vue';
</script>

<template>
  <RouterLink class="button button--primary" to="/project/setup">
    <AppIcon name="connect" :size="16" />
    {{ $t('sectionNav.viewSdkSetup') }}
  </RouterLink>
</template>
