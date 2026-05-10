import { defineConfig } from 'vitepress'
import { fileURLToPath } from 'url'
import path from 'path'

const __dirname = fileURLToPath(new URL('.', import.meta.url))

export default defineConfig({
  title: 'Supply Drop BBS',
  description: 'Open-source BBS for MeshCore LoRa radio networks',
  base: '/supply-drop-bbs/',
  srcDir: '../docs',

  // Links to repo-root files (LICENSE, config.example.toml, CONTRIBUTING, SECURITY)
  // and the adr/index stub are valid on GitHub but outside VitePress srcDir.
  ignoreDeadLinks: [/\/adr\/index/, /\.\.\//],

  themeConfig: {
    logo: '/supply-drop-icon-transparent.svg',

    nav: [
      { text: 'Home', link: '/' },
      { text: 'Docs', link: '/USER_GUIDE' },
      {
        text: 'GitHub',
        link: 'https://github.com/Mesh-America/supply-drop-bbs',
      },
      { text: 'Mesh America', link: 'https://meshamerica.com' },
    ],

    sidebar: [
      {
        text: 'Getting Started',
        items: [
          { text: 'User Guide', link: '/USER_GUIDE' },
          { text: 'Operations', link: '/OPERATIONS' },
          { text: 'CLI Reference', link: '/CLI' },
        ],
      },
      {
        text: 'Configuration',
        items: [{ text: 'Configuration Reference', link: '/CONFIG' }],
      },
      {
        text: 'Guides',
        items: [
          { text: 'Architecture', link: '/ARCHITECTURE' },
          { text: 'Protocol Notes', link: '/PROTOCOL' },
          { text: 'Transport Plugins', link: '/TRANSPORT_PLUGINS' },
        ],
      },
      {
        text: 'Plugin API',
        items: [{ text: 'Plugin API Guide', link: '/PLUGIN_API' }],
      },
      {
        text: 'Architecture Decision Records',
        collapsed: true,
        items: [
          { text: 'ADR-0001: License', link: '/adr/0001-license' },
          { text: 'ADR-0002: Process Model', link: '/adr/0002-process-model' },
          { text: 'ADR-0003: Web UI as Plugin', link: '/adr/0003-web-ui-as-plugin' },
          {
            text: 'ADR-0004: Cargo Features',
            link: '/adr/0004-cargo-features-not-runtime-plugins',
          },
          { text: 'ADR-0005: DB Strategy', link: '/adr/0005-db-strategy' },
          {
            text: 'ADR-0006: No mesh-citadel Migration',
            link: '/adr/0006-no-migration-from-mesh-citadel',
          },
          {
            text: 'ADR-0007: pymc_core Bridge',
            link: '/adr/0007-bridge-stays-pymc-core',
          },
          {
            text: 'ADR-0008: TOML Config',
            link: '/adr/0008-toml-config-with-env-overrides',
          },
          {
            text: 'ADR-0009: Tracing Config',
            link: '/adr/0009-tracing-config-respected',
          },
          {
            text: 'ADR-0010: OpenAPI from Rust',
            link: '/adr/0010-openapi-from-rust',
          },
          {
            text: 'ADR-0011: Transport-Agnostic Core',
            link: '/adr/0011-transport-protocol-agnostic-core',
          },
          {
            text: 'ADR-0012: Persistence Layer',
            link: '/adr/0012-persistence-layer',
          },
          {
            text: 'ADR-0013: Native Serial Transport',
            link: '/adr/0013-native-serial-transport-for-usb-devices',
          },
        ],
      },
    ],

    socialLinks: [
      {
        icon: 'github',
        link: 'https://github.com/Mesh-America/supply-drop-bbs',
      },
    ],

    footer: {
      message: 'Released under the Apache 2.0 + Commons Clause License.',
      copyright: 'Copyright © 2024-present Mesh America',
    },

    search: {
      provider: 'local',
    },
  },

  // Vue must resolve from docs-site/node_modules when srcDir is outside the project root.
  vite: {
    resolve: {
      alias: {
        vue: path.resolve(__dirname, '../node_modules/vue'),
      },
    },
  },
})
