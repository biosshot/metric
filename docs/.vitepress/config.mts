import { defineConfig } from 'vitepress';

export default defineConfig({
  title: 'Metric',
  description: 'User documentation for Metric',
  lang: 'en-US',
  base: process.env.DOCS_BASE ?? '/metric/',
  cleanUrls: true,
  lastUpdated: true,
  markdown: {
    lineNumbers: true,
  },
  themeConfig: {
    nav: [
      { text: 'Get started', link: '/getting-started' },
      { text: 'Configuration', link: '/configuration' },
      { text: 'GitHub', link: 'https://github.com/biosshot/metric' },
    ],
    sidebar: [
      {
        text: 'Get started',
        items: [
          { text: 'Overview', link: '/' },
          { text: 'Install Metric', link: '/getting-started' },
          { text: 'First setup', link: '/first-setup' },
          { text: 'Connect an SDK', link: '/sdk-setup' },
        ],
      },
      {
        text: 'Run Metric',
        items: [
          { text: 'Docker', link: '/docker' },
          { text: 'Configuration', link: '/configuration' },
          { text: 'Running Metric', link: '/operations' },
          { text: 'Update Metric', link: '/upgrading' },
          { text: 'Troubleshooting', link: '/troubleshooting' },
        ],
      },
      {
        text: 'Reference',
        items: [
          { text: 'Supported features', link: '/supported-capabilities' },
          { text: 'SDK compatibility', link: '/compatibility' },
          { text: 'Known limits', link: '/known-limits' },
          { text: 'Capacity and sizing', link: '/capacity' },
        ],
      },
    ],
    search: {
      provider: 'local',
    },
    outline: {
      level: [2, 3],
      label: 'On this page',
    },
    editLink: {
      pattern: 'https://github.com/biosshot/metric/edit/main/docs/:path',
      text: 'Edit this page on GitHub',
    },
    socialLinks: [{ icon: 'github', link: 'https://github.com/biosshot/metric' }],
    footer: {
      message: 'Released under the MIT License.',
      copyright: 'Metric contributors',
    },
  },
});
