import path from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// 开发期把 /ws 代理到 nomic --web 服务（缺省 127.0.0.1:3333，可用
// NOMIC_API_TARGET 覆盖）；生产构建同源访问（nomic 伺服 dist），无需代理。
// 单测配置在独立的 vitest.config.ts。
const apiTarget = process.env.NOMIC_API_TARGET ?? 'http://127.0.0.1:3333'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/ws': {
        target: apiTarget,
        changeOrigin: true,
        ws: true,
      },
    },
  },
})
