import { createI18n } from 'vue-i18n';
import en from './locales/en';
import ru from './locales/ru';

const localeStorageKey = 'metric.locale';

export const localeMessages = { en, ru };
export type AppLocale = keyof typeof localeMessages;
export const supportedLocales = Object.keys(localeMessages) as AppLocale[];

function isAppLocale(value: string | null | undefined): value is AppLocale {
  return supportedLocales.includes(value as AppLocale);
}

function browserLocale(): AppLocale {
  const candidates = typeof navigator === 'undefined' ? [] : navigator.languages;
  for (const candidate of candidates) {
    const language = candidate.toLowerCase().split('-')[0];
    if (isAppLocale(language)) return language;
  }
  return 'en';
}

function initialLocale(): AppLocale {
  try {
    const saved = window.localStorage.getItem(localeStorageKey);
    if (isAppLocale(saved)) return saved;
  } catch {
    // Storage may be unavailable in privacy-restricted browser contexts.
  }
  return browserLocale();
}

export const i18n = createI18n({
  legacy: false,
  locale: initialLocale(),
  fallbackLocale: 'en',
  messages: localeMessages,
});

export function setLocale(locale: AppLocale): void {
  i18n.global.locale.value = locale;
  document.documentElement.lang = locale;
  try {
    window.localStorage.setItem(localeStorageKey, locale);
  } catch {
    // The selected locale still applies to the current page without persistence.
  }
}

document.documentElement.lang = i18n.global.locale.value;
