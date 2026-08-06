# Changelog

所有值得关注的变更记录于本文件。

格式基于 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)，
由 [git-cliff](https://git-cliff.org) 从 conventional commits 自动生成。

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

