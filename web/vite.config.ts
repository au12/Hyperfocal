import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  base: './',
  plugins: [svelte()],
  build: {
    target: 'es2022',
    cssCodeSplit: false,
    chunkSizeWarningLimit: 900,
  },
  server: {
    proxy: { '/api': 'http://localhost:8099' },
  },
});
