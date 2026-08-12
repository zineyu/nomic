# ADR-0018: 输入与过程显示分离——BROWSE 默认态与输入框内嵌 edtui

## Status

Accepted（2026-08-13）

修订 [ADR-0011](0011-vim-like-interaction.md)（默认模式反转、Esc 语义、
草稿编辑归属）；复活 [ADR-0015](0015-embedded-draft-editor-edtui.md) 的
edtui 防腐层（从「Ctrl+G 全屏编辑器」改为「输入框本体」）；
[ADR-0017](0017-external-editor.md) 的外部编辑器入口保留。

## Date

2026-08-13

## Context

ADR-0011 把隐式模式显式化后，默认仍落 INSERT：启动即可打字，NORMAL
是「按 Esc 才进入的浏览态」。实际使用中暴露的问题：

- **输入与过程显示未分离**。agent 运行中聊天区持续滚动输出（过程显示），
  而键盘焦点永远在输入框：浏览输出时按到的可打印字符会污染草稿；
  反之，想打字又必须确认当前没有弹层/运行态等隐式上下文。「看」与
  「写」共用一个焦点，靠用户自己小心。
- **输入框编辑能力两极分化**。INSERT 只有 readline 系词级编辑；真正的
  vim 编辑键（`x`/`dd`/`dw`）却挂在 NORMAL——而 NORMAL 的主职是浏览
  聊天区，草稿编辑键与消息游标键混在同一键位表里。
- **ADR-0015 的 edtui 集成被放在错误的位置**。作为 `Ctrl+G` 全屏编辑器
  它是低频入口，维护面相对收益过大（ADR-0017 因此移除）；但它的 vim
  键位恰好是「输入框内部编辑」的正确答案——需求方明确要求输入框内部
  采用 vim mode 键位。

需求方确认的交互契约：**只有明确切换到输入模式才进行输入**；输入框
内部是 vim 双模；`Esc` 逐层退回 `insert → normal → 退出输入模式`。

## Decision

### 模式架构

```
启动 ──► BROWSE（默认；过程显示聚焦，可打印字符一律不进输入框）
            │  i（光标原位）/ a（光标到文末）——仅有的两个入口
            ▼
         INPUT（输入框 = 常驻 edtui 编辑器，进入即 INSERT 子态）
            Esc：INSERT → NORMAL（edtui 自带）→ 退出 INPUT 回 BROWSE
```

- **顶层模式改名**：`Mode::Normal` → `Mode::Browse`、`Mode::Insert` →
  `Mode::Input`。NORMAL/INSERT 两个词降级为 INPUT 内部 edtui 子态的
  徽标，避免一词两义。
- **默认 BROWSE**：这是过程查看工具的第一态；打字必须先 `i`/`a`。
  与 ADR-0011「INSERT-first」的取舍相反：当时的理由是减少启动摩擦，
  现在的判断是误输入的代价高于多按一次 `i`。
- **BROWSE 键位**：保留原 NORMAL 的全部浏览能力（`j/k`、
  `Ctrl+D/U`、`gg`/`G`、`[m]m`/`[t]t`、`yy`/`yc`/`Y`、`V`、`/`、`n/N`、
  `Q`）。移除 `Enter/A/I/:` 回输入与 `x`/`dd`/`dw` 草稿编辑——草稿编辑
  收编进 INPUT-NORMAL（由 edtui 提供更完整的 vim 键位），slash 命令
  统一 `i` 后输入。
- **BROWSE `Enter` 的唯一保留语义**：空闲 + 队列暂停（异常结束残留）
  时取出队首发送下一条（沿用 ADR-0014 的恢复路径，非输入入口）。

### INPUT 键位协议（app 层拦截优先，其余转发 edtui）

| 子态 | 键 | 行为 |
|---|---|---|
| INSERT | 字符/移动/删除 | 转发 edtui（vim insert 键位） |
| INSERT | `Enter` | 换行（edtui 原生行为） |
| 任意子态 | `Shift+Enter` | **提交**（运行中排入统一队列，ADR-0014 不变） |
| INSERT | `Tab` | 补全确认（`/` 触发的补全弹层保留） |
| INSERT | `↑`/`↓` | 补全开 → 移动候选；否则转发 edtui |
| INSERT | `Esc` | 补全开 → 关弹层；否则 → NORMAL 子态 |
| NORMAL | vim 键位全集 | 转发 edtui（`h/l/w/b`、`x`、`dd`、`u` 等） |
| NORMAL | `Esc` | 退出 INPUT → BROWSE，草稿保留 |
| INSERT | `Ctrl+G` | 外部编辑器（ADR-0017），写回 edtui 缓冲 |
| 任意子态 | `Ctrl+V` / 粘贴 | 事件循环拦截，写入 edtui 缓冲 |
| 任意子态 | `Ctrl+C` | 全局取消运行/退出（拦截，不给 edtui） |

要点：

- **`Enter` 与 `Shift+Enter` 的职责对调**（相对旧 INSERT）：Enter 回归
  vim insert 的换行本义，提交收敛到 `Shift+Enter`（kitty 键盘增强协议
  区分两者，启动路径已启用）。edtui 因此零按键冲突——所有拦截键都是
  edtui 未绑定或语义单一的键。
- **`Ctrl+D` 让位 edtui**（半页滚动/dedent），退出统一 `Ctrl+C`，
  全局「取消/退出」一键一义（延续 ADR-0011 修订的原则）。
- **补全弹层逻辑不变**，只是数据源从 `String` 缓冲换成 edtui 缓冲：
  每次转发按键后读取 `editor.text()` 重算候选；接受候选 =
  `set_text("/候选")` + 光标到末尾。仅 INSERT 子态且光标在文末时弹出。
- **BROWSE 下粘贴**（`Ctrl+V`/bracketed paste）视为编辑意图：进入
  INPUT（INSERT 子态）并粘贴，与旧「NORMAL 粘贴回 INSERT」口径一致。

### 草稿归属与渲染

- 草稿的唯一持有者是**常驻 `DraftEditor`**（ADR-0015 防腐层复活并
  扩展协议：`text()`/`set_text()`/`insert_text()`/`is_insert()`/
  `enter_insert_at_cursor()`/`enter_insert_at_end()`/`cursor_at_end()`/
  `render()`/`cursor_screen_position()`；edtui 类型不外泄）。不再有
  「打开/关闭编辑器」的生命周期，INPUT 只是「把按键转发给它」。
- BROWSE 下输入框暗色显示草稿纯文本（非焦点，现状行为保留）；
  INPUT 下草稿区改用 edtui `EditorView` 渲染（附件行、队列区、
  SEARCH 复用输入框均不变）。徽标/光标形状随 edtui 子态：
  INSERT 竖条 + ` INSERT ` 徽标，NORMAL 实心块 + ` NORMAL ` 徽标；
  BROWSE 实心块 + ` BROWSE ` 徽标。
- QUEUE 就地编辑**不上 edtui**：改用独立的轻量文本缓冲（`TextBuf`，
  从现有 `String`+`cursor` 编辑助手提取），交互不变。控制范围，
  避免一次改动两处编辑路径。
- `Mode::Input` 不入派生态判断：`mode()` 仍只在 picker 打开时派生
  Picker；INPUT 的子态经 `editor.is_insert()` 查询，不单列模式变体。

### Send 约束

edtui `EditorState` 的 `Rc<RefCell<dyn ClipboardTrait>>` 问题与
ADR-0015 相同：恢复 tui 模块级 `#[allow(clippy::future_not_send)]` 与
传播出口（`sessions::resume`、`main::dispatch`）的定点 allow，安全性
论证不变（`block_on` 主线程驱动，future 不跨线程迁移）。

## Consequences

- 启动即 BROWSE：启动后第一次输入需 `i`/`a`；README、欢迎页、`/help`
  键位表同步改写。
- 提交键从 `Enter` 变为 `Shift+Enter`：肌肉记忆破坏是本次最大的
  行为变更；不支持 kitty 键盘增强协议的终端无法区分 `Shift+Enter`，
  退化为换行（提交需经 slash 命令或直接依赖终端支持）——README 注明。
- 运行中排队多一步 `i`（先进入输入态再打字 `Shift+Enter`），需求方
  已确认接受。
- 旧 NORMAL 的 `x`/`dd`/`dw` 草稿编辑被 edtui 的全集取代，状态层删除
  约 200 行自实现编辑代码（词级移动/删除等），由 edtui 接管。
- picker 从 INPUT 提交 slash 命令打开，关闭后回到 INPUT（草稿已随
  提交清空），语义与旧 INSERT 一致。
- ADR-0011 中与本 ADR 冲突的部分（默认 INSERT、Esc 栈底回 INSERT、
  NORMAL 草稿编辑键）以本 ADR 为准；其余（模式一等状态、显式徽标、
  一键一义）继续有效。

## Non-goals

- QUEUE 就地编辑、SEARCH 输入框改为 edtui（均为单行轻量输入，维持
  现有实现）。
- edtui 之外的完整 vim 模拟（寄存器跨编辑器互通、宏、`:wq` 语义）。
- 可配置键位映射。
- print 模式与 core crate 的任何改动。
