import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

// This runs in Node.js - Don't use client-side code here (browser APIs, JSX...)

const config: Config = {
  headTags: [
    { tagName: 'link', attributes: { rel: 'preload', href: '/fonts/anton-400.woff2', as: 'font', type: 'font/woff2', crossorigin: 'anonymous' } },
    { tagName: 'link', attributes: { rel: 'preload', href: '/fonts/space-grotesk-400.woff2', as: 'font', type: 'font/woff2', crossorigin: 'anonymous' } },
    { tagName: 'link', attributes: { rel: 'preload', href: '/fonts/space-grotesk-700.woff2', as: 'font', type: 'font/woff2', crossorigin: 'anonymous' } },
    { tagName: 'link', attributes: { rel: 'preload', href: '/fonts/jetbrains-mono-400.woff2', as: 'font', type: 'font/woff2', crossorigin: 'anonymous' } },
  ],
  title: 'Midnight Manual',
  tagline: 'Ask your docs, not your model.',
  favicon: 'img/favicon.svg',

  // Future flags, see https://docusaurus.io/docs/api/docusaurus-config#future
  future: {
    v4: true, // Improve compatibility with the upcoming Docusaurus v4
  },

  url: 'https://manual.midnightntwrk.expert',
  baseUrl: '/',
  trailingSlash: false,

  organizationName: 'devrelaicom',
  projectName: 'midnight-manual',

  onBrokenLinks: 'throw',

  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'throw',
    },
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          editUrl: 'https://github.com/devrelaicom/midnight-manual/tree/main/site/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themes: [
    ['@easyops-cn/docusaurus-search-local', { hashed: true, indexBlog: false }],
  ],

  plugins: [
    ['@signalwire/docusaurus-plugin-llms-txt', {
      content: {
        enableMarkdownFiles: true,
        enableLlmsFullTxt: false,
        includeDocs: true,
        includeBlog: false,
        includePages: false,
        excludeRoutes: ['/search'],
      },
    }],
  ],

  themeConfig: {
    image: 'img/og.png',
    colorMode: { defaultMode: 'light', respectPrefersColorScheme: true },
    navbar: {
      title: 'Midnight Manual',
      items: [
        { to: '/docs/intro', label: 'Get started', position: 'left' },
        { href: 'https://github.com/devrelaicom/midnight-manual', label: 'GitHub', position: 'right' },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [
            { label: 'Introduction', to: '/docs/intro' },
          ],
        },
        {
          title: 'More',
          items: [
            { label: 'GitHub', href: 'https://github.com/devrelaicom/midnight-manual' },
            { label: 'llms.txt', to: 'pathname:///llms.txt' },
          ],
        },
      ],
      copyright: 'Apache-2.0 OR MIT · Midnight Manual',
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
