# ADR-0046: Flutter GUI 替代 Web UI —— headless 事件流服务 + Flutter 桌面应用

## Status

Accepted（supersedes ADR-0030 的前端与静态伺服部分；服务端 WebSocket 事件协议保留）

## Date

2026-02-12

## Context

ADR-0030 引入的 Web UI（React + Vite + TS + TailwindCSS + shadcn/ui，产物经
rust-embed 编译期内嵌由 `nomic --web` 伺服）存在结构性问题：

- 前端工具链（nodejs/npm）与 Rust 工程异构，`check` 变重且前后端构建强耦合
  （干净 checkout 必须先 `npm run build` 才能 `cargo build`）；
- 浏览器作为运行容器带来额外约束（Origin/CSRF 防护、标签页生命周期）；
- 需求方决定改用原生桌面体验：Flutter GUI。

已与需求方确认的决策：

- **通信方式（方案 A）**：Rust 侧保留事件流服务，Flutter 应用作为客户端经
  WebSocket 连接——不重写 agent 运行时的进程边界，既有协议（快照 + 全局事件
  总线、request_id 关联、fire-and-forget 命令、ADR-0034 steering 队列）全部
  复用；
- **移除范围**：`web/` 前端目录、rust-embed 静态伺服、nodejs 工具链彻底移除；
- **目标平台**：macOS 桌面优先（其余桌面平台不主动适配，也不设置障碍）。

## Decision

### Rust 侧：`--web` → `--serve`（headless 事件流服务）

- CLI 开关 `--web` 重命名为 `--serve`，模块 `nomic-cli::web` 重命名为
  `nomic-cli::serve`；`--port`（缺省 3333）/ `--host`（缺省 127.0.0.1）不变。
- 服务端只保留 `GET /ws` 双向事件流（`ClientEvent` / `ServerEvent` 协议不变）；
  删除 `assets.rs`（rust-embed 内嵌伺服）与 SPA fallback——serve 模式不伺服
  任何静态资源。
- Origin 校验保留（浏览器场景防护仍有意义）；Flutter 为非浏览器客户端，
  默认不发送 `Origin` 头，不受该中间件影响。
- 依赖变化：移除 `rust-embed`；`axum` / `tower-http` 保留。

### Flutter 侧：`app/` 目录（非 cargo crate）

- Flutter 桌面应用（macOS 优先），Material 3，视觉 token 继续以
  `DESIGN.md` 为单一事实来源（`app/lib/theme.dart` 同步 `@theme` 变量）。
- 连接层：`web_socket_channel` 连接 `ws://127.0.0.1:3333/ws`，指数退避重连
  （上限 15s，对齐原 web 前端 `createStreamClient` 语义）；落后收到 `refresh`
  事件时重新拉取 `get_state` 快照补齐。
- 状态管理：按 session 的 reducer（快照 + 事件增量合并），与原 web 前端
  `useChat` 同一口径。
- 范围（MVP）：流式聊天（markdown 渲染、工具调用卡片）、输入区
  （发送/停止，运行中排队）、work/session 列表与新建/恢复、提问弹层、
  模型选择。后续迭代补齐：队列编辑、设置页、mention/斜杠命令补全。

### 工程集成

- `devenv.nix`：`nodejs` 换为 `flutter`；脚本 `app-dev`（flutter run）/
  `app-check`（pub get → dart format → flutter analyze → flutter test），
  `check` 末尾追加 `app-check`。
- `flake.nix`：移除 web/dist 沙箱构建与 `.#web` 输出——Rust 包不再内嵌
  任何前端产物，干净 checkout 可直接 `cargo build`（消除 ADR-0030 引入的
  前后端构建耦合）。
- CI（ci.yml / release.yml）：移除 web 前端构建步骤；Flutter GUI 不打进
  nomic 发行包（桌面应用单独分发，后续再定）。

## Non-goals

- Flutter 应用的打包与分发（notarize/dmg）；多平台（Linux/Windows/移动端）
  适配；远程访问场景的鉴权与 TLS。
- 服务端事件协议的变更（Flutter 端按既有协议实现，协议演进另行 ADR）。

## Consequences

- ADR-0030 的「集成方式」与「前端」两节失效（本 ADR 取代）；其服务端模型
  （Runtime/SessionRuntime/事件转发/运行调度/提问注册表）与 WebSocket 协议
  继续有效。ADR-0034（steering 队列协议）不受影响。
- `check` 不再包含 npm 步骤，但新增 Flutter 步骤（pub get 需要网络，
  pub 缓存于用户目录）。
- 仓库回到单一 Rust 工具链 + 一个 Flutter 目录；前后端版本耦合解除
  （GUI 与 serve 通过 WS 协议解耦，可独立发布）。
