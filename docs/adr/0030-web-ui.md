# ADR-0030: Web UI — 内置 HTTP 服务 + React 前端

## Status

Accepted

## Date

2026-08-16

## Context

nomic 目前有 print（`-p`，管道可用）与交互 TUI（ratatui）两种运行模式。需求方
提出增加 **Web UI**，技术栈限定为：React + Vite + TypeScript + TailwindCSS +
shadcn/ui + Vitest + Storybook。

已与需求方确认的决策：

- 集成方式：**`nomic --web` 内置服务器**——Rust 侧新增 web 模式，复用
  `bootstrap` 运行时；axum 提供 REST + SSE 流式接口，并直接伺服构建好的前端
  产物；开发期前端另起 Vite dev server，把 `/api` 代理到 Rust 服务。
- 范围（MVP）：流式聊天（markdown 渲染、thinking 折叠、工具调用卡片）、输入区
  （发送/停止，运行中排队）、会话列表/新建/恢复、模型选择器、状态栏（模型、
  上下文 token、运行状态）、`ask_user_question` 提问弹层。

## Decision

### 入口与模式分发

`main.rs` 新增 `--web` 开关（与 `-p` 互斥）与 `--port`（缺省 3333）、
`--host`（缺省 `127.0.0.1`）。有 `--web` 时进入 `web::run`；`bootstrap` 与
print/TUI 完全复用。前端产物编译期内嵌进二进制（见下），不再有 `--web-dist`
/ `NOMIC_WEB_DIST` 磁盘目录伺服。

### 服务端模型（crates/nomic-cli/src/web/）

- **单一共享状态 `Shared`**：`AgentHandle`（ADR-0022 actor）、`SessionRecorder`、
  `ModelResolver`、broadcast 事件通道（`tokio::sync::broadcast`）、提问应答表
  （question id → oneshot）、运行队列与运行中取消令牌。
- **事件转发**：agent 构建时的 `AgentEvent` 接收端由专属任务持续消费——
  每事件先经 `SessionRecorder::record` 落库（与 print/TUI 同一 seam），再转发到
  broadcast。SSE 客户端订阅即收到同一事件流。
- **运行调度**：`POST /api/prompt` 提交 prompt。运行中提交进入队列（对齐 TUI 的
  统一消息队列语义：当前轮完成后按序续跑）；空闲时启动 runner 任务逐条消费。
  每条 prompt 附带独立 `CancellationToken`，`POST /api/cancel` 取消当前轮。
- **提问**：`ask_user_question` 经 `WebQuestionSink`（`QuestionSink` 实现）把
  问题 id + 内容发到 SSE（`question` 事件），应答表登记 oneshot；前端弹层回答后
  `POST /api/question/{id}` 经 oneshot 回填，`cancel` 触发时询问立即返回错误。

### API（REST + SSE）

- `GET /api/state`：当前快照——消息历史、模型、思考级别、上下文 token 估算、
  运行状态、队列长度、当前 session id。前端挂载时先取快照再连 SSE。
- `GET /api/stream`：SSE。事件负载为 JSON，`type` 区分：`agent`（原样
  `AgentEvent`）、`question`、`question_cancelled`、`run_started`、`run_finished`、
  `error`。断线重连由前端负责（fetch + ReadableStream 解析，失败退避重连）。
- `GET /api/sessions` / `POST /api/sessions`（新建）/ `POST /api/sessions/resume`
  （按 id 恢复）：复用 `SessionStore`；恢复时 `AgentHandle::restore_messages` +
  `SessionRecorder::switch`。
- `GET /api/models` / `POST /api/models`：候选来自 `ModelResolver::candidates`；
  切换复用 TUI 同一口径（跨 provider 时 `api_key` 分层、`set_model` + `set_provider`），
  并 `set_config(CONFIG_KEY_MODEL, spec)` 落库（与 TUI 一致）。
- `POST /api/question/{id}`：提交提问回答。

### 序列化

`nomic-core` 的 `AgentEvent`、`ToolResult`、`ToolUpdate` 补 `Serialize` 派生
（`nomic-ai` 消息模型本就全量 serde；工具结果此前仅内部使用，无序列化需求，
现为 SSE 通道补齐）。`AgentEvent` 直接作为 SSE 负载，前端按既有事件协议重建
消息流（delta 累积、`MessageEnd` 定稿替换）。

### 安全

- 缺省只绑定 `127.0.0.1`（`--host` 显式覆盖）。
- 状态变更请求（POST）校验 `Origin` 头：非空且与服务器 host 不符时拒绝
  （DNS rebinding / 跨站请求防护）。本服务能执行 bash，**不开放 CORS**——
  开发期前端经 Vite 代理 `/api` 同源访问，无需跨域。

### 前端（web/ 目录，非 cargo crate）

- Vite + React + TS；TailwindCSS v4（`@tailwindcss/vite`）；shadcn/ui 组件
  （`components.json` + `src/components/ui/`）；Vitest + Testing Library 单测；
  Storybook（`@storybook/react-vite`）组件开发。
- `web/src/lib/api.ts`：REST 客户端 + SSE 客户端（fetch stream 解析）。
- 视图：聊天区（消息流/工具卡片/thinking 折叠）、输入区、侧栏（会话/模型）、
  提问弹层、状态栏。状态由 `useChat` hook 集中管理（快照 + 事件增量合并）。
- 产物：`web/dist`（`vite build`），经 `rust-embed` 编译期内嵌进二进制
  （`crates/nomic-cli/src/web/assets.rs`），`--web` 直接伺服内嵌资源（SPA
  回退 `index.html`）；开发期 `npm run dev` + Vite 代理 `/api` 到 `nomic --web`。

### 工程集成

- `devenv.nix` 新增 `nodejs`；脚本 `web-dev` / `web-build` / `web-test` /
  `web-check`；`check` 末尾追加 web 检查（install → lint → typecheck → build →
  vitest run）。
- `.gitignore` 排除 `web/node_modules`、`web/dist`、`web/storybook-static`、
  `web/coverage`；`_typos.toml` 排除上述目录。
- flake 打包（Nix）：crane 源过滤放行 `web/dist`，`nix build` 前需先在
  `web/` 下 `npm run build`（内嵌目录缺失时编译报错）；发行包单文件，
  不携带独立前端目录。

## Non-goals

- 队列的图形化编辑（TUI QUEUE 模式的 web 版）、会话树浏览与分支创建。
- 图片附件上传、剪贴板粘贴（TUI 已有，web 版后续迭代）。
- 多用户 / 鉴权 / TLS；本服务定位为本地单用户工具。

## Consequences

- workspace 新增 `axum`、`tower-http` 依赖（仅 nomic-cli 使用）。
- nomic-cli 新增 `rust-embed` 依赖（仅 nomic-cli 使用），`cargo build` 时
  `web/dist` 必须已存在（`check`/`web-build` 先构建前端）；干净 checkout 直接
  `cargo build` 会因内嵌目录缺失而编译失败，属于有意的前后端同版本耦合。
- `nomic-core` 三个类型补 `Serialize` 派生（向后兼容，无行为变化）。
- `check` 变重（多一步 npm 安装 + 前端构建/测试）；CI 与本地仍共用同一 `check`。
- Web 模式与 TUI/print 共享 bootstrap、事件落库 seam、模型切换口径，语义不漂移；
  前端状态机按既有事件协议重建消息流，后续 TUI 新增事件类型时 web 端同步演进。
