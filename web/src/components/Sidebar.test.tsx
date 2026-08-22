// Sidebar 测试：模仿 DeepSeek Harness 布局。
//
// 按 workspace 分组（可折叠）的会话列表，工作区组标题为卡片样式。
// 组标题右侧带「新建会话」按钮（在该 workspace 下创建）；「工作区」标题行带
// 「添加工作区」内联输入。每个 session 只展示标题，长标题被 CSS 截断但可通过
// title 读取全文；当前会话以主色高亮；侧栏与聊天区之间有右侧分隔边框。
// 无默认 workspace：新建会话必须经组标题按钮显式指定归属。

import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'

import { Sidebar } from './Sidebar'

const LONG_TITLE =
  '左侧session列表与右侧会话界面不够清晰明确，session名称被硬遮盖，需要调整布局与对比度让左右两侧边界分明'

const sessions = [
  { id: 'a', title: LONG_TITLE, workspace_id: 'wa', workspace: '/tmp', first_message_at: null, last_message_at: 1786960000000, message_count: 101 },
  { id: 'b', title: null, workspace_id: 'wb', workspace: '/tmp', first_message_at: null, last_message_at: 1786960000000, message_count: 3 },
]

const groupedSessions = [
  { id: 'a1', title: '项目 A 会话一', workspace_id: 'wa', workspace: '/home/zine/alpha', first_message_at: null, last_message_at: 300, message_count: 5 },
  { id: 'b1', title: '项目 B 会话一', workspace_id: 'wb', workspace: '/home/zine/beta', first_message_at: null, last_message_at: 200, message_count: 2 },
  { id: 'a2', title: '项目 A 会话二', workspace_id: 'wa', workspace: '/home/zine/alpha', first_message_at: null, last_message_at: 100, message_count: 1 },
]

function workspace(id: string, path: string, sessionCount = 0) {
  return { id, path, session_count: sessionCount, last_active_at: null }
}

function renderSidebar(overrides: Partial<Parameters<typeof Sidebar>[0]> = {}) {
  return render(
    <Sidebar
      sessions={sessions}
      workspaces={[]}
      currentSessionId="a"
      workspace="/tmp"
      running={false}
      onNewSession={vi.fn()}
      onAddWorkspace={vi.fn().mockResolvedValue(undefined)}
      onResume={vi.fn()}
      {...overrides}
    />,
  )
}

describe('Sidebar', () => {
  it('会话标题带完整 title 提示，长标题悬停可读全文', () => {
    renderSidebar()
    const title = screen.getByText(LONG_TITLE)
    expect(title).toHaveAttribute('title', LONG_TITLE)
    // 缺省标题回退为「新会话」（在会话列表中）
    const fallbackTitles = screen.getAllByText('新会话')
    // 至少有一个带 title="新会话" 的会话按钮
    expect(fallbackTitles.some((el) => el.getAttribute('title') === '新会话')).toBe(true)
    // 简洁风格：列表项不展示消息数、时间等元信息
    expect(screen.queryByText(/101 条消息/)).not.toBeInTheDocument()
  })

  it('侧栏根部带右侧分隔边框（与聊天区边界清晰）', () => {
    renderSidebar()
    const root = screen.getByText('工作区').closest('div')
    expect(root?.parentElement).not.toBeNull()
    // 向上找到主容器 div（有 border-r）
    let container = root?.parentElement
    while (container && !container.className.includes('border-r')) {
      container = container.parentElement
    }
    expect(container).not.toBeNull()
    expect(container?.className).toContain('border-r')
    expect(container?.className).toContain('border-sidebar-border')
  })

  it('当前会话高亮并标记 aria-current', () => {
    renderSidebar()
    const active = screen.getByRole('button', { name: new RegExp(LONG_TITLE) })
    expect(active).toHaveAttribute('aria-current', 'page')
  })

  it('运行中当前会话显示脉冲指示点', () => {
    renderSidebar({ running: true })
    const active = screen.getByRole('button', { name: new RegExp(LONG_TITLE) })
    expect(active.querySelector('span[aria-hidden="true"]')).not.toBeNull()
  })

  it('会话按 workspace 分组，组标题为路径最后一段且 title 为完整路径', () => {
    renderSidebar({ sessions: groupedSessions, workspace: '/home/zine/alpha' })
    const alpha = screen.getByRole('heading', { name: /alpha/ })
    const beta = screen.getByRole('heading', { name: /beta/ })
    // title 完整路径在组标题的折叠按钮上（折叠按钮名以组名开头，区别于「新建会话」按钮）
    expect(within(alpha).getByRole('button', { name: /^alpha/ })).toHaveAttribute(
      'title',
      '/home/zine/alpha',
    )
    expect(within(beta).getByRole('button', { name: /^beta/ })).toHaveAttribute(
      'title',
      '/home/zine/beta',
    )

    // 组顺序：最新会话更活跃的 alpha 组在前；组内保持活跃度排序
    const headings = screen.getAllByRole('heading').map((h) => h.textContent)
    expect(headings[0]).toContain('alpha')
    expect(headings[1]).toContain('beta')

    // 会话归属到对应分组（section aria-label 为 workspace 路径）
    const alphaGroup = screen.getByRole('region', { name: '/home/zine/alpha' })
    expect(
      within(alphaGroup).getAllByRole('button', { name: /^项目 A/ }).map((b) => b.textContent),
    ).toEqual(['项目 A 会话一', '项目 A 会话二'])
    const betaGroup = screen.getByRole('region', { name: '/home/zine/beta' })
    expect(within(betaGroup).getAllByRole('button', { name: /^项目 B/ })).toHaveLength(1)
  })

  it('当前 workspace 所在分组带「当前」标记', () => {
    renderSidebar({ sessions: groupedSessions, workspace: '/home/zine/beta' })
    const betaGroup = screen.getByRole('region', { name: '/home/zine/beta' })
    expect(within(betaGroup).getByText('当前')).toBeInTheDocument()
    const alphaGroup = screen.getByRole('region', { name: '/home/zine/alpha' })
    expect(within(alphaGroup).queryByText('当前')).not.toBeInTheDocument()
  })

  it('工作区组标题为卡片样式，当前工作区高亮，且不存在额外的固定工作区卡片', () => {
    renderSidebar({ sessions: groupedSessions, workspace: '/home/zine/alpha' })
    const alphaToggle = screen.getByRole('button', { name: /^alpha/ })
    const betaToggle = screen.getByRole('button', { name: /^beta/ })
    // 实际工作区采用卡片样式（圆角 + accent 底色）
    expect(alphaToggle.className).toContain('rounded-lg')
    expect(betaToggle.className).toContain('bg-sidebar-accent/50')
    // 当前工作区以更强的 accent 底色高亮
    expect(alphaToggle.className).toContain('bg-sidebar-accent')
    expect(alphaToggle.className).not.toContain('bg-sidebar-accent/50')
    // 不再有固定工作区卡片：工作区路径只出现在组标题的 title 上
    const titled = screen.getAllByTitle('/home/zine/alpha')
    expect(titled).toHaveLength(1)
    expect(titled[0]).toBe(alphaToggle)
  })

  it('组标题可折叠/展开，默认全部展开，且各组折叠互不影响', async () => {
    const user = userEvent.setup()
    renderSidebar({ sessions: groupedSessions, workspace: '/home/zine/alpha' })

    const alphaToggle = screen.getByRole('button', { name: /^alpha/ })
    const betaToggle = screen.getByRole('button', { name: /^beta/ })
    // 默认展开
    expect(alphaToggle).toHaveAttribute('aria-expanded', 'true')
    expect(betaToggle).toHaveAttribute('aria-expanded', 'true')

    // 折叠 alpha 组：其会话消失，beta 组不受影响
    await user.click(alphaToggle)
    expect(alphaToggle).toHaveAttribute('aria-expanded', 'false')
    expect(screen.queryByRole('button', { name: '项目 A 会话一' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: '项目 B 会话一' })).toBeInTheDocument()

    // 再次点击恢复展开
    await user.click(alphaToggle)
    expect(alphaToggle).toHaveAttribute('aria-expanded', 'true')
    expect(screen.getByRole('button', { name: '项目 A 会话一' })).toBeInTheDocument()
  })

  it('折叠包含当前会话的组时，组标题保留活跃指示点', async () => {
    const user = userEvent.setup()
    renderSidebar({
      sessions: groupedSessions,
      currentSessionId: 'a1',
      workspace: '/home/zine/alpha',
    })
    const alphaGroup = screen.getByRole('region', { name: '/home/zine/alpha' })
    const alphaToggle = within(alphaGroup).getByRole('button', { name: /^alpha/ })

    // 展开时无指示点（会话本身可见高亮）
    expect(alphaToggle.querySelector('span.bg-foreground[aria-hidden="true"]')).toBeNull()

    await user.click(alphaToggle)
    expect(alphaToggle.querySelector('span.bg-foreground[aria-hidden="true"]')).not.toBeNull()
  })

  it('展开的会话列表缩进在组标题下方并带竖向引导线，体现从属关系', () => {
    renderSidebar({ sessions: groupedSessions, workspace: '/home/zine/alpha' })
    const alphaGroup = screen.getByRole('region', { name: '/home/zine/alpha' })
    const sessionButton = within(alphaGroup).getByRole('button', { name: '项目 A 会话一' })
    // 会话列表容器：缩进 + 左侧引导线
    const list = sessionButton.parentElement
    expect(list?.className).toContain('ml-4')
    expect(list?.className).toContain('border-l')
    expect(list?.className).toContain('border-sidebar-border')
  })

  it('折叠组之间保持紧凑间距，展开的组用额外下边距分隔', async () => {
    const user = userEvent.setup()
    renderSidebar({ sessions: groupedSessions, workspace: '/home/zine/alpha' })
    const alphaGroup = screen.getByRole('region', { name: '/home/zine/alpha' })
    const betaGroup = screen.getByRole('region', { name: '/home/zine/beta' })

    // 默认展开：两组都带额外下边距
    expect(alphaGroup.className).toContain('mb-2')
    expect(betaGroup.className).toContain('mb-2')

    // 折叠后取消额外下边距，组间仅保留紧凑基础间距
    await user.click(within(alphaGroup).getByRole('button', { name: /^alpha/ }))
    await user.click(within(betaGroup).getByRole('button', { name: /^beta/ }))
    expect(alphaGroup.className).not.toContain('mb-2')
    expect(betaGroup.className).not.toContain('mb-2')
  })

  it('不存在不指定 workspace 的顶部新会话按钮（无默认 workspace）', () => {
    renderSidebar({ sessions: [] })
    // 「新会话」文本只允许出现在缺省标题回退的会话列表项中；
    // 空会话列表下不应有任何「新会话」按钮
    expect(screen.queryByRole('button', { name: '新会话' })).not.toBeInTheDocument()
  })

  it('组标题「新建会话」按钮在该 workspace 下创建会话', async () => {
    const user = userEvent.setup()
    const onNewSession = vi.fn()
    renderSidebar({ sessions: groupedSessions, workspace: '/home/zine/alpha', onNewSession })
    const alphaGroup = screen.getByRole('region', { name: '/home/zine/alpha' })
    await user.click(
      within(alphaGroup).getByRole('button', { name: '在 /home/zine/alpha 下新建会话' }),
    )
    expect(onNewSession).toHaveBeenCalledWith('/home/zine/alpha')
  })

  it('无会话的已登记 workspace 也展示为空组', () => {
    renderSidebar({
      sessions: groupedSessions,
      workspaces: [
        workspace('wa', '/home/zine/alpha', 2),
        workspace('wc', '/home/zine/gamma'),
      ],
      workspace: '/home/zine/alpha',
    })
    // 无会话的 gamma 也成组（顺序跟 workspace 列表）
    const gammaGroup = screen.getByRole('region', { name: '/home/zine/gamma' })
    expect(within(gammaGroup).getByText('暂无会话')).toBeInTheDocument()
    const headings = screen.getAllByRole('heading').map((h) => h.textContent)
    expect(headings.map((h) => h?.replace(/\d+/g, ''))).toEqual([
      expect.stringContaining('alpha'),
      expect.stringContaining('gamma'),
      expect.stringContaining('beta'),
    ])
  })

  it('添加工作区：内联输入回车提交，成功后关闭输入框', async () => {
    const user = userEvent.setup()
    const onAddWorkspace = vi.fn().mockResolvedValue(undefined)
    renderSidebar({ onAddWorkspace })

    await user.click(screen.getByRole('button', { name: '添加工作区' }))
    const input = screen.getByRole('textbox', { name: '工作区路径' })
    await user.type(input, '~/code/proj{Enter}')
    expect(onAddWorkspace).toHaveBeenCalledWith('~/code/proj')
    // 成功后输入框关闭
    expect(screen.queryByRole('textbox', { name: '工作区路径' })).not.toBeInTheDocument()
  })

  it('添加工作区失败时就地展示错误，输入框保留', async () => {
    const user = userEvent.setup()
    const onAddWorkspace = vi.fn().mockRejectedValue(new Error('目录不存在：/nope'))
    renderSidebar({ onAddWorkspace })

    await user.click(screen.getByRole('button', { name: '添加工作区' }))
    const input = screen.getByRole('textbox', { name: '工作区路径' })
    await user.type(input, '/nope{Enter}')
    expect(await screen.findByRole('alert')).toHaveTextContent('目录不存在：/nope')
    expect(screen.getByRole('textbox', { name: '工作区路径' })).toBeInTheDocument()
  })

  it('添加工作区输入框 Esc 取消', async () => {
    const user = userEvent.setup()
    const onAddWorkspace = vi.fn()
    renderSidebar({ onAddWorkspace })

    await user.click(screen.getByRole('button', { name: '添加工作区' }))
    await user.type(screen.getByRole('textbox', { name: '工作区路径' }), '/tmp{Escape}')
    expect(onAddWorkspace).not.toHaveBeenCalled()
    expect(screen.queryByRole('textbox', { name: '工作区路径' })).not.toBeInTheDocument()
  })
})
