import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  // Relative URLs throughout: standalone hoja serves the page at `/`, and a
  // Tracon node serves it under `/t/<token>/`.
  base: './',
  build: { outDir: 'dist', emptyOutDir: true },
  // `bun run dev` proxies the API to a running `hoja serve` on the default port.
  server: { proxy: { '/api': 'http://127.0.0.1:4520' } }
});
