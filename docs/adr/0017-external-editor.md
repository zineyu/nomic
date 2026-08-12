# ADR-0017: 外部编辑器（$VISUAL/$EDITOR）替代嵌入编辑器

## Status

Accepted（2026-08-11）

Supersedes [ADR-0016](0016-external-editor-nixvim.md)（嵌入编辑器，nvim --embed）。

## Date

2026-08-11

## Context

INSERT `Ctrl+G` 长文编辑的演进：外部系统编辑器（初版）→ 内嵌 edtui
（ADR-0015）→ `nvim --embed` 嵌入 TUI（ADR-0016）。ADR-0016 落地后重新评估，
决定撤销嵌入路线、回到外部编辑器：

- **嵌入实现的维护面过大**：`tui/editor.rs` 要自行实现 nvim UI 协议
  （redraw 解析、网格/高亮/宽字符渲染、按键记法映射、退出收尾），
  约 700 行与 agent 主业无关的终端 UI 代码；
- **依赖与构建链重**：nvim-rs（LGPL-3.0，deny.toml 需单独放行）+
  nixvim flake input（构建 nomic 需拖入整套 nixpkgs-unstable 与 nixvim
  闭包），只为一个低频编辑入口；
- **用户环境已有完整编辑器**：$VISUAL/$EDITOR 是 unix 惯例（与 git 同一
  口径），用户自己的 nvim 配置/插件直接可用，无需 nomic 再烘焙一份
  nixvim standalone。

ADR-0015 当初反对挂起方案的顾虑（TUI 冻结、依赖外部环境）依然存在，
但权衡后认为：低频入口的简洁性优先；编辑期间 agent 事件在 channel 积压、
恢复后照常处理，可接受。

## Decision

INSERT `Ctrl+G`：挂起 TUI，把当前草稿写入临时文件（`.md` 后缀），用
`$VISUAL` → `$EDITOR` → `vi`（与 git 同一口径）打开编辑，保存退出后
整体写回输入框。

- **编辑器解析**：环境变量优先，命令经 `sh -c` 执行以支持带参数形式
  （如 `code --wait`）；退出码非 0（如 vim `:cq`）视为放弃，原草稿保留；
  内容为空时同样保留原草稿（保存空文件是常见误操作）。
- **终端生命周期**：复用启动路径的 `enter_tui_terminal` /
  `leave_tui_terminal`；编辑器运行期间事件循环同步挂起（tty 已交出，
  不应重绘），退出后清屏全量重绘并按当前模式恢复光标形状。
  `spawn_blocking` 执行，不占 runtime worker。
- **写回归一化**（`App::apply_editor_result`）：`\r\n` 归一、去尾部空白，
  光标移到末尾。
- **删除**：`tui/editor.rs`（`EmbeddedEditor`、nvim UI 协议渲染）、
  nvim-rs 依赖（deny.toml 的 LGPL-3.0 放行与 RUSTSEC-2026-0058 忽略
  一并移除）、flake 的 nixvim input 与 `packages.<system>.nvim`、
  编译期 `NOMIC_EDITOR` 烘焙；`Mode::Editor` 派生态与相关渲染/键位
  路由随之移除。

## Consequences

- 依赖 −3（nvim-rs、rmpv 等传递依赖），nomic-cli 不再直接使用
  async-trait；deny.toml 许可证白名单回到纯 permissive。
- flake 构建不再拖入 nixvim/nixpkgs-unstable 闭包，构建链与运行时闭包
  都显著变小；`nix run .#nomic` 不再自带编辑器。
- `Ctrl+G` 体验取决于用户环境：$VISUAL/$EDITOR 未配置时退化到 `vi`；
  编辑期间 TUI 挂起不可见（事件积压，恢复后立即可见）。
- 终端挂起/恢复是一组易错操作，集中在 `edit_input_in_editor` 一处，
  与启动路径共用 enter/leave 函数保证口径一致。

## Non-goals

- QUEUE 模式（ADR-0012/0014）的就地编辑仍为 TUI 内单行编辑，
  不改为外部编辑器；
- 不提供 config `editor` 键（环境变量已覆盖；未知键继续硬报错）。
