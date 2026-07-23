<script setup lang="ts">
import { useQuery } from '@tanstack/vue-query';
import { api } from '../api/client';
import ApiErrorPanel from '../components/ApiErrorPanel.vue';
import LoadingPanel from '../components/LoadingPanel.vue';
import StatusBadge from '../components/StatusBadge.vue';

const capabilities = useQuery({
  queryKey: ['capabilities'],
  queryFn: api.capabilities,
});
const status = useQuery({
  queryKey: ['component-status'],
  queryFn: api.status,
  refetchInterval: 30_000,
});
</script>

<template>
  <section>
    <header class="page-header">
      <div>
        <p class="eyebrow">Operations</p>
        <h1>System status</h1>
        <p>Authenticated component health and the exact capabilities of this build.</p>
      </div>
      <StatusBadge v-if="status.data.value" :status="status.data.value.status" />
    </header>
    <LoadingPanel v-if="status.isPending.value" label="Checking components…" />
    <ApiErrorPanel
      v-else-if="status.error.value"
      :error="status.error.value"
      @retry="status.refetch()"
    />
    <section v-else class="panel">
      <div class="component-grid">
        <article v-for="(value, name) in status.data.value?.components" :key="name">
          <div>
            <strong>{{ name }}</strong>
            <small>Required runtime component</small>
          </div>
          <StatusBadge :status="value" />
        </article>
      </div>
    </section>
    <LoadingPanel v-if="capabilities.isPending.value" label="Loading capabilities…" />
    <ApiErrorPanel
      v-else-if="capabilities.error.value"
      :error="capabilities.error.value"
      @retry="capabilities.refetch()"
    />
    <section v-else class="panel">
      <div class="section-heading">
        <div>
          <p class="eyebrow">API {{ capabilities.data.value?.api_version }}</p>
          <h2>Build capabilities</h2>
        </div>
      </div>
      <div class="capability-list">
        <article v-for="(enabled, name) in capabilities.data.value?.features" :key="name">
          <span>{{ name.replaceAll('_', ' ') }}</span>
          <StatusBadge :status="enabled ? 'enabled' : 'disabled'" />
        </article>
      </div>
      <div class="search-capability">
        <strong>Search v1 indexed fields</strong>
        <code v-for="field in capabilities.data.value?.search.fields" :key="field">{{
          field
        }}</code>
        <p>
          Full text and arbitrary custom tags are not enabled. Unsupported expressions return an
          explicit error.
        </p>
      </div>
    </section>
  </section>
</template>
