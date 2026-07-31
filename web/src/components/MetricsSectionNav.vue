<template>
  <nav class="section-tabs" :aria-label="$t('sectionNav.metricViews')">
    <RouterLink to="/metrics">{{ $t('sectionNav.overview') }}</RouterLink>
    <RouterLink to="/metrics/query">{{ $t('sectionNav.query') }}</RouterLink>
  </nav>
</template>
