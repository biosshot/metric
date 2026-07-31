<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import { ApiError } from '../api/client';
import AppIcon from './AppIcon.vue';

const props = defineProps<{
  error: unknown;
  title?: string;
  retryLabel?: string;
  onRetry?: () => void;
}>();
const { t } = useI18n();

const apiError = computed(() =>
  props.error instanceof ApiError
    ? props.error
    : new ApiError(0, 'unexpected_error', null, t('errors.unexpectedClient')),
);
</script>

<template>
  <section class="error-panel" role="alert" aria-live="assertive">
    <div class="error-panel__mark" aria-hidden="true">
      <AppIcon name="failure" :size="22" />
    </div>
    <div>
      <h2>{{ title ?? $t('errors.unableToLoad') }}</h2>
      <p>{{ apiError.message }}</p>
      <dl class="error-details">
        <div>
          <dt>{{ $t('common.code') }}</dt>
          <dd>{{ apiError.code }}</dd>
        </div>
        <div v-if="apiError.status">
          <dt>{{ $t('common.http') }}</dt>
          <dd>{{ apiError.status }}</dd>
        </div>
        <div v-if="apiError.requestId">
          <dt>{{ $t('common.requestId') }}</dt>
          <dd>
            <code>{{ apiError.requestId }}</code>
          </dd>
        </div>
      </dl>
      <button
        v-if="apiError.retryable && onRetry"
        class="button button--secondary"
        type="button"
        @click="onRetry"
      >
        <AppIcon name="refresh" :size="16" />
        {{ retryLabel ?? $t('common.tryAgain') }}
      </button>
    </div>
  </section>
</template>
