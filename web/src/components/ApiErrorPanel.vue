<script setup lang="ts">
import { computed } from 'vue';
import { ApiError } from '../api/client';
import AppIcon from './AppIcon.vue';

const props = defineProps<{
  error: unknown;
  title?: string;
  retryLabel?: string;
}>();

defineEmits<{ retry: [] }>();

const apiError = computed(() =>
  props.error instanceof ApiError
    ? props.error
    : new ApiError(
        0,
        'unexpected_error',
        null,
        'An unexpected client error interrupted this view.',
      ),
);
</script>

<template>
  <section class="error-panel" role="alert" aria-live="assertive">
    <div class="error-panel__mark" aria-hidden="true">
      <AppIcon name="failure" :size="22" />
    </div>
    <div>
      <h2>{{ title ?? 'Unable to load this view' }}</h2>
      <p>{{ apiError.message }}</p>
      <dl class="error-details">
        <div>
          <dt>Code</dt>
          <dd>{{ apiError.code }}</dd>
        </div>
        <div v-if="apiError.status">
          <dt>HTTP</dt>
          <dd>{{ apiError.status }}</dd>
        </div>
        <div v-if="apiError.requestId">
          <dt>Request ID</dt>
          <dd>
            <code>{{ apiError.requestId }}</code>
          </dd>
        </div>
      </dl>
      <button
        v-if="apiError.retryable"
        class="button button--secondary"
        type="button"
        @click="$emit('retry')"
      >
        <AppIcon name="refresh" :size="16" />
        {{ retryLabel ?? 'Try again' }}
      </button>
    </div>
  </section>
</template>
