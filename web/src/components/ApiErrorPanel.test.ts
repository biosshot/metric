import { fireEvent, render, screen } from '@testing-library/vue';
import { describe, expect, it } from 'vitest';
import { ApiError } from '../api/client';
import ApiErrorPanel from './ApiErrorPanel.vue';

describe('ApiErrorPanel', () => {
  it('renders actionable diagnostics and retry only for retryable failures', async () => {
    const rendered = render(ApiErrorPanel, {
      props: {
        error: new ApiError(
          503,
          'temporarily_unavailable',
          'req-17',
          'MongoDB is unavailable.',
          true,
        ),
      },
    });

    expect(screen.getByRole('alert')).toHaveTextContent('MongoDB is unavailable.');
    expect(screen.getByRole('alert')).toHaveTextContent('req-17');
    await fireEvent.click(screen.getByRole('button', { name: 'Try again' }));
    expect(rendered.emitted().retry).toHaveLength(1);
  });
});
