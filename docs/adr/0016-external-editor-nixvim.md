# ADR-0016: 嵌入编辑器（nixvim standalone 二进制，nvim --embed）

## Status

**Superseded by [ADR-0017](0017-external-editor.md)**（2026-08-11：嵌入编辑器整体移除，回到「挂起 TUI + 外部编辑器」；nvim-rs 依赖与 nixvim flake input 已移除）。

Accepted（2026-08-11 当日生效后撤销）

原状态：Accepted（2026-08-11 修订：由「挂起 TUI + 终端接管」改为「nvim --embed 嵌入 TUI」）

## Date

2026-08-11

## Context

ADR-0015（2026-08-07）以「TUI 挂起期间界面冻结、依赖外部环境、临时文件往返」
为由，把 INSERT `Ctrl+G` 从外部 `$VISUAL`/`$EDITOR` 编辑器改为内嵌 edtui。
落地后的实际使用反馈：

- **edtui 的 vim 子集对长文编辑不够用**：无寄存器、宏、文本对象、`:wq`，
  多文件/多 buffer 场景缺失；
- **生态不一致**：用户本机已维护一套完整的 nixvim 配置，内嵌编辑器是第二套、
  功能更弱的 vim 键位，配置无法复用；
- **编辑器二进制应由 nomic 的 flake 直接产出**：`nix run .#nomic` 应自带给定
  的 nixvim standalone 二进制（自包含，配置/插件内嵌），不依赖用户 PATH。

需求：**`Ctrl+G` 长文编辑 = 完整 nixvim 体验，且不挂起 TUI**。两个方案：

| 方案 | 结论 |
| --- | --- |
| 挂起 TUI + 终端接管跑外部编辑器（初版实现） | 完整 nvim，但回到 ADR-0015 的挂起问题：终端生命周期切换、编辑期间 TUI 不可见、agent 事件积压无反馈 |
| **`nvim --embed` 嵌入 TUI**（msgpack-rpc + `nvim_ui_attach`） | 完整 nvim + TUI 全程存活，采用 |

## Decision

INSERT `Ctrl+G`：nomic 把 flake 构建的 nixvim standalone 二进制以
`nvim --embed` 方式 spawn 为子进程，注册为远程 UI（msgpack-rpc over stdio），
**nvim 的界面渲染在 nomic 的 TUI 内（全屏面板），TUI 不挂起**。

### 二进制来源（flake）

> **2026-08-11 更新**：去掉 makeWrapper 与 config/env 回退链，编辑器二进制
> 唯一化——路径在 crane 构建期经环境变量 `NOMIC_EDITOR` 烘焙进二进制
> （`option_env!`），运行时只使用该 nixvim standalone 二进制；config
> `editor` 键移除（按未知键硬报错）。二进制内的路径引用使 nvim 进入
> 运行时闭包，无需 wrapper。cargo 直接构建的二进制无此路径，`Ctrl+G`
> 提示不可用而非回退 PATH 中的 nvim。

- `flake.nix` 引入 `nixvim` input（与 home-manager 配置同 rev），
  `packages.<system>.nvim = nixvim.legacyPackages.<system>.makeNixvimWithModule`
  构建自包含的 `nvim`（配置见 `nix/editor.nix`：轻量 prompt 编辑配置——
  catppuccin、系统剪贴板、打开即 INSERT 到文末、无 LSP/重型插件）；
- `default` 包即 nomic 本体，构建期注入 `NOMIC_EDITOR=<nvim>/bin/nvim`
  并烘焙进二进制；
- 编辑器二进制唯一：编译期路径，无回退。

### 嵌入协议（tui/editor.rs，防腐层）

- `EmbeddedEditor` 是唯一触点：`open(initial, editor, w, h)` / `handle_key` /
  `render` / `cursor_position` / `is_insert` / `wait_exit` / `read_result`；
  nvim-rs / rmpv 类型不外泄；
- **经典 UI 协议**（不启用 ext_linegrid，浮窗/补全菜单已画进主 grid）：
  `resize`/`clear`/`eol_clear`/`cursor_goto`/`update_fg`/`update_bg`/
  `highlight_set`/`put`/`mode_change`；每批 = `[name, params]`，params 是
  单个 Array；
- **按键全部直接转发**（crossterm KeyEvent → nvim 键位记法，如 `<C-x>`、
  `<CR>`、`<Left>`）：保存/放弃用 nvim 自身语义（`:wq`/`ZZ` 写盘退出，
  `:q!` 放弃，`:cq` 非 0 退出码视为放弃）；nomic 不拦截编辑键；
- **退出收尾**：`child.wait()` 任务回传是否正常退出 → 事件循环 `finalize`：
  正常退出读回临时文件经 `apply_editor_result` 写回（`\r\n` 归一、去尾部
  空白、空白内容保留原草稿），异常退出保留草稿；临时文件随 drop 删除；
- **尺寸同步**：终端 resize → `nvim_ui_try_resize`，`grid_resize` 事件回流
  更新屏幕状态；
- **光标形状**：跟随 nvim 模式（`mode_change` → INSERT 竖条 / 其余实心块）。

### 依赖

- `nvim-rs 0.9`（LGPL-3.0，deny.toml 白名单已加）：tokio 版 msgpack-rpc
  客户端（stdio transport + UI attach + redraw 通知分发）。

## Consequences

- `Ctrl+G` 行为：完整 nixvim 编辑器嵌入 TUI；TUI 不挂起，agent 事件在
  编辑期间继续累积、关闭后立即可见；
- 保存语义从「nomic 拦截 Esc/Ctrl+C」改为「nvim 的 `:wq`/`:q!`」——完整
  编辑器习惯，无额外按键协议；
- flake 直接产出编辑器二进制：`nix run .#nomic` 自带 `packages.<system>.nvim`，
  不依赖用户环境；cargo 直跑时回退 PATH `nvim`/config；
- 依赖 +3（nvim-rs、rmpv、async-trait 间接）；edtui 已移除（ADR-0015 撤销）；
- `future_not_send` 定点 allow 不再需要（edtui 的 Rc 已移除，nvim-rs 是 Send）。

## Non-goals

- QUEUE 模式（ADR-0012）的 oil.nvim 式就地编辑仍为 TUI 内单行编辑，
  不改为外部编辑器；
- NORMAL 模式的 vim 式浏览/导航键位不变（那是 TUI 交互而非文本编辑器）；
- 不实现 ext_linegrid/ext_multigrid 窗口协议（经典协议已覆盖补全菜单与
  浮窗，够用；分屏窗口由 nvim 布局画进主 grid）。
