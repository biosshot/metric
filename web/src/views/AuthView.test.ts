import { fireEvent, render, screen, waitFor } from '@testing-library/vue';
import { createPinia } from 'pinia';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createMemoryHistory, createRouter } from 'vue-router';
import AuthView from './AuthView.vue';

function memoryStorage(): Storage {
  const values = new Map<string, string>();
  return {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => values.delete(key),
    setItem: (key, value) => values.set(key, value),
  };
}

describe('AuthView invitation route', () => {
  beforeEach(() => {
    vi.stubGlobal('localStorage', memoryStorage());
    vi.stubGlobal('sessionStorage', memoryStorage());
  });

  it('reacts to an invitation URL after the auth view is already mounted', async () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [
        { path: '/issues', name: 'issues', component: { template: '<div />' } },
        { path: '/auth/setup', name: 'password-setup', component: { template: '<div />' } },
      ],
    });
    await router.push('/issues');
    await router.isReady();
    render(AuthView, { global: { plugins: [createPinia(), router] } });

    const token = 'b'.repeat(64);
    await router.push(`/auth/setup?setup_token=${token}`);

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Set your password' })).toBeVisible();
      expect(screen.getByLabelText('Setup token')).toHaveValue(token);
      expect(screen.queryByLabelText('Organization ID')).not.toBeInTheDocument();
    });
  });

  it('suggests the organization slug from its name during first setup', async () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [{ path: '/', name: 'auth', component: { template: '<div />' } }],
    });
    await router.push('/');
    await router.isReady();
    render(AuthView, { global: { plugins: [createPinia(), router] } });

    await fireEvent.click(screen.getByRole('tab', { name: 'First setup' }));
    await fireEvent.update(screen.getByLabelText('Organization'), 'Платёжный сервис');

    expect(screen.getByLabelText(/^Slug/)).toHaveValue('platezhnyy-servis');
  });
});
