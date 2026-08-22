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
  it('类别映射到 foreground 明度阶梯（单色体系）', () => {
    expect(toolCategoryIconClass('bash')).toBe('text-foreground')
    expect(toolCategoryIconClass('read')).toBe('text-foreground/75')
    expect(toolCategoryIconClass('edit')).toBe('text-foreground/60')
    expect(toolCategoryIconClass('ask_user_question')).toBe('text-foreground/45')
    expect(toolCategoryIconClass('create_agent')).toBe('text-foreground/35')
  })

  it('未知工具回退 muted', () => {
    expect(toolCategoryIconClass('web_search')).toBe('text-muted-foreground')
  })
})
