# ADR-0029: `ask_user_question` 交互工具（用户提问）

- 状态：已接受
- 日期：2026-08-16

## 背景

agent 运行中经常需要用户介入：决策（选哪个方案）、偏好（语言/风格）、
或只有用户才知道的信息（密钥、账号、真实意图）。此前 agent 只能通过
输出文字「建议用户输入什么」，或依赖统一消息队列（ADR-0014）等用户
主动输入——模型无法**主动**请求输入并阻塞等待回答。

需求：新增 `ask_user_question` 工具，支持单选 / 多选 / 填空三种问题
形式；单选/多选问题**始终默认增加一个允许用户自己填写的选项**，避免
固定选项框死用户。工具执行期间 agent 阻塞等待，回答作为工具结果回喂
模型。

## 决策

### 工具契约（`nomic-tools`）

- 参数：`question`（问题文本）+ `kind`（`single_choice` / `multiple_choice`
  / `fill_in`，默认单选）+ `options`（候选选项；单选/多选必填，填空忽略）。
- 单选/多选问题在校验后**无条件**在选项末尾追加自定义选项
  （[`CUSTOM_OPTION`] = `✏️ 其他（自定义填写）`，已在选项中则不重复），
  选项由工具侧保证——宿主无需自行补。
- 回答 `AskUserAnswer { answers: Vec<String>, custom: Option<String> }`：
  `answers` 为最终答案文本（单选 1 个、多选若干、填空 1 个，自定义填写
  时含自定义文本），`custom` 标记其中的自由输入部分（单选/多选未用
  自定义选项时为 `None`，填空时等于唯一答案）。
- 执行模式声明 `ExecutionMode::Sequential`：交互阻塞本轮，且保证同批次
  至多一个提问在途（批内工具串行执行），提问通道不会积压第二个未答问题。
- 校验错误（空问题、单选/多选缺选项）按既有契约转为错误工具结果回喂
  模型，由模型自我修正。

### 与宿主解耦：`QuestionSink` trait

工具不直接碰终端：通过 `QuestionSink::ask(question, cancel)` 把问题交给
宿主并阻塞等待回答。宿主实现两个：

- **TUI**（`tui/ask.rs`）：一条独立 mpsc 通道直连 agent 任务与事件循环
  ——工具把问题连同 `oneshot::Sender<AskUserAnswer>` 推入通道，事件循环
  在 `next_wake` 新增的 `Wake::UserQuestion` 分支收到后打开提问弹层
  （模态覆盖层，`Mode::Question` 派生态），用户作答后经 oneshot 回传。
  回答通道暂存在 `Driver.pending_question`（状态层不持有外部资源，
  沿用 Effect 接线模式）。
- **print**（`print.rs`）：问题与编号选项渲染到 stderr（stdout 保持流式
  输出纯净），回答从 stdin 读取；编号选择，非编号文本视为自定义答案，
  自定义选项（末位）选中后二次输入文本。

### 取消与生命周期

- 弹层 Esc：关闭弹层并丢弃回答通道 → 工具侧收到通道关闭转为错误结果
  回喂模型（模型可重试或改道）。
- 运行中断（NORMAL `q` / Ctrl+C）：`Effect::Cancel` 取消令牌并丢弃回答
  通道，`TuiQuestionSink::ask` 的 `tokio::select!` 中取消分支先就绪返回
  错误，工具不挂起。
- 运行结束（含失败）：`App::finish_run` 兜底关闭弹层；`submit_prompt`
  丢弃上一轮残留通道（防御）。
- 同一时刻至多一个提问在途（Sequential 保证），弹层状态是单值
  `App.question: Option<Question>`。

### 弹层交互

- 选项列表：↑/↓（或 j/k）循环移动；单选 Enter 提交游标项；多选空格
  勾选/取消、Enter 提交（空勾选提示留在列表）；自定义选项先进入文本
  输入（单选输入后直接提交，多选勾选后回列表继续勾选，Enter 再提交）。
- 自定义输入：普通文本编辑（复用 `Input` 缓冲，单行），Enter 提交、
  Esc 放弃回列表（填空无列表，Esc 直接取消提问）。
- 答案组装（含多选勾选 + 自定义文本合并）收在 `app/question.rs`，
  提交经 `Effect::SubmitQuestionAnswer` 交给事件循环回传。

## 后果

- 模型获得主动请求用户输入的能力；交互体验（弹层、键位、自定义填写）
  与既有 Picker/Help 模态一致。
- `nomic-tools` 的 `default_tools` / `default_tools_with_skills` 签名新增
  `question_sink: Arc<dyn QuestionSink>` 参数（pre-1.0，调用点仅
  print / TUI 两处，随本 ADR 一并更新）。
- 提问/回答作为普通工具结果经既有事件管线进入聊天区与 session 落库
  （`ToolResultMessage`），会话可回放。
- 交互成本显式化：agent 每次提问都会阻塞并打断用户，模型应在确需用户
  输入时才调用（工具描述中已强调）。
- 未来若需要「预设答案自动作答」（测试/无人值守），可在宿主侧实现
  一个新的 `QuestionSink`，工具层零改动。
