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

function thread(fn: string, flags: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    ...flags,
    stacktrace: { frames: [{ filename: 'main.go', function: fn, in_app: true }] },
  };
}

describe('StackTrace', () => {
  it('renders derived source context for every caller and restores raw context on toggle', async () => {
    const raw = { function: 'a', filename: 'bundle.js', lineno: 1, context_line: 'generated();' };
    const body = {
      stacktrace: { frames: [raw, { instruction_addr: '0x10001000' }] },
      _metric: {
        symbolication: {
          status: 'complete',
          derived: [
            {
              origin: { kind: 'event' },
              frames: [
                {
                  original_index: 0,
                  function: 'caller',
                  filename: 'app.ts',
                  line: 8,
                  pre_context: ['prepare();'],
                  context_line: 'fail();',
                  post_context: ['finish();'],
                },
                {
                  original_index: 1,
                  function: 'fail',
                  filename: 'native.c',
                  line: 42,
                  pre_context: ['before();'],
                  context_line: '',
                  post_context: ['after();'],
                },
              ],
            },
          ],
        },
      },
    };
    const original = JSON.stringify(body);
    render(StackTrace, { props: { body } });
    expect(
      Array.from(document.querySelectorAll('.stack-frame strong')).map((node) => node.textContent),
    ).toEqual(['fail', 'caller']);
    expect(document.querySelectorAll('.source-context__current')).toHaveLength(2);
    expect(document.querySelector('.source-context__current')).toHaveTextContent('42');
    expect(document.querySelectorAll('.source-context li')).toHaveLength(6);
    expect(document.querySelector('.stack-list')).not.toHaveTextContent('generated();');
    await fireEvent.click(screen.getByRole('button', { name: 'Show original frames' }));
    expect(document.querySelector('.source-context code')).toHaveTextContent('generated();');
    expect(document.querySelector('.stack-list')).not.toHaveTextContent('before();');
    expect(JSON.stringify(body)).toBe(original);
  });

  it('displays derived native frames with their matching exception and keeps raw data intact', () => {
    const body = {
      exception: {
        values: [
          {
            stacktrace: {
              frames: [{ function: 'otherException', filename: 'other.py', lineno: 3 }],
            },
          },
          { stacktrace: { frames: [{ instruction_addr: '0x10001000', in_app: true }] } },
        ],
      },
      _metric: {
        symbolication: {
          status: 'complete',
          derived: [
            {
              origin: { kind: 'exception', index: 1 },
              frames: [
                {
                  original_index: 0,
                  function: 'metric_sdk_compatibility',
                  filename: 'src/main.rs',
                  line: 42,
                },
              ],
            },
          ],
        },
      },
    };
    const original = JSON.stringify(body);
    render(StackTrace, { props: { body } });
    expect(screen.getByText('metric_sdk_compatibility')).toBeVisible();
    expect(screen.getByText('otherException')).toBeVisible();
    expect(screen.getByText('src/main.rs', { exact: false })).toHaveTextContent(':42');
    expect(JSON.stringify(body)).toBe(original);
  });

  it('keeps unresolved frames in a partial event and does not show stale generated context', () => {
    const body = {
      stacktrace: {
        frames: [
          { function: 'a', filename: 'bundle.js', lineno: 1, context_line: 'minified();' },
          { function: 'unresolved', filename: 'vendor.js', lineno: 2 },
        ],
      },
      _metric: {
        symbolication: {
          status: 'partial',
          derived: [
            {
              origin: { kind: 'event' },
              frames: [
                { original_index: 0, function: 'explode', filename: 'app.ts', line: 12, column: 5 },
              ],
            },
          ],
        },
      },
    };
    render(StackTrace, { props: { body } });
    expect(screen.getByText('explode')).toBeVisible();
    expect(screen.getByText('unresolved')).toBeVisible();
    expect(screen.queryByText('minified();')).not.toBeInTheDocument();
  });

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
          exception: { values: [{ type: 'MetricRustSdkCompatibilityError' }] },
          threads: {
            values: [
              {
                current: true,
                stacktrace: {
                  frames: [
                    {
                      filename: 'src/main.rs',
                      function: 'metric_sdk_compatibility_rust::main',
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

    expect(screen.getByRole('heading', { name: '1 frame' })).toBeVisible();
    expect(screen.getByText('metric_sdk_compatibility_rust::main')).toBeVisible();
    expect(screen.getByText('src/main.rs')).toBeVisible();
  });

  it('prefers the crashed thread and accepts threads as a bare array', () => {
    render(StackTrace, {
      props: {
        body: {
          message: 'alert',
          threads: [
            thread('main.current', { current: true }),
            thread('main.crashed', { crashed: true }),
          ],
        },
      },
    });

    expect(screen.getByText('main.crashed')).toBeVisible();
    expect(screen.queryByText('main.current')).toBeNull();
  });

  it('takes the crashed and current flags only when they are true', () => {
    render(StackTrace, {
      props: {
        body: {
          message: 'alert',
          threads: [
            thread('main.other', { crashed: 'false' }),
            thread('main.current', { current: true }),
          ],
        },
      },
    });

    expect(screen.getByText('main.current')).toBeVisible();
    expect(screen.queryByText('main.other')).toBeNull();
  });

  it('skips thread frames that are not objects and source lines that are not text', () => {
    const frame = {
      function: 'main',
      lineno: 3,
      pre_context: [null, 'prepare();'],
      context_line: 'fail();',
    };
    render(StackTrace, {
      props: {
        body: {
          message: 'alert',
          threads: [{ current: true, stacktrace: { frames: [null, ['main.go'], frame] } }],
        },
      },
    });

    expect(screen.getByRole('heading', { name: '1 frame' })).toBeVisible();
    expect(screen.getByText('main')).toBeVisible();
    expect(document.querySelectorAll('.source-context li')).toHaveLength(3);
    expect(document.querySelector('.source-context__current')).toHaveTextContent('3');
  });

  it.each([
    ['several threads carry no flag', [thread('main.first'), thread('main.second')]],
    [
      'the crashed thread has no stack',
      [{ crashed: true }, thread('main.current', { current: true })],
    ],
  ])('shows no thread stack where the server takes none: %s', (_, threads) => {
    render(StackTrace, { props: { body: { message: 'alert', threads } } });

    expect(document.querySelectorAll('.stack-frame')).toHaveLength(0);
    expect(screen.getByText('This event does not contain stack frames.')).toBeVisible();
  });

  it('reads a thread stacktrace sent as a bare array, as the server does', () => {
    render(StackTrace, {
      props: {
        body: {
          message: 'alert',
          threads: [{ current: true, stacktrace: [{ function: 'main', filename: 'main.go' }] }],
        },
      },
    });

    expect(screen.getByText('main')).toBeVisible();
  });

  it('shows only thread frame fields of the expected type', () => {
    const frame = { function: 'main', filename: { toString: null }, lineno: '3', in_app: 'yes' };
    render(StackTrace, {
      props: {
        body: { message: 'alert', threads: [{ current: true, stacktrace: { frames: [frame] } }] },
      },
    });

    expect(screen.getByText('main')).toBeVisible();
    expect(screen.getByText('unknown source')).toBeVisible();
    expect(screen.queryByText('in app')).toBeNull();
  });

  it('ignores malformed thread values', () => {
    render(StackTrace, { props: { body: { message: 'alert', threads: { values: {} } } } });

    expect(document.querySelectorAll('.stack-frame')).toHaveLength(0);
  });
});
