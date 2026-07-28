<script setup lang="ts">
import AppIcon from '../components/AppIcon.vue';
</script>

<template>
  <section class="settings-shell">
    <header class="settings-shell__header">
      <p class="eyebrow">Configure</p>
      <h1>Settings</h1>
      <p>Project controls, notifications, organization access, and system state in one place.</p>
    </header>

    <div class="settings-shell__layout">
      <nav class="settings-navigation" aria-label="Settings sections">
        <div>
          <span>Project</span>
          <RouterLink to="/settings/project">
            <AppIcon name="settings" :size="17" />
            Data & access
          </RouterLink>
          <RouterLink to="/dashboard?edit=1">
            <AppIcon name="dashboard" :size="17" />
            Dashboards
          </RouterLink>
        </div>
        <div>
          <span>Automation</span>
          <RouterLink to="/settings/notifications">
            <AppIcon name="alerts" :size="17" />
            Notifications
          </RouterLink>
        </div>
        <div>
          <span>Workspace</span>
          <RouterLink to="/settings/organization">
            <AppIcon name="organization" :size="17" />
            Organization
          </RouterLink>
          <RouterLink to="/settings/system">
            <AppIcon name="activity" :size="17" />
            System
          </RouterLink>
        </div>
      </nav>

      <div class="settings-shell__content">
        <RouterView />
      </div>
    </div>
  </section>
</template>
