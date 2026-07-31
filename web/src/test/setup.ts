import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/vue';
import { config } from '@vue/test-utils';
import { afterEach } from 'vitest';
import { i18n, setLocale } from '../i18n';

config.global.plugins = [i18n];

afterEach(() => {
  cleanup();
  setLocale('en');
});
