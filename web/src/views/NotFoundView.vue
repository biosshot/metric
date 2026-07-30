<script setup lang="ts">
import { computed } from 'vue';
import { useRoute } from 'vue-router';
import AppIcon from '../components/AppIcon.vue';
import EmptyState from '../components/EmptyState.vue';

const route = useRoute();
const requestedPath = computed(() => route.fullPath);
</script>

<template>
  <section class="not-found-view">
    <header class="page-header">
      <div>
        <p class="eyebrow">404 / route not found</p>
        <h1>Page not found</h1>
        <p>
          <code>{{ requestedPath }}</code>
          does not match a Metric page.
        </p>
      </div>
    </header>
    <EmptyState
      icon="blocked"
      title="This page is unavailable"
      description="The address may be incorrect, or the linked resource may have been removed."
    >
      <div class="empty-state__actions">
        <RouterLink class="button button--primary" to="/dashboard">
          <AppIcon name="dashboard" :size="16" />
          Dashboard
        </RouterLink>
        <RouterLink class="button button--secondary" to="/issues">
          <AppIcon name="clipboard" :size="16" />
          Issues
        </RouterLink>
      </div>
    </EmptyState>
  </section>
</template>
