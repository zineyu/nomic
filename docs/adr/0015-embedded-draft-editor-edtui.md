# ADR-0015: 内嵌草稿编辑器（edtui）替代外部系统编辑器

## Status

**Superseded by [ADR-0016](0016-external-editor-nixvim.md)**（2026-08-11：改用 nixvim standalone 二进制，以 `nvim --embed` 嵌入 TUI；edtui 依赖已移除）。

Accepted（2026-08-07 至 2026-08-11 生效期）

## Date

2026-08-07

## Context

INSERT 下 `Ctrl+G` 原实现为挂起 TUI、在临时文件上运行
`$VISUAL`/`$EDITOR`（缺省 `vi`）、退出后整体写回输入框（commit
`8d9ef00f`）。实际使用与维护中的问题：

- **TUI 挂起期间界面冻结**：编辑器运行期间聊天区不重绘，agent
  事件在 channel 积压；tty 交出/恢复、清屏重绘、光标形状还原是
  一组易错的终端生命周期操作；
- **依赖外部环境**：`$EDITOR` 未配置或指向 GUI 编辑器（无 `--wait`）
  时体验退化；退出码语义（非 0 即放弃）把编辑器习惯泄露进 TUI；
- **临时文件往返**：为一次草稿编辑引入文件系统副作用。

需求：**长文/多行 prompt 编辑不离开 TUI**——编辑器内嵌，TUI 持续
存活（agent 事件继续累积），保存/放弃语义明确。

候选方案评估：

| 方案 | 结论 |
| --- | --- |
| 自实现全屏编辑模式 | 零依赖，但 motion/yank/undo 等要重造，维护面大 |
| vimltui 0.2 | vim 保真度最高（text objects、registers、`:wq`），但 3 个月新库、单人、下载量三位数，不符引入审查标准 |
| **edtui 0.11** | 成熟活跃、ratatui 生态对齐（只依赖 ratatui-core/widgets，lock 已有）；vim 风格键位对 prompt 编辑足够；重依赖（syntect/arboard）可裁 |

## Decision

采用 **edtui**（`default-features = false`，裁掉 arboard/syntect/
mouse，只要最基础 vim 编辑），以薄封装接入：

### 防腐层（`tui/editor.rs`）

- `DraftEditor` 是唯一触点，协议为：`new(initial)` /
  `handle_key(KeyEvent) -> DraftAction::{Continue, Save, Cancel}` /
  `render` / `cursor_position` / `is_insert`。edtui 类型不外泄到
  `app`/`ui`/`mod`，未来替换实现只动这一个文件。
- 主题底色跟随终端默认（不用 edtui 默认的黑底白字），与 nomic
  主题共存。

### 按键协议

- 打开即 **INSERT**、光标在文末（用户正在起草，`Ctrl+G` 的意图是
  继续编辑长文；先切模式再定位，Insert 下光标才可停在行尾后一位）。
- INSERT 下 `Esc` 回 NORMAL（edtui vim 键位自带）；**NORMAL 下
  `Esc` 保存并关闭**——edtui 无 `:wq` 语义，且 Esc 在其 NORMAL
  键位中无绑定，复用为保存退出，与 QUEUE 就地编辑 Enter/Esc
  保存的口径一致。
- 任意时刻 `Ctrl+C` 放弃修改：编辑器持有草稿副本，放弃即丢弃，
  输入缓冲原样保留。
- 写回归一化沿用原外部编辑器语义（`apply_editor_result`）：
  `\r\n` 归一、去尾部空白、空白内容保留原草稿。

### 模式接入

- `Mode::Editor` 为**派生态**（`draft_editor.is_some()`，与 Picker
  同一模式），不入 `App::mode` 字段；底层模式保持 INSERT，关闭
  即回原处。
- 编辑器打开时**接管键位**：事件循环把原始 `KeyEvent` 直接转发给
  `DraftEditor`（edtui 需要完整按键信息，语义 `Key` 枚举粒度不够），
  不经 `map_key` 映射。
- 光标形状跟随 edtui 自身模式（INSERT 竖条 / 其余实心块），
  而非 nomic 模式。
- 渲染为全屏接管：聊天区被遮盖期间 agent 事件继续累积，关闭后
  下一帧照常恢复——不再有挂起/恢复终端的生命周期问题。

### Send 约束与定点 allow

edtui 的 `EditorState` 内含 `Rc<RefCell<dyn ClipboardTrait>>`
（内部剪贴板，`set_clipboard` 也只能替换内容、Rc 壳仍在），使
`App` 及所有持其跨 await 的 future 非 Send，触发 workspace deny
的 nursery lint `future_not_send`。`unsafe impl Send` 被
`forbid(unsafe_code)` 排除。

处理：tui 模块级 `#[allow(clippy::future_not_send)]` + 传播出口
（`sessions::resume`、`dispatch`）两处定点 allow，均注明理由。
安全性论证：main 以 `block_on` 在主线程驱动整个 TUI，future 不会
跨线程迁移；`tokio::spawn` 的任务（driver、剪贴板读写）均不接触
`App`。若未来要把 TUI future 放入 `tokio::spawn`，需先解决 edtui
的 Rc（上游改为 `Arc<Mutex>` 或自维护 fork）。

### 删除

- `edit_input_in_editor` / `run_external_editor` /
  `Effect::OpenEditor` 及临时文件逻辑全部删除；`$VISUAL`/`$EDITOR`
  不再被读取。需要完整编辑器能力的用户场景（宏、多寄存器）由
  内嵌编辑器的基础 vim 键位覆盖到什么程度算什么程度，不再保留
  外部出口（如确有诉求可后续以独立功能重议）。

## Consequences

- 新增依赖仅 3 个 crate（edtui、edtui-jagged、enum_dispatch）；
  syntect/arboard 未引入（与 nomic 自有 clipboard 模块不重复）。
- `Ctrl+G` 行为变更：不再打开用户配置的系统编辑器，键位为
  edtui vim 子集；README 与 `/help` 文案已同步。
- TUI 全程不挂起，长文编辑期间可看到聊天区更新（关闭编辑器后
  立即呈现积压内容）。
- vimltui 若日后成熟（维护记录、下载量达标）且需要完整 vim
  体验，可在防腐层内替换实现，协议不变。
