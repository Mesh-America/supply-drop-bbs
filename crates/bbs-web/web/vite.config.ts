import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// `base: './'` makes the built index.html reference assets with relative
// paths so the SPA works when served from any subpath.
// The dev proxy forwards /api/* to the BBS backend so `npm run dev` works
// without CORS configuration.
export default defineConfig({
  plugins: [vue()],
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
