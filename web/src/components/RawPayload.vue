<script setup lang="ts">
import CodeBlock from './CodeBlock.vue';

defineProps<{ code: string }>();
</script>

<template>
  <details class="raw-event">
    <summary>{{ $t('common.rawPayload') }}</summary>
    <p>{{ $t('common.rawPayloadDescription') }}</p>
    <CodeBlock language="json" :code="code" />
  </details>
</template>
