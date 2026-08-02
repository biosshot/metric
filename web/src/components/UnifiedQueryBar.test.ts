import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { createMemoryHistory, createRouter } from 'vue-router';
import { describe, expect, it, vi } from 'vitest';
import UnifiedQueryBar from './UnifiedQueryBar.vue';

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({ selectedProjectId: '7' }),
}));

async function testRouter() {
  const router = createRouter({
    history: createMemoryHistory(),
    routes: [{ path: '/', component: { template: '<div />' } }],
  });
  await router.push('/');
  await router.isReady();
  return router;
}

describe('UnifiedQueryBar', () => {
  it('offers source aliases and keeps submitted query text in the URL', async () => {
    const router = await testRouter();
    const view = render(UnifiedQueryBar, {
      props: { modelValue: '', source: 'logs' },
      global: { plugins: [router] },
    });
    const input = screen.getByRole('searchbox', { name: 'Query' });
    expect(input).toHaveAttribute(
      'placeholder',
      'Filter Logs: msg:"connection refused", svc:payments, or level:error',
    );

    await fireEvent.update(input, 's');
    await view.rerender({ modelValue: 's', source: 'logs' });
    await fireEvent.focus(input);
    expect(screen.getByRole('option', { name: /svc/ })).toBeVisible();

    await fireEvent.update(input, 'level:error svc:api');
    await view.rerender({ modelValue: 'level:error svc:api', source: 'logs' });
    await fireEvent.click(screen.getByRole('button', { name: 'Search' }));

    await waitFor(() => expect(router.currentRoute.value.query.q).toBe('level:error svc:api'));
    expect(view.emitted().submit).toHaveLength(1);
  });

  it('uses a different closed field schema for every source', async () => {
    const router = await testRouter();
    const view = render(UnifiedQueryBar, {
      props: { modelValue: 'l', source: 'replays' },
      global: { plugins: [router] },
    });
    const input = screen.getByRole('searchbox', { name: 'Query' });
    await fireEvent.focus(input);
    expect(screen.queryByRole('option', { name: /level/ })).not.toBeInTheDocument();

    await view.rerender({ modelValue: 'l', source: 'errors' });
    expect(screen.getByRole('option', { name: /level/ })).toBeVisible();

    await view.rerender({ modelValue: 'd', source: 'logs' });
    expect(screen.getByRole('option', { name: /date/ })).toBeVisible();

    await view.rerender({ modelValue: 'd', source: 'traces', allowedFields: ['service'] });
    expect(screen.queryByRole('option', { name: /dur/ })).not.toBeInTheDocument();
  });

  it('restores a source default without storing it in the URL', async () => {
    const router = await testRouter();
    const view = render(UnifiedQueryBar, {
      props: {
        modelValue: 'title:checkout',
        source: 'issues',
        defaultQuery: 'status:open',
        showReset: true,
      },
      global: { plugins: [router] },
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Reset' }));

    await waitFor(() => {
      const updates = view.emitted()['update:modelValue'];
      expect(updates?.at(-1)).toEqual(['status:open']);
      expect(router.currentRoute.value.query.q).toBeUndefined();
    });
  });
});
