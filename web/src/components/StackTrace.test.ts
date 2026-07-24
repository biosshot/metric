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
  it('renders pre, current, and post source context with line numbers', () => {
    const body = {
      exception: {
        values: [
          {
            stacktrace: {
              frames: [
                {
                  filename: 'src/example.ts',
                  function: 'explode',
                  lineno: 12,
                  pre_context: ['const attempt = prepare();', 'try {'],
                  context_line: '  attempt.run();',
                  post_context: ['} catch (error) {', '  report(error);'],
                  in_app: true,
                },
              ],
            },
          },
        ],
      },
    };

    render(StackTrace, { props: { body } });

    const sourceLines = Array.from(document.querySelectorAll('.source-context code')).map(
      (element) => element.textContent,
    );
    expect(sourceLines).toContain('const attempt = prepare();');
    expect(sourceLines).toContain('  attempt.run();');
    expect(sourceLines).toContain('  report(error);');
    expect(document.querySelector('.source-context .token--keyword')).toHaveTextContent('const');
    expect(document.querySelector('.source-context__current')).toHaveTextContent('12');
    expect(document.querySelectorAll('.source-context li')).toHaveLength(5);
  });

  it('bounds initial rendering and exposes the exact hidden count', async () => {
    render(StackTrace, { props: { body: eventWithFrames(120) } });

    expect(screen.getByRole('heading', { name: '120 frames' })).toBeVisible();
    expect(document.querySelectorAll('.stack-frame')).toHaveLength(40);
    const showAll = screen.getByRole('button', { name: 'Show all 120' });
    expect(showAll).toHaveAttribute('aria-expanded', 'false');

    await fireEvent.click(showAll);
    expect(document.querySelectorAll('.stack-frame')).toHaveLength(120);
  });

  it('renders the current thread stack when an exception has no stacktrace', () => {
    render(StackTrace, {
      props: {
        body: {
          exception: { values: [{ type: 'FaultkeepRustSdkCompatibilityError' }] },
          threads: {
            values: [
              {
                current: true,
                stacktrace: {
                  frames: [
                    {
                      filename: 'src/main.rs',
                      function: 'faultkeep_sdk_compatibility_rust::main',
                      lineno: 40,
                      in_app: true,
                    },
                  ],
                },
              },
            ],
          },
        },
      },
    });

    expect(screen.getByRole('heading', { name: '1 frames' })).toBeVisible();
    expect(screen.getByText('faultkeep_sdk_compatibility_rust::main')).toBeVisible();
    expect(screen.getByText('src/main.rs')).toBeVisible();
  });
});
