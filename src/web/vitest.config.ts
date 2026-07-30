import { fileURLToPath } from 'node:url';

import { quasar, transformAssetUrls } from '@quasar/vite-plugin';
import vue from '@vitejs/plugin-vue';
import { defineConfig } from 'vitest/config';

// Test config, kept separate from vite.config.ts.
//
// Two reasons it is its own file rather than a `test` key on the build config:
// `defineConfig` from 'vite' does not type the `test` block (vue-tsc would reject
// it), and the SFC/Quasar plugins are needed here for a different purpose —
// compiling components for @vue/test-utils, not producing a bundle.
//
// The default environment stays `node`. Most suites are pure logic and jsdom's
// globals (WebSocket, fetch) would shadow the fakes that api/*.test.ts install.
// Files that need a DOM opt in with a `@vitest-environment jsdom` docblock
// (SPEC-004 §7.1) — notably anything touching DOMPurify, which refuses to run
// without a DOM and is the reason jsdom is installed at all.
export default defineConfig({
  plugins: [vue({ template: { transformAssetUrls } }), quasar()],
  resolve: {
    alias: {
      // Vue's ESM build resolves to a browser bundle; under vitest the runtime
      // compiler is needed for inline templates in component tests.
      vue: 'vue/dist/vue.esm-bundler.js',
    },
  },
  test: {
    include: ['src/**/*.test.ts'],
    root: fileURLToPath(new URL('.', import.meta.url)),
  },
});
