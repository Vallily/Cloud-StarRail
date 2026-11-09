import { defineConfig } from 'vite';
export default defineConfig({
  root: 'src-tauri/ui',
  server: { host: '127.0.0.1', port: 1420, strictPort: true },
  clearScreen: false,
  plugins: [{
    name: 'reload-local-window-ui',
    handleHotUpdate({ server }) {
      // Reload the local title/controls/dialog views, never the remote game page.
      server.ws.send({ type: 'full-reload', path: '*' });
      return [];
    },
  }],
});