import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  base: '/wtn/',
  plugins: [react()],
  server: {
    proxy: {
      '/wtn/api': 'http://127.0.0.1:3174',
    },
  },
})
