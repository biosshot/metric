import { i18n, setLocale, supportedLocales } from './index';
import { describe, expect, it, vi } from 'vitest';

describe('web localization', () => {
  it('registers the supported locales and applies a persisted selection', () => {
    const setItem = vi.fn();
    Object.defineProperty(window, 'localStorage', {
      configurable: true,
      value: { getItem: vi.fn(), setItem },
    });
    expect(supportedLocales).toEqual(['en', 'ru']);

    setLocale('ru');

    expect(i18n.global.t('locale.label')).toBe('Язык');
    expect(document.documentElement.lang).toBe('ru');
    expect(setItem).toHaveBeenCalledWith('metric.locale', 'ru');
  });
});
