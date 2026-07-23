import { fireEvent, render, screen } from '@testing-library/vue';
import { describe, expect, it } from 'vitest';
import StackTrace from './StackTrace.vue';

function eventWithFrames(count: number): Record<string, unknown> {
  return {
    exception: {
      values: [
        {
          stacktrace: {
            frames: Array.from({ length: count }, (_, index) => ({
              filename: `src/frame-${index}.ts`,
              function: `function${index}`,
              lineno: index + 1,
              in_app: true,
            })),
          },
        },
      ],
    },
  };
}

describe('StackTrace', () => {
  it('bounds initial rendering and exposes the exact hidden count', async () => {
    render(StackTrace, { props: { body: eventWithFrames(120) } });

    expect(screen.getByRole('heading', { name: '120 frames' })).toBeVisible();
    expect(document.querySelectorAll('.stack-frame')).toHaveLength(40);
    const showAll = screen.getByRole('button', { name: 'Show all 120' });
    expect(showAll).toHaveAttribute('aria-expanded', 'false');

    await fireEvent.click(showAll);
    expect(document.querySelectorAll('.stack-frame')).toHaveLength(120);
  });
});
