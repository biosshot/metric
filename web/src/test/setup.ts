import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/vue';
import { config } from '@vue/test-utils';
import { afterEach } from 'vitest';
import { i18n, initializeLocale, setLocale } from '../i18n';

await initializeLocale();

config.global.plugins = [i18n];

afterEach(async () => {
  cleanup();
  await setLocale('en');
});
