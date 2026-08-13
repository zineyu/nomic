# ADR-0019: `?` 键位帮助弹层

## Status

Accepted（2026-08-13）

扩展 [ADR-0011](0011-vim-like-interaction.md) 的键位表（新增 NORMAL `?`）。
[ADR-0018](0018-browse-default-and-edtui-input.md) 的模式改名落地后，
本 ADR 的 NORMAL 即其 BROWSE，语义不变。

## Date

2026-08-13

## Context

键位表随模式增多（INSERT/NORMAL/SEARCH/VISUAL/QUEUE/PICKER）持续膨胀，
但应用内可查的入口只有三处，且都有缺口：

- 状态栏提示刻意精简（只留当前模式核心键）；
- 欢迎页只在空会话可见，对话开始后即被聊天内容顶掉；
- `/help` 列出的是 slash 命令，键位只在末尾附带两段说明。

用户忘记某个键位时（尤其低频键：`]t`、`yc`、`Q`），只能退出 TUI 翻
README。需要一个随时可达、完整的应用内键位速查。

## Decision

NORMAL 下 `?` 打开键位帮助弹层：居中面板列出全模式键位分组表，
`j`/`k`、`Ctrl+D/U`、`PgUp/PgDn`、`gg`/`G` 滚动，`Esc`/`q`/`?` 关闭。

- **派生态，与 Picker 同构**：帮助不是一等模式，而是叠加在 NORMAL
  之上的只读覆盖层（`help_scroll: Option<u16>` 为 `Some` 即打开，
  `App::mode()` 派生 `Mode::Help`）；关闭时底层 mode 字段未动，
  天然回到打开前的 NORMAL，无需「返回模式」簿记。
- **只绑 NORMAL**：ADR-0011 的原则是「每个按键在当前模式只有一个
  语义」——INSERT 下 `?` 是正常文本输入，SEARCH/VISUAL/QUEUE 各有
  主职，均不抢占；从 INSERT 出发的路径是 `Esc` → `?`，与所有
  浏览类键位同一入口。
- **渲染为模态覆盖层**：绘制前清空状态栏以上的内容区再居中面板，
  避免被覆盖的输入框/聊天区在面板边缘留下边框残片；内容超出面板
  高度时滚动，上限由渲染回写钳制（与聊天区 `clamp_scroll` 同口径）。
- **可发现性**：NORMAL 状态栏提示加 `? 帮助`、欢迎页浏览行加
  `? 帮助`、`/help` 输出末尾指向 `?`；键位表内容与 README
  「TUI 键位」保持一致。

## Consequences

- 任何模式调整键位时需同步四处：实现、`HELP_GROUPS`（ui.rs）、
  README 键位表、必要时状态栏提示。`HELP_GROUPS` 是应用内唯一来源，
  README 是其文档投影。
- 帮助打开期间不阻塞运行中的 agent（只读层，不触碰 running 态）；
  `Ctrl+C` 在帮助内保持取消运行/退出语义，与全局口径一致。
- INSERT 下输入 `?` 行为不变（纯文本字符）。
