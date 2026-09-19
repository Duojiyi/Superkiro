import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

export default defineConfig({
  base: '/admin/',
  plugins: [react()],
  server: {
    port: 3000,
    proxy: {
      '/admin': {
        target: 'http://127.0.0.1:19820',
        changeOrigin: true,
      },
    },
  },
});
