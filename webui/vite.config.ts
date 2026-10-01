import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

export default defineConfig({
  plugins: [react()],
  // Keep built assets relative to index.html so the WebUI works behind a
  // reverse proxy mounted at either the domain root or a URL prefix.
  base: './',
  server: {
    proxy: {
      '/api': 'http://localhost:8080',
      '/ws': { target: 'ws://localhost:8080', ws: true },
    },
  },
  build: {
    outDir: process.env.TNG_WEBUI_OUT_DIR ?? '../sidecar/static',
    emptyOutDir: true,
  },
})
