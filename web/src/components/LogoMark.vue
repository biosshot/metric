<script setup lang="ts">
withDefaults(
  defineProps<{
    size?: number;
    label?: string;
  }>(),
  {
    size: 18,
    label: undefined,
  },
);
</script>

<template>
  <svg
    class="app-icon"
    :width="size"
    :height="size"
    viewBox="-6 -6 108 108"
    fill="none"
    stroke="currentColor"
    stroke-width="10"
    stroke-linecap="round"
    :aria-hidden="label ? undefined : true"
    :aria-label="label"
    role="img"
  >
    <!-- Looped-square knot: four woven bands with alternating over/under gaps -->
    <path d="M 30 80 L 30 73" />
    <path d="M 30 59 L 30 16" />
    <path d="M 16 30 L 23 30" />
    <path d="M 37 30 L 80 30" />
    <path d="M 66 16 L 66 23" />
    <path d="M 66 37 L 66 80" />
    <path d="M 16 66 L 59 66" />
    <path d="M 73 66 L 80 66" />
    <path d="M 30 16 A 14 14 0 1 0 16 30" />
    <path d="M 80 30 A 14 14 0 1 0 66 16" />
    <path d="M 66 80 A 14 14 0 1 0 80 66" />
    <path d="M 16 66 A 14 14 0 1 0 30 80" />
  </svg>
</template>
