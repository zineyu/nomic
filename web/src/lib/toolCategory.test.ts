// toolCategory / toolCategoryIconClass 测试。

import { describe, expect, it } from 'vitest'

import { toolCategory, toolCategoryIconClass } from './toolCategory'

describe('toolCategory', () => {
  it('bash 归入执行类', () => {
    expect(toolCategory('bash')).toBe('exec')
  })

  it('read/grep/find 归入查看类', () => {
    expect(toolCategory('read')).toBe('inspect')
    expect(toolCategory('grep')).toBe('inspect')
    expect(toolCategory('find')).toBe('inspect')
  })

  it('write/edit 归入修改类', () => {
    expect(toolCategory('write')).toBe('modify')
    expect(toolCategory('edit')).toBe('modify')
  })

  it('todo/ask 归入交互类', () => {
    expect(toolCategory('todo_read')).toBe('interact')
    expect(toolCategory('todo_write')).toBe('interact')
    expect(toolCategory('ask_user_question')).toBe('interact')
  })

  it('agent 系列归入代理类', () => {
    expect(toolCategory('create_agent')).toBe('agent')
    expect(toolCategory('send_message')).toBe('agent')
    expect(toolCategory('wait_result')).toBe('agent')
    expect(toolCategory('wait_all')).toBe('agent')
    expect(toolCategory('close_agent')).toBe('agent')
    expect(toolCategory('list_agents')).toBe('agent')
  })

  it('未知工具归入 other', () => {
    expect(toolCategory('web_search')).toBe('other')
  })
})

describe('toolCategoryIconClass', () => {
  it('类别映射到 chart 色板', () => {
    expect(toolCategoryIconClass('bash')).toBe('text-chart-1')
    expect(toolCategoryIconClass('read')).toBe('text-chart-2')
    expect(toolCategoryIconClass('edit')).toBe('text-chart-3')
    expect(toolCategoryIconClass('ask_user_question')).toBe('text-chart-4')
    expect(toolCategoryIconClass('create_agent')).toBe('text-chart-5')
  })

  it('未知工具回退 muted', () => {
    expect(toolCategoryIconClass('web_search')).toBe('text-muted-foreground')
  })
})
