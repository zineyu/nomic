import '@testing-library/jest-dom/vitest'

// Radix UI tooltip/popover 的 Popper 定位依赖 ResizeObserver，
// jsdom 未实现，这里提供最小 mock。
class ResizeObserverMock {
  observe() {}
  unobserve() {}
  disconnect() {}
}

beforeAll(() => {
  globalThis.ResizeObserver = ResizeObserverMock
})

afterAll(() => {
  // @ts-expect-error 测试结束后清理 mock
  delete globalThis.ResizeObserver
})
