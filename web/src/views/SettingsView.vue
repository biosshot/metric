<script setup lang="ts">
import AppIcon from '../components/AppIcon.vue';
import { useSessionStore } from '../stores/session';

const session = useSessionStore();
</script>

<template>
  <section class="settings-shell">
    <header class="settings-shell__header">
      <p class="eyebrow">{{ $t('settingsShell.eyebrow') }}</p>
      <h1>{{ $t('settingsShell.title') }}</h1>
      <p>{{ $t('settingsShell.description') }}</p>
    </header>

    <div class="settings-shell__layout">
      <nav class="settings-navigation" :aria-label="$t('settingsShell.sections')">
        <div>
          <span>{{ $t('settingsShell.project') }}</span>
          <RouterLink to="/settings/project">
            <AppIcon name="settings" :size="17" />
            {{ $t('settingsShell.dataAccess') }}
          </RouterLink>
          <RouterLink to="/dashboard?edit=1">
            <AppIcon name="dashboard" :size="17" />
            {{ $t('settingsShell.dashboards') }}
          </RouterLink>
        </div>
        <div>
          <span>{{ $t('settingsShell.automation') }}</span>
          <RouterLink v-if="session.has('project:admin')" to="/settings/notifications">
            <AppIcon name="alerts" :size="17" />
            {{ $t('settingsShell.notifications') }}
          </RouterLink>
        </div>
        <div>
          <span>{{ $t('settingsShell.workspace') }}</span>
          <RouterLink to="/settings/organization">
            <AppIcon name="organization" :size="17" />
            {{ $t('settingsShell.organization') }}
          </RouterLink>
          <RouterLink to="/settings/system">
            <AppIcon name="activity" :size="17" />
            {{ $t('settingsShell.system') }}
          </RouterLink>
        </div>
      </nav>

      <div class="settings-shell__content">
        <RouterView />
      </div>
    </div>
  </section>
</template>
