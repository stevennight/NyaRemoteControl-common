import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

// The main page of NyaRemoteControl.exe (windows repository), embedded by
// crates/nya-webui: remote control of other computers (src/client) and this
// computer as a host (src/host, the "本机" section).
export default defineConfig({
  plugins: [svelte()],
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'chrome110', // WebView2 (Chromium) only
    rollupOptions: {
      input: { client: 'client.html' },
    },
  },
  server: { port: 5173, strictPort: true },
});
