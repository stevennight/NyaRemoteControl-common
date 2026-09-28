import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// Two pages, embedded into the executables by crates/nya-webui:
//   client.html  – nya-client launcher
//   manager.html – nya-server (host manager)
export default defineConfig({
  plugins: [svelte()],
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'chrome110', // WebView2 (Chromium) only
    rollupOptions: {
      input: { client: 'client.html', manager: 'manager.html' },
    },
  },
  server: { port: 5173, strictPort: true },
});
