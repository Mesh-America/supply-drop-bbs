import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { ViteImageOptimizer } from 'vite-plugin-image-optimizer'

// `base: './'` makes the built index.html reference assets with relative
// paths so the SPA works when served from any subpath.
// The dev proxy forwards /api/* to the BBS backend so `npm run dev` works
// without CORS configuration.
export default defineConfig({
  plugins: [
    vue(),
    // Compress images at build time so they don't bloat the rust-embed binary.
    // The logo PNG alone was ~770 KB; lossy PNG compression brings it well under 100 KB.
    ViteImageOptimizer({
      png: {
        // quality 80 gives excellent visual quality at a fraction of the size.
        quality: 80,
      },
      jpeg: { quality: 80 },
      jpg:  { quality: 80 },
    }),
  ],
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    sourcemap: false,
  },
  server: {
    port: 5173,
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8080',
        changeOrigin: false,
        ws: false,
      },
    },
  },
})
