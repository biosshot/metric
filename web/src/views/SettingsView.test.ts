import { render, screen } from '@testing-library/vue';
import { describe, expect, it, vi } from 'vitest';
import SettingsView from './SettingsView.vue';

vi.mock('../stores/session', () => ({
  useSessionStore: () => ({
    has: () => false,
  }),
}));

describe('SettingsView authorization boundary', () => {
  it('does not advertise project administration pages to a member', () => {
    render(SettingsView, {
      global: {
        stubs: {
          RouterLink: { props: ['to'], template: '<a :href="to"><slot /></a>' },
          RouterView: true,
        },
      },
    });

    expect(screen.queryByRole('link', { name: /Notifications/i })).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: /Data & access/i })).toBeVisible();
    expect(screen.getByRole('link', { name: /Organization/i })).toBeVisible();
  });
});
