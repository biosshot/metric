import { i18n, setLocale, supportedLocales } from './index';
import { describe, expect, it, vi } from 'vitest';
import en from './locales/en';
import ru from './locales/ru';

function messageKeys(messages: Record<string, unknown>, prefix = ''): string[] {
  return Object.entries(messages).flatMap(([key, value]) => {
    const path = prefix ? `${prefix}.${key}` : key;
    return value && typeof value === 'object'
      ? messageKeys(value as Record<string, unknown>, path)
      : [path];
  });
}

describe('web localization', () => {
  it('loads a selected locale and persists it', async () => {
    const setItem = vi.fn();
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: { getItem: vi.fn(), setItem },
    });
    expect(supportedLocales).toEqual(['en', 'ru']);

    await setLocale('ru');

    expect(i18n.global.t('locale.label')).toBe('Язык');
    expect(i18n.global.availableLocales).toContain('ru');
    expect(document.documentElement.lang).toBe('ru');
    expect(setItem).toHaveBeenCalledWith('metric.locale', 'ru');
  });

  it('keeps English and Russian message catalogs in sync', () => {
    expect(messageKeys(ru).sort()).toEqual(messageKeys(en).sort());
  });

  it('compiles every message in both locales', async () => {
    for (const locale of supportedLocales) {
      await setLocale(locale);
      for (const key of messageKeys(locale === 'en' ? en : ru)) {
        expect(() => i18n.global.t(key, { count: 2 })).not.toThrow();
      }
    }
  });
});
