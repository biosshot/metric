import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { describe, expect, it } from 'vitest';
import { setLocale } from '../i18n';
import LocaleSelect from './LocaleSelect.vue';

describe('LocaleSelect', () => {
  it('changes the active application language', async () => {
    await setLocale('en');
    render(LocaleSelect);

    await fireEvent.click(screen.getByRole('combobox', { name: 'Language' }));
    await fireEvent.click(screen.getByRole('option', { name: 'Russian' }));

    await waitFor(() =>
      expect(screen.getByRole('combobox', { name: 'Язык' })).toHaveTextContent('Русский'),
    );
    expect(document.documentElement.lang).toBe('ru');
  });
});
