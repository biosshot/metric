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

  it('exposes a separated action without treating it as the selected value', async () => {
    const view = render(BaseSelect, {
      props: {
        modelValue: 'open',
        options: [
          ...options,
          { value: '__create__', label: 'New project', icon: 'plus', action: true },
        ],
        ariaLabel: 'Project',
      },
    });
    const trigger = screen.getByRole('combobox', { name: 'Project' });

    await fireEvent.click(trigger);
    const action = screen.getByRole('option', { name: 'New project' });
    expect(action).toHaveAttribute('aria-selected', 'false');
    expect(action).toHaveClass('base-select__option--action');
    await fireEvent.click(action);

    expect(view.emitted()['update:modelValue']).toEqual([['__create__']]);
    expect(trigger).toHaveTextContent('Open');
  });

  it('renders organization groups above their project options', async () => {
    render(BaseSelect, {
      props: {
        modelValue: 'payments',
        options: [
          { value: 'payments', label: 'Payments', group: 'Org: TestOrg' },
          { value: 'website', label: 'Website', group: 'Org: EchoSys' },
        ],
        ariaLabel: 'Workspace',
      },
    });

    await fireEvent.click(screen.getByRole('combobox', { name: 'Workspace' }));

    expect(screen.getByText('Org: TestOrg')).toBeVisible();
    expect(screen.getByText('Org: EchoSys')).toBeVisible();
    expect(screen.getByRole('option', { name: 'Payments' })).toBeVisible();
    expect(screen.getByRole('option', { name: 'Website' })).toBeVisible();
  });
});
