import path from 'node:path'
import { defineConfig } from 'vitest/config'

// 单测配置独立文件：Storybook 与 vite build 不共享 test 字段。
// jsdom 环境 + Testing Library；global 注入见 tsconfig 的 types。
export default defineConfig({
  resolve: {
    alias: {
      '@': path.resolve(__dirname, './src'),
    },
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test/setup.ts'],
    css: true,
    include: ['src/**/*.test.{ts,tsx}'],
  },
})
