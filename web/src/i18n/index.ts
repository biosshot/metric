import { createI18n } from 'vue-i18n';

const localeStorageKey = 'metric.locale';

const localeLoaders = {
  en: () => import('./locales/en'),
  ru: () => import('./locales/ru'),
} as const;

export type AppLocale = keyof typeof localeLoaders;
export const supportedLocales = Object.keys(localeLoaders) as AppLocale[];

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
  locale: 'en',
  fallbackLocale: 'en',
  messages: {},
});

function applyDocumentLocale(locale: AppLocale): void {
  document.documentElement.lang = locale;
  const description = document.querySelector<HTMLMetaElement>('meta[name="description"]');
  if (description) description.content = i18n.global.t('meta.description');
}

async function loadLocale(locale: AppLocale): Promise<void> {
  if (i18n.global.availableLocales.includes(locale)) return;
  const { default: messages } = await localeLoaders[locale]();
  i18n.global.setLocaleMessage(locale, messages);
}

export async function initializeLocale(): Promise<void> {
  const locale = initialLocale();
  await loadLocale('en');
  if (locale !== 'en') await loadLocale(locale);
  i18n.global.locale.value = locale;
  applyDocumentLocale(locale);
}

export async function setLocale(locale: AppLocale): Promise<void> {
  await loadLocale(locale);
  i18n.global.locale.value = locale;
  applyDocumentLocale(locale);
  try {
    window.localStorage.setItem(localeStorageKey, locale);
  } catch {
    // The selected locale still applies to the current page without persistence.
  }
}
