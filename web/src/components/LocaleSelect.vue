<script setup lang="ts">
import { useI18n } from 'vue-i18n';
import { setLocale, supportedLocales, type AppLocale } from '../i18n';

defineOptions({ inheritAttrs: false });

const { locale, t } = useI18n();

async function changeLocale(event: Event): Promise<void> {
  if (!(event.target instanceof HTMLSelectElement)) return;
  await setLocale(event.target.value as AppLocale);
}
</script>

<template>
  <label v-bind="$attrs" class="locale-select">
    <span>{{ t('locale.label') }}</span>
    <select :value="locale" @change="changeLocale">
      <option v-for="option in supportedLocales" :key="option" :value="option">
        {{ t(`locale.${option}`) }}
      </option>
    </select>
  </label>
</template>
