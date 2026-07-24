import { fireEvent, render, screen } from '@testing-library/vue';
import { describe, expect, it } from 'vitest';
import BaseSelect from './BaseSelect.vue';

const options = [
  { value: 'open', label: 'Open' },
  { value: 'resolved', label: 'Resolved' },
  { value: 'ignored', label: 'Ignored' },
];

describe('BaseSelect', () => {
  it('selects options with the keyboard and exposes combobox state', async () => {
    const view = render(BaseSelect, {
      props: { modelValue: 'open', options, ariaLabel: 'Issue status' },
    });
    const trigger = screen.getByRole('combobox', { name: 'Issue status' });

    expect(trigger).toHaveAttribute('aria-expanded', 'false');
    await fireEvent.keyDown(trigger, { key: 'ArrowDown' });
    expect(trigger).toHaveAttribute('aria-expanded', 'true');
    await fireEvent.keyDown(trigger, { key: 'Enter' });

    expect(view.emitted()['update:modelValue']).toEqual([['resolved']]);
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
  });
});
