import { render, screen } from '@testing-library/vue';
import { describe, expect, it } from 'vitest';
import RelatedSignals from './RelatedSignals.vue';

const longId = '0123456789abcdef0123456789abcdef';
const routerLinkStub = {
  props: ['to'],
  template:
    '<a :href="typeof to === \'string\' ? to : to.path" :data-to="JSON.stringify(to)"><slot /></a>',
};

describe('RelatedSignals', () => {
  it('does not render a panel without exact links', () => {
    const { container } = render(RelatedSignals, {
      props: { links: [] },
      global: { stubs: { RouterLink: routerLinkStub } },
    });
    expect(container).toBeEmptyDOMElement();
  });

  it('renders route objects and shortens IDs without changing their target', () => {
    render(RelatedSignals, {
      props: {
        links: [
          {
            key: 'trace',
            icon: 'traces',
            label: 'Open trace',
            description: longId,
            to: { path: `/traces/${longId}`, query: { span: 'feedface' } },
          },
        ],
      },
      global: {
        stubs: {
          RouterLink: routerLinkStub,
        },
      },
    });

    const description = screen.getByTitle(longId);
    expect(description).toHaveTextContent('0123456789ab…cdef');
    expect(screen.getByRole('link', { name: /Open trace/ })).toHaveAttribute(
      'data-to',
      JSON.stringify({ path: `/traces/${longId}`, query: { span: 'feedface' } }),
    );
  });
});
