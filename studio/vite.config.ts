import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import { resolve } from 'path'

export default defineConfig({
  plugins: [vue()],
  resolve: {
    alias: {
      '@': resolve(__dirname, 'src'),
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/collections': 'http://localhost:8108',
      '/multi_search': 'http://localhost:8108',
      '/aliases': 'http://localhost:8108',
      '/synonym_sets': 'http://localhost:8108',
      '/presets': 'http://localhost:8108',
      '/stopwords': 'http://localhost:8108',
      '/keys': 'http://localhost:8108',
      '/health': 'http://localhost:8108',
      '/admin': 'http://localhost:8108',
      '/metrics': 'http://localhost:8108',
      '/api': 'http://localhost:8108',
    },
  },
  build: {
    outDir: 'dist',
  },
})
