<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';
import BaseSelect, { type SelectOption } from './BaseSelect.vue';
import { setLocale, supportedLocales, type AppLocale } from '../i18n';

defineOptions({ inheritAttrs: false });

const { locale, t } = useI18n();
const localeOptions = computed<SelectOption[]>(() =>
  supportedLocales.map((value) => ({
    value,
    label: t(`locale.${value}`),
  })),
);

async function changeLocale(value: string): Promise<void> {
  await setLocale(value as AppLocale);
}
</script>

<template>
  <BaseSelect
    v-bind="$attrs"
    class="locale-select"
    :model-value="locale"
    :options="localeOptions"
    :label="t('locale.label')"
    @update:model-value="changeLocale"
  />
</template>
