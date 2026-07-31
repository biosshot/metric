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
        <p class="eyebrow">{{ $t('notFound.eyebrow') }}</p>
        <h1>{{ $t('notFound.title') }}</h1>
        <p>
          <code>{{ requestedPath }}</code>
          {{ $t('notFound.pathDescription') }}
        </p>
      </div>
    </header>
    <EmptyState
      icon="blocked"
      :title="$t('notFound.unavailable')"
      :description="$t('notFound.description')"
    >
      <div class="empty-state__actions">
        <RouterLink class="button button--primary" to="/dashboard">
          <AppIcon name="dashboard" :size="16" />
          {{ $t('app.dashboard') }}
        </RouterLink>
        <RouterLink class="button button--secondary" to="/issues">
          <AppIcon name="clipboard" :size="16" />
          {{ $t('app.issues') }}
        </RouterLink>
      </div>
    </EmptyState>
  </section>
</template>
