<script setup lang="ts">
import type { RouteLocationRaw } from 'vue-router';
import AppIcon, { type AppIconName } from './AppIcon.vue';

export interface RelatedSignalLink {
  key: string;
  icon: string;
  label: string;
  description?: string;
  to: RouteLocationRaw;
}

defineProps<{ links: RelatedSignalLink[] }>();

function iconName(value: string): AppIconName {
  return value as AppIconName;
}

function displayDescription(value: string | undefined): string | undefined {
  if (!value || value.length <= 20) return value;
  return `${value.slice(0, 12)}…${value.slice(-4)}`;
}
</script>

<template>
  <section v-if="links.length" class="panel detail-panel related-signals">
    <div class="section-heading">
      <div>
        <p class="eyebrow">{{ $t('relations.correlation') }}</p>
        <h2>{{ $t('relations.relatedData') }}</h2>
      </div>
    </div>
    <div class="compact-list">
      <RouterLink v-for="link in links" :key="link.key" :to="link.to">
        <AppIcon :name="iconName(link.icon)" :size="16" />
        <span>
          <strong>{{ link.label }}</strong>
          <small v-if="link.description" :title="link.description">
            {{ displayDescription(link.description) }}
          </small>
        </span>
        <AppIcon name="view" :size="16" />
      </RouterLink>
    </div>
  </section>
</template>
