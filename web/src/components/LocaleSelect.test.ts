import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { describe, expect, it } from 'vitest';
import { setLocale } from '../i18n';
import LocaleSelect from './LocaleSelect.vue';

describe('LocaleSelect', () => {
  it('changes the active application language', async () => {
    await setLocale('en');
    render(LocaleSelect);

    await fireEvent.update(screen.getByLabelText('Language'), 'ru');

    await waitFor(() => expect(screen.getByLabelText('Язык')).toHaveValue('ru'));
    expect(document.documentElement.lang).toBe('ru');
  });
});
