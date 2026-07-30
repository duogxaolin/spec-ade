import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { quasar, transformAssetUrls } from '@quasar/vite-plugin';

// Vite dev/build config for the Spec ADE SPA.
//
// In dev, proxy /api to the Rust backend so the SPA and server share one
// origin model (docs/analysis/02-architecture.md). In production the built
// dist/ is embedded in the binary, so no proxy applies.
export default defineConfig({
  plugins: [
    vue({ template: { transformAssetUrls } }),
    // TODO(phase-0): point sassVariables at the project's Quasar variables file.
    quasar(),
  ],
  server: {
    proxy: {
      // REST + WS + SSE all live under /api on the backend (default :4123).
      '/api': {
        target: 'http://127.0.0.1:4123',
        changeOrigin: true,
        ws: true,
      },
    },
  },
  build: {
    // Output consumed by the Rust binary's embedder (rust-embed/memory-serve).
    outDir: 'dist',
  },
});
