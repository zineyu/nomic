# Changelog

所有值得关注的变更记录于本文件。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)，
由 [git-cliff](https://git-cliff.org) 从 conventional commits 自动生成。

## [0.3.1] - 2026-08-24

### 代码风格

- 归位 rustfmt 并补齐 tracing 调用缺失的分号
- 修复存量 rustfmt/clippy 偏差（check 门禁前置修复）

### 修复

- **(chat)** Extract shared isEmptyAssistant check to eliminate double spacing between tool cards
- **(web)** Create_session 对齐查询式命令（request_id 关联 + 总线广播）
- **(core)** Agent 查询改读共享状态视图，修复运行中 session 的 get_state 超时

### 文档

- 新增 UI 规则（Minimalism 风格、70-20-10 配色、比例节奏、基础 token 一套）
- ADR-0034 web steering 队列，README 同步 web 排队语义

### 新功能

- **(logging)** Output JSON lines for file-based log target
- **(web)** 引入 gsap 动画适配层
- **(web)** 消息列表入场动画
- **(web)** 折叠面板高度动画
- **(web)** 错误横幅与最小化提问入口入场动画
- **(web)** 输入框支持粘贴图片附件
- **(design)** 引入淡蓝强调色与 warning token，重调中性色与类别色板
- **(web)** 上下文用量环增加 65–80% 琥珀警示档
- **(web)** 工具卡片图标按类别着色（chart 类别色板）
- Add structured tracing instrumentation across codebase
- **(session)** 无用户消息的空 session 不保留且不计入统计
- **(web)** 补齐 Web UI 微交互状态反馈
- **(web)** Steering 统一消息队列与运行中转向（后端）
- **(web)** 队列区展示排队消息并支持编辑（前端）
- **(web)** 输入框上方展示 todo 任务清单，完成项删除线置灰
- **(session)** SQLite 加固——全表重建为 STRICT，显式 busy_timeout，补齐特性保障测试
- **(todopanel)** Collapse todo list by default with expand toggle
- **(session)** 补齐 session/workspace 的删除与自定义标题
- **(web)** Workspace/session 的删除与重命名 WS 事件
- **(web)** 前端接线 workspace/session 的删除与重命名事件
- **(web)** 侧栏补齐会话重命名/删除与工作区删除入口

### 杂项

- **(deps)** Bump ignore in the rust-dependencies group (#19)
- **(deps)** Bump the rust-dependencies group with 3 updates (#21)

### 重构

- **(web)** 页面列水平内边距改为响应式（px-4 sm:px-7）
- **(web)** 窄屏防溢出：侧栏抽屉与下拉菜单宽度约束
- **(web)** 小屏组件适配：消息气泡放宽、操作栏可收缩
- **(web)** Api/handlers.rs 拆出 session/workspace 生命周期子模块
- **(session)** Workspace 列表按登记时间排序，不再随活跃度浮动
- **(web)** 移除「当前 workspace」概念
## [0.3.0] - 2026-08-21

### 代码风格

- **(web)** 聊天消息字号收细为 15px 并加宽消息间距

### 修复

- **(chat)** Preserve tool state in history and live updates
- **(sec)** Upgrade h2 to 0.4.16 (RUSTSEC-2026-0258)
- **(web)** Rail 徽标 nom 改回 n
- **(web)** 消息正文与工具卡片统一 72ch 阅读宽度
- **(web)** 助手消息复制按钮改为悬浮右下角，不再占用布局空间
- **(web)** 用户消息与思考内容右缘对齐 72ch 阅读列
- **(web)** 用户消息右缘对齐阅读列（去掉 ml-auto 导致的页面列偏移）
- **(web)** Resolve check fallout from session_id event refactor
- **(web)** ContextRing 运行中实时更新上下文用量

### 其他

- **(web)** 全面采用websocket通信

### 文档

- 同步 DESIGN.md 与 web 实现（深色 token、Inter Variable、组件定义）
- **(readme)** 会话恢复措辞从 cwd 隔离更新为 workspace 归属

### 新功能

- **(web)** Nomic --web 内置 HTTP 服务（axum REST + SSE 流式 + 前端伺服）
- **(web)** React + Vite + TailwindCSS + shadcn/ui 前端（web/）
- **(web)** Revamp UI with highlighting and theme
- Add DESIGN.md configuration for design system
- **(web)** Kimi 风格布局重构（模型选择移入输入区 + Topbar/Rail）
- **(web)** [**breaking**] 前端产物编译期内嵌进二进制（rust-embed）
- Add session stats to status bar
- **(reasoning)** Persist reasoning level across sessions
- **(web)** 默认字体改为 Maple Mono（@fontsource/maple-mono）
- **(web)** 助手消息支持复制与失败重试
- **(session)** Config 表支持会话级隔离（session_id 列 + 会话级读写 API）
- **(web)** 多 session 并行（SessionRuntime 注册表 + 会话级路由与模型持久化）
- **(session)** Workspace 成为一等实体，session 按 workspace 归属
- **(cli)** Session 操作以 workspace 路径为基准端到端接线
- **(web)** 侧栏会话列表按 workspace 分组，组可折叠
- **(session)** WorkspaceSummary 支持序列化
- **(web)** Create_session 支持指定 workspace，新增 workspace 登记/查询事件
- **(web)** 侧栏支持按组新建会话与添加工作区
- **(web)** 重新设计 favicon 并统一替换 UI 图标
- **(web)** 输入框支持 @skill:/@file: mention 与 /compact、/continue 命令
- **(core)** Actor 之上新增 SessionRunner 收敛会话级串行 job 语义

### 杂项

- **(web)** Devenv 集成 node/web-check + 文档
- **(deny)** Remove unused Unicode-DFS-2016 license allowance

### 重构

- **(web)** 侧栏改为简洁胶囊风格会话列表
- **(web)** 移除侧栏 cwd 提示，将上下文用量移入输入框环形指示器
- **(tui)** 模仿工具调用样式优化 thinking 块渲染
- **(web)** 模仿工具调用样式优化 thinking 胶囊
- **(web)** 前端设计对齐 DESIGN.md（Inter 字体、类型比例、token 化）
- **(web)** 布局列宽收敛为 max-w-reading/max-w-page 设计 token（单一来源）
- **(web)** Remove session id path parameter from websocket endpoint
- **(web)** Extract handlers module and add session_id to all events
- **(session)** Config 表方法拆分至独立模块
- **(web)** Require explicit workspace for session creation, remove default session
- **(cli)** Mention 模块移至 crate 根，供 TUI 与 Web 共享
- **(chat)** Remove max-w-reading constraint from message components
- **(cli)** 收敛三入口的 agent 工具配方到 agent_recipe 组装模块
- **(ai)** ThinkingLevel 字符串映射收敛到 nomic-ai（FromStr/Display + off 共享 helper）
- **(tui)** Driver 迁移到 core SessionRunner
- **(web)** Session runner 迁移到 core SessionRunner
- 在途提问生命周期提为共享 QuestionRegistry
## [0.2.0] - 2026-08-15

### 修复

- **(tui)** Reserve arrow for visual selection
- **(tui)** Resume/tree 恢复后消息游标失效
- Allow CopyMenu can view all messages
- **(session)** 并发写入改用 BEGIN IMMEDIATE，消除 WAL 写锁竞态

### 文档

- **(adr)** ADR-0018 BROWSE 默认态与输入框内嵌 edtui
- **(nomic-cli)** Prompt/注入文案说明子资源与 skill 根目录
- README + ADR-0003 修订记录子资源与可见性语义
- **(adr)** ADR-0021 单字母动作层交互设计（重设计快捷键）
- README 键位章节对齐 ADR-0021
- **(adr)** ADR-0022 agent 使用方式改为 actor 模型
- **(adr)** ADR-0023 落库策略收进 SessionRecorder
- **(adr)** ADR-0024 driver 状态按关注点下沉，模型切换收进显式状态机
- **(adr)** ADR-0025 聊天区几何上移状态层，渲染回写通道删除
- **(adr)** ADR-0027 steering 队列上移到 TUI，core 只保留注入点
- 完善 AGENTS.md 开发指引
- ADR-0021 修订与 README 键位表同步 Esc/q 新语义
- ADR-0020 修订与 README 同步浮层命令栏、无前缀命令语法
- ADR-0028 agent hooks 并入事件拦截

### 新功能

- Remove embend vim like editor (#15)
- **(tui)** NORMAL `?` 打开键位帮助弹层
- **(nomic-skills)** Skill:// 子路径资源解析与穿越防护
- **(nomic-tools)** Read 支持 skill://<name>/<path> 与目录清单
- **(nomic-skills)** Frontmatter 支持 enabled/hide
- **(nomic-cli)** 启动时输出 skill 加载诊断
- **(nomic-cli)** /skill:<name> 支持附带 args
- **(tui)** 专门的命令输入框（COMMAND 模式，ADR-0020）
- **(tui)** 消息游标与 VISUAL 选择区改为整行背景高亮
- **(tui)** NORMAL 进入 VISUAL 的快捷键由 Shift+V 改为 v
- **(tui)** VISUAL 模式条目折叠为单行摘要（oil.nvim 式），复制仍取全文
- **(tui)** NORMAL 单字母动作层（y 复制菜单、Space 条目折叠、r 重试，移除 VISUAL 与序列键）
- **(tui)** S 会话菜单 overlay（恢复/新建/分支树合一入口）
- **(tui)** INSERT Esc 回 NORMAL、Ctrl+C/D 清草稿/退出、↑/↓ 输入历史召回
- **(tui)** 会话快捷键分离——NORMAL `s`/`b`/`c` 直达恢复/分支/新建
- **(cli)** Add --cwd to set working directory
- **(core)** Agent actor 封装——AgentHandle 命令邮箱（ADR-0022）
- **(session)** SessionRecorder——落库策略收进 AgentEvent 流之后
- **(tui)** 取消运行快捷键收归 NORMAL q，Ctrl+C 统一为退出
- **(tui)** 草稿 @ mention 补全（skill/file），聊天区折叠展示
- **(tui)** [**breaking**] Esc 纯化为层回退、q 收敛为带守卫的退出
- **(tui)** [**breaking**] NORMAL q 纯化为中断键，退出收敛到 /quit 命令
- **(tui)** [**breaking**] 命令收敛到浮层命令栏，语法去 / 前缀
- 使用continue命令替换retry命令
- **(core)** Agent hooks 并入事件拦截（AgentInterceptor 多拦截器）
- **(tools)** Ask_user_question 工具（单选/多选/填空 + TUI 提问弹层）

### 杂项

- 增加函数长度与文件行数限制
- Edit AGENTS.md

### 重构

- **(tui)** 按关注点拆分 app 状态层为子模块
- **(tui)** 按 Effect 族拆分执行逻辑为 effects 子模块
- **(cli)** 拆出 model.rs 模型解析模块，bootstrap 只留启动装配
- **(tui)** 叠加层 g 单键到顶（help/queue），统一 less 式键位
- **(tui)** 渲染层改为自定义 ratatui Widget（ui.rs → widgets/ 模块）
- 拆分超 800 行文件以满足行数门禁，移除基线豁免
- **(cli)** TUI driver 改用 core actor（AgentHandle）
- **(cli)** Print 模式改用 core actor（AgentHandle）
- **(cli)** Print 与 TUI 落库改用 SessionRecorder（ADR-0023）
- **(core)** Context_tokens 单一权威——事件携带权威值，App 只抄不算
- **(tui)** Session 落库状态收进 effects::session 的 SessionBinding
- **(tui)** 两步模型切换收进 ModelSwitcher 状态机（ADR-0024）
- **(tui)** Goal 追问状态收进 GoalNudger（ADR-0024）
- **(tui)** Driver 字段全部降为私有——结构体对外只是不透明句柄
- **(picker)** 抽选择内核（selected/offset/window + 可选过滤），两处各留薄 adapter
- **(tui)** 聊天区几何上移状态层——渲染前按视口主动计算，删除渲染回写通道
- [**breaking**] 引入 dirs 替换手写 XDG 目录解析
- **(tui)** NORMAL 纯浏览化，移除消息游标与内容操作
- **(core)** Steering 队列上移到 TUI，core 只保留 TurnInjection 注入点
- **(ai)** 收敛两个 provider 的 empty_output 与截断 JSON 修复
- **(ai)** Openai reasoning 字段回退链收敛为 first_non_empty
- **(ai)** 抽出两个 provider 共用的带重试流式骨架
- **(session)** Append_message/append_compaction 收敛到共用 append_entry
- **(session)** Fetch_titles 用 slice::from_ref 避免临时 Vec
- **(cli)** Parse_slash 的命令参数尾巴解析收敛为 command_tail
- **(cli)** Collapse_mention_blocks 用枚举替代字符串 kind
- **(cli)** Print 模式两处小清理
- **(cli)** --model spec 的 provider 段解析收敛为 cli_model_provider
- **(core)** 工具调用预备结果用 PreparedToolCall 枚举替代 Result 控制流
- **(tui)** 命令相关标识符去 slash 命名
- **(tui)** 选择器弹层改为居中浮层（resume/models/tree/思考级别），不再贴输入框
- Crate 目录按 runtime/app 分层
- 具体工具实现移到 app 层，runtime 仅保留工具抽象
## [0.1.3] - 2026-08-11

### CI

- Patch 版本 GitHub Action 自动发版，发布成功后回写 main
- Set ci
- **(release)** Patch 发版改为创建 PR 回写 main，不再直接推送 (#5)
- **(release)** 统一为 PR 发版 + 合并后自动打 tag，Intel macOS 迁移 macos-15-intel (#6)

### 修复

- **(release)** 规范化 changelog 文件尾

### 其他

- Remove target of macos-intel (#4)

### 新功能

- **(tui)** Steering 与 follow-up 合并 (#11)
- **(tui)** Vim like editor

### 杂项

- **(deps)** Bump actions/checkout from 5 to 7 (#1)
- **(deps)** Bump cachix/cachix-action from 16 to 17 (#3)
- **(deps)** Bump actions/upload-artifact from 4 to 7 (#7)
- **(deps)** Bump softprops/action-gh-release from 2 to 3 (#8)
- **(deps)** Bump actions/download-artifact from 4 to 8 (#9)
- **(deps)** Bump the rust-dependencies group across 1 directory with 8 updates (#13)
## [0.1.1] - 2026-08-06

### 杂项

- Fix devenv test
## [0.1.0] - 2026-08-06

### CI

- 使用 devenv 统一执行 CI 检查
- Tag 触发的 release workflow（多平台二进制 + GitHub Release）

### 代码风格

- **(tui)** 输入框与弹层边框统一为直角

### 修复

- 修正 skill 覆盖优先级顺序
- 单个损坏的 skill 不再阻断整个 catalog
- 补齐 devenv.lock 末尾换行使 end-of-file-fixer 钩子通过
- **(tui)** 聊天区增加左右留白，输出不再紧贴屏幕边缘
- **(ai)** Models.dev 拉取超时放宽到 10s
- TUI 各消息块之间统一以空行分隔
- **(tui)** 移除聊天区「生成中」流式指示，运行状态仅由输入框标题表达
- **(cli)** 移除思考块开头的 Thinking 标记
- **(core)** 流建立前失败补发 MessageStart，保证消息事件配对
- **(tui)** /tree 按分叉缩进并折叠工具调用条目
- 仓库地址统一为 zineyu/nomic

### 文档

- README 路线图拆分已完成/未完成，ADR-0001 补现状修订
- 新增 config.example.toml 示例配置
- Init AGENTS.md
- ADR-0005 上下文压缩设计与文档
- ADR-0008 prompt templates 与 README 文档
- ADR 0009 sqlite 配置与模型选择迁移，README 同步
- **(adr)** 0011 vim-like 交互模式重构
- README 与欢迎页键位同步为模式化交互（ADR-0011）
- 说明 TUI 捕获鼠标，文本选择需 Shift+拖选
- 发布流程文档与 README 安装说明
- 更新README

### 新功能

- M1 核心 — Rust 复刻 pi-coding-agent 的 agent loop、provider 抽象与四工具
- 新增 nomic-session crate，SQLite session 存储层（树结构 entries + session 时间/cwd 摘要）
- Nomic-cli 接入 session 持久化（事件驱动，MessageEnd 落库）
- Session resume（--continue/--session 恢复历史消息续聊）
- 交互 TUI（ratatui 最小可交互版）— main.rs 拆分为 print/bootstrap/tui 模块，新增流式对话界面
- Read config form config file
- --continue 按 cwd 隔离恢复 + nomic sessions list 子命令
- AGENTS.md 上下文文件加载并注入系统提示词
- 实现 skills 系统并支持 read 读取 skill
- TUI slash 命令与自动补全
- Skill frontmatter 支持块标量与嵌套未知字段
- Agent 支持在两轮 prompt 之间注入 user 消息
- TUI /skill:<name> 手动载入 skill
- TUI 主题层、用户消息块与工具条目树形化
- TUI spinner、流式指示与输入框三态边框
- TUI 状态栏分区、补全弹层边框与欢迎页
- 优化 TUI 工具调用显示（参数语义摘要 + 多行结果）
- TUI 渲染 assistant 输出的 Markdown（标题/列表/代码块/引用/表格/行内样式）
- 新增 resume 子命令交互选择并恢复历史 session
- TUI 新增 /resume 命令，交互选择并恢复历史 session
- 模型规格分层解析（配置 → models.dev → 内置默认）
- **(cli)** 基于 tracing 的日志系统，默认写入 XDG state 目录，支持 --log 切换终端输出
- **(core,tools)** Agent loop 与工具执行的 tracing 插桩
- **(ai)** LLM 流式请求的 tracing 插桩
- **(ai)** Provider 请求失败自动重试（最多 3 次）
- **(core)** 上下文压缩——compaction 模块与 agent 自动/手动触发
- **(session)** Compaction entry 持久化与上下文重建
- **(cli)** /compact 命令、压缩配置与两模式集成
- 新增 flake.nix，提供 Nix 安装与运行方式
- **(tui)** 输入框支持多行输入
- **(core)** Agent prompt 支持图片附件
- **(cli)** Print 模式支持 --image 图片附件
- **(tui)** /image 命令为下一条消息附加图片
- **(tui)** Ctrl+V 粘贴剪贴板图片/文本
- **(tui)** 粘贴的图片文件路径自动转为附件
- **(core)** 引入 typestate builder 完善 agent 创建
- **(tui)** Driver 任务意外退出时在 TUI 内提示，不再静默退出
- **(tui)** Thinking 块增加标题与竖线 gutter，与工具输出视觉区分
- **(tui)** 新增 /copy 命令，复制最新一条消息到剪贴板
- **(tui)** 状态栏显示上下文用量（token 估算 / 窗口占比）
- **(tui)** 新增 /models 命令，运行时切换当前 provider 下的模型
- **(tui)** 工具调用/assistant 输出/thinking 统一采用用户消息的 gutter 块组件，以颜色区分类型
- /retry 命令重试失败的 LLM 响应
- **(tui)** System 条目与错误/流式状态行统一套用 gutter 块组件
- **(prompts)** 新增 nomic-prompts crate，支持 prompt template 发现与参数展开
- **(cli)** 接入 prompt templates（--prompt-template、config prompts、print 模式展开）
- **(tui)** /name 调用 prompt template 并展开提交，补全弹层展示模板与 argument-hint
- **(session)** 分支浏览/加载 API（list_tree、load_branch、latest_entry_id）
- **(tui)** /tree 命令浏览会话树并从非工具调用条目创建分支
- **(session)** 会话标题替代 session id 作为用户可见名称
- **(tui)** /models 切换模型后联动选择思考级别
- **(session)** Sqlite 配置表与历史回退读取 API
- **(cli)** 模型选择迁移到 sqlite 配置，支持 <provider>/<model> 与回退链
- **(tui)** /models 跨 provider 选择并落库，运行时切换 provider
- **(tools)** 新增 grep 与 find 工具（ripgrep/fd 语义）
- **(tools)** Bash 工具默认 60s 超时，超时强杀进程组并返回已收集输出
- **(tui)** 运行中放行本地 slash 命令，工具调用不再阻塞 /help、/copy 等
- **(tools)** 新增 todo_read/todo_write 工具，支持父子 todo 嵌套
- **(tui)** Thinking 消息折叠显示与 /thinking 开关
- **(tui)** 新增 /goal 模式，react loop 停止且 todo 未完成时自动追问
- **(tui)** Vim-like 交互模式骨架（ADR-0011 Phase 1）
- **(tui)** Picker 增强——过滤输入、Home/End 首尾跳与半页翻（ADR-0011 Phase 2）
- **(tui)** NORMAL : 进入命令输入（预填 /，补全自动出现）
- **(tui)** NORMAL 消息游标——[m]m/[t]t 跳转、yy/yc 复制（ADR-0011 Phase 2）
- **(tui)** NORMAL / 聊天搜索——增量命中、n/N 跳转与高亮（ADR-0011 Phase 2）
- **(tui)** VISUAL 消息选择与 y 复制（ADR-0011 Phase 3）
- **(tui)** NORMAL 草稿编辑——x/dd/dw 删除与 A/I 插入（ADR-0011 Phase 3）
- **(tui)** [**breaking**] Esc 统一为模式切换，取消运行收归 Ctrl+C
- **(release)** Git-cliff CHANGELOG 与 devenv release 脚本

### 杂项

- 初始化 rust workspace 开发环境与 CI
- 升级 similar 至 v3，精简 SSE 测试 fixture 类型，启用 rust-analyzer
- 对齐 devenv check 与 CI 检查项
- Update rust version
- 升级 devenv，适配 prek 与 git-hooks 工具链变更
- **(devenv)** 增加 ripgrep 与 fd 工具

### 重构

- **(ai)** 收敛 compaction 重建语义到 nomic-ai::compaction
- **(tools)** 截断展示措辞收进 truncate 模块
- **(tui)** 收窄 App 接口为语义级操作
- **(skills)** 收敛 skill:// 与 <active_skill> 契约到 nomic-skills 接口
- **(ai)** 流消费 fold 收进 AssistantStream，契约违约统一为 Err
- **(tui)** 消息抽象为 MessageBlock 组件，gutter 竖条与折行收拢为组件内部职责
- **(tui)** Gutter 竖条覆盖完整 MessageBlock，空行不再留白
- **(cli)** ModelResolver 多 provider 化，为跨 provider 模型选择做准备
- 移除内置 provider 与默认模型，模型选择必须显式给出
- 移除工具调用块的多余状态圆点
- **(tui)** 精简状态栏信息密度
- **(tui)** 输入框不再叠加模式提示标题
- **(tui)** 移除进入浏览模式的一次性提示
- **(tui)** 状态栏不再显示会话标题
