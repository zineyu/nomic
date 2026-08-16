import type { StorybookConfig } from '@storybook/react-vite'

// Storybook 10：essentials（controls/actions/viewport 等）已并入核心，
// 无需额外 addon；framework 用 react-vite 共享 vite 配置（含 @ 别名）。
const config: StorybookConfig = {
  stories: ['../src/**/*.stories.@(ts|tsx)'],
  addons: [],
  framework: {
    name: '@storybook/react-vite',
    options: {},
  },
  core: {
    disableTelemetry: true,
  },
}

export default config
