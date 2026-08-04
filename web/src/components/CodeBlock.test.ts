import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { describe, expect, it, vi } from 'vitest';
import CodeBlock from './CodeBlock.vue';

describe('CodeBlock', () => {
  it('renders highlighted code and copies the exact source', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: { writeText },
    });
    const code = 'const dsn = "http://key@localhost/1";';

    render(CodeBlock, { props: { code, language: 'javascript', title: 'Browser' } });
    expect(document.querySelector('.token--keyword')).toHaveTextContent('const');

    await fireEvent.click(screen.getByRole('button', { name: 'Copy' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(code));
    expect(screen.getByRole('button', { name: 'Copied' })).toBeVisible();
  });

  it('falls back to document copy when Clipboard API is unavailable on HTTP', async () => {
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: undefined,
    });
    const execCommand = vi.fn().mockReturnValue(true);
    Object.defineProperty(document, 'execCommand', {
      configurable: true,
      value: execCommand,
    });

    render(CodeBlock, { props: { code: 'http://key@example/1', language: 'text' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Copy' }));

    await waitFor(() => expect(execCommand).toHaveBeenCalledWith('copy'));
    expect(screen.getByRole('button', { name: 'Copied' })).toBeVisible();
    expect(document.querySelector('textarea')).toBeNull();
  });
});
