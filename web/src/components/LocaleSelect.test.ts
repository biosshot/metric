import { fireEvent, render, screen } from '@testing-library/vue';
import { describe, expect, it } from 'vitest';
import { setLocale } from '../i18n';
import LocaleSelect from './LocaleSelect.vue';

describe('LocaleSelect', () => {
  it('changes the active application language', async () => {
    setLocale('en');
    render(LocaleSelect);

    await fireEvent.update(screen.getByLabelText('Language'), 'ru');

    expect(screen.getByLabelText('Язык')).toHaveValue('ru');
    expect(document.documentElement.lang).toBe('ru');
  });
});
