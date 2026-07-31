<template>
  <nav class="section-tabs" :aria-label="$t('sectionNav.tracingViews')">
    <RouterLink to="/traces">{{ $t('sectionNav.traces') }}</RouterLink>
    <RouterLink to="/performance">{{ $t('sectionNav.performance') }}</RouterLink>
  </nav>
</template>
