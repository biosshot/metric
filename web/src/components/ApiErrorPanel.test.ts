import { fireEvent, render, screen } from '@testing-library/vue';
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../api/client';
import ApiErrorPanel from './ApiErrorPanel.vue';

describe('ApiErrorPanel', () => {
  it('renders actionable diagnostics and retry only for retryable failures', async () => {
    const retry = vi.fn();
    const rendered = render(ApiErrorPanel, {
      props: {
        error: new ApiError(
          503,
          'temporarily_unavailable',
          'req-17',
          'MongoDB is unavailable.',
          true,
        ),
        onRetry: retry,
      },
    });

    expect(screen.getByRole('alert')).toHaveTextContent('MongoDB is unavailable.');
    expect(screen.getByRole('alert')).toHaveTextContent('req-17');
    await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(retry).toHaveBeenCalledOnce();
    expect(rendered.emitted().retry).toBeUndefined();
  });

  it('does not render an inert retry button without a handler', () => {
    render(ApiErrorPanel, {
      props: {
        error: new ApiError(503, 'temporarily_unavailable', null, 'Unavailable.', true),
      },
    });

    expect(screen.queryByRole('button', { name: 'Try again' })).not.toBeInTheDocument();
  });
});
