import { mount } from '@vue/test-utils';
import { describe, expect, it } from 'vitest';
import TimeRangeSelect from './TimeRangeSelect.vue';

describe('TimeRangeSelect', () => {
  it('applies a custom period from a popover without expanding the control', async () => {
    const wrapper = mount(TimeRangeSelect, {
      props: {
        modelValue: '24h',
        windowValue: {
          from: new Date('2026-07-20T00:00:00.000Z').getTime(),
          until: new Date('2026-07-21T00:00:00.000Z').getTime(),
        },
        ariaLabel: 'Test time range',
      },
      attachTo: document.body,
    });

    await wrapper.get('[role="combobox"]').trigger('click');
    await wrapper
      .findAll('[role="option"]')
      .find((option) => option.text().includes('Custom range'))!
      .trigger('click');
    await wrapper.setProps({ modelValue: 'custom' });

    expect(wrapper.get('[role="dialog"]').isVisible()).toBe(true);
    const inputs = wrapper.findAll('input[type="datetime-local"]');
    await inputs[0].setValue('2026-07-20T10:00');
    await inputs[1].setValue('2026-07-21T10:00');
    await wrapper.get('button.button--secondary').trigger('click');

    const applied = wrapper.emitted('update:windowValue')?.at(-1)?.[0] as {
      from: number;
      until: number;
    };
    expect(applied).toEqual({
      from: new Date('2026-07-20T10:00').getTime(),
      until: new Date('2026-07-21T10:00').getTime(),
    });
    expect(wrapper.find('[role="dialog"]').exists()).toBe(false);

    await wrapper.setProps({ windowValue: applied });
    expect(wrapper.get('[role="combobox"]').text()).not.toContain('Custom range');
    wrapper.unmount();
  });
});
