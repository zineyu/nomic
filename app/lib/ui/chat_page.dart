/// 聊天页：消息流（含工具行）+ 队列区 + Working 状态行 + Codex 式 composer。
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../protocol/models.dart';
import '../state/chat_items.dart';
import '../theme.dart';
import 'input_bar.dart';
import 'message_item.dart';
import 'question_panel.dart';
import 'sidebar.dart';

class ChatPage extends StatefulWidget {
  const ChatPage({super.key, required this.controller});

  final AppController controller;

  @override
  State<ChatPage> createState() => _ChatPageState();
}

class _ChatPageState extends State<ChatPage> {
  final _scrollController = ScrollController();

  /// 上次滚动检查时的列表签名（条数 + 末条内容长度）；
  /// 流式增长不改条数，靠末条长度变化识别。
  int _lastSignature = 0;

  /// 本轮运行开始时间（Working 状态行计时用；空闲时为 null）。
  DateTime? _runStartedAt;

  /// 用户是否停留在底部附近（决定「回到底部」浮钮是否显示）。
  bool _atBottom = true;

  @override
  void initState() {
    super.initState();
    _scrollController.addListener(_onScroll);
  }

  @override
  void dispose() {
    _scrollController.dispose();
    super.dispose();
  }

  void _onScroll() {
    if (!_scrollController.hasClients) return;
    final position = _scrollController.position;
    final atBottom = position.maxScrollExtent - position.pixels < 200;
    if (atBottom != _atBottom) setState(() => _atBottom = atBottom);
  }

  void _scrollToBottom() {
    if (!_scrollController.hasClients) return;
    _scrollController.animateTo(
      _scrollController.position.maxScrollExtent,
      duration: AppMotion.normal,
      curve: AppMotion.curve,
    );
  }

  /// 焦点处的可编辑文本是否处于 IME 组词中（composing region 有效）。
  /// 组词期间 Enter / Esc 等按键属于输入法，页面级快捷键不得消费。
  static bool _primaryFocusComposing() {
    final focusContext = FocusManager.instance.primaryFocus?.context;
    if (focusContext == null) return false;
    final editable = focusContext.findAncestorStateOfType<EditableTextState>();
    final composing = editable?.currentTextEditingValue.composing;
    return composing != null && composing.isValid && !composing.isCollapsed;
  }

  /// 新消息到达或流式增长时贴底滚动（用户在底部时）。
  void _maybeScrollToBottom() {
    final items = widget.controller.items;
    final signature = Object.hash(items.length, _tailContentLength(items));
    if (signature == _lastSignature) return;
    _lastSignature = signature;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!_scrollController.hasClients) return;
      final position = _scrollController.position;
      if (position.maxScrollExtent - position.pixels < 400) {
        _scrollController.jumpTo(position.maxScrollExtent);
      }
    });
  }

  /// 末条项的内容长度（流式 delta 只改内容不改条数，用于滚动签名）。
  static int _tailContentLength(List<ChatItem> items) {
    if (items.isEmpty) return 0;
    return switch (items.last) {
      UserItem i => i.text.length,
      AssistantItem i => i.text.length + i.thinking.length,
      ToolItem i => i.resultPreview.length,
      ExecutionItem i => i.steps.fold(
        0,
        (sum, s) =>
            sum +
            switch (s) {
              ToolItem t => t.resultPreview.length,
              AssistantItem a => a.thinking.length,
              _ => 0,
            },
      ),
      SystemItem i => i.text.length,
    };
  }

  /// 「回答」区块标注：每个用户请求段内，最后一段执行过程之后的首条
  /// assistant 正文即最终回答（视觉主体）。无执行过程的段不标注。
  static Set<String> _answerItemIds(List<ChatItem> entries) {
    final answers = <String>{};
    var seenExecution = false;
    ChatItem? candidate;
    void flush() {
      if (candidate != null) answers.add(candidate!.id);
      candidate = null;
      seenExecution = false;
    }

    for (final entry in entries) {
      switch (entry) {
        case UserItem _:
          flush();
        case ExecutionItem _ || ToolItem _:
          seenExecution = true;
          candidate = null;
        case AssistantItem a:
          if (seenExecution && candidate == null && a.text.isNotEmpty) {
            candidate = a;
          }
        default:
          break;
      }
    }
    flush();
    return answers;
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    _maybeScrollToBottom();
    // 执行段在 assistant 正文（text）出现时折叠为执行过程卡片；尚无
    // 文本收尾的尾部段保持平铺（组 id 锚定首个调用，展开态不丢）
    final entries = groupExecutionSteps(controller.items);
    final answerIds = _answerItemIds(entries);

    // 运行边沿记录开始时间（Working 状态行计时）
    if (controller.running && _runStartedAt == null) {
      _runStartedAt = DateTime.now();
    } else if (!controller.running) {
      _runStartedAt = null;
    }

    return CallbackShortcuts(
      bindings: {
        // Esc 中断当前运行（排队消息保留）；IME 组词期间 Esc 属于输入法
        // （取消组词），不得消费
        const SingleActivator(LogicalKeyboardKey.escape): () {
          if (controller.running && !_primaryFocusComposing()) {
            controller.cancel();
          }
        },
      },
      child: Focus(
        autofocus: true,
        child: Column(
          children: [
            _ContextBar(controller: controller),
            if (controller.error != null) _ErrorBanner(controller: controller),
            if (controller.goal != null) _GoalBanner(goal: controller.goal!),
            Expanded(
              child: controller.items.isEmpty && !controller.running
                  ? const _EmptySessionHint()
                  : Stack(
                      children: [
                        Center(
                          child: ConstrainedBox(
                            constraints: const BoxConstraints(
                              maxWidth: maxPageWidth,
                            ),
                            // 桌面端常显滚动条（长会话定位用）
                            child: Scrollbar(
                              controller: _scrollController,
                              thumbVisibility: true,
                              child: ListView.builder(
                                controller: _scrollController,
                                // 底部 140px 安全区：滚动到底时内容不被
                                // composer / 浮动按钮遮挡
                                padding: const EdgeInsets.fromLTRB(
                                  Spacing.lg,
                                  Spacing.lg,
                                  Spacing.lg,
                                  140,
                                ),
                                itemCount: entries.length,
                                // ValueKey 锚定 item.id：插入新项时折叠/展开状态
                                //（_StepRow / ExecutionCard）跟随数据而非位置
                                itemBuilder: (context, index) =>
                                    MessageItemView(
                                      key: ValueKey(entries[index].id),
                                      item: entries[index],
                                      isAnswer: answerIds.contains(
                                        entries[index].id,
                                      ),
                                    ),
                              ),
                            ),
                          ),
                        ),
                        // 「回到底部」浮钮：输入框上方右侧，不压正文
                        if (!_atBottom)
                          Positioned(
                            right: Spacing.lg,
                            bottom: Spacing.sm,
                            child: _ScrollToBottomButton(
                              onTap: _scrollToBottom,
                            ),
                          ),
                      ],
                    ),
            ),
            // steering 队列区（服务端权威：queue_changed / 快照驱动）
            if (controller.queue.isNotEmpty) _QueueBar(controller: controller),
            if (controller.running && _runStartedAt != null)
              _WorkingLine(startedAt: _runStartedAt!),
            // 提问面板（内嵌不阻塞；ValueKey 锚定 id，新提问重置表单态）
            if (controller.question != null)
              Center(
                child: ConstrainedBox(
                  constraints: const BoxConstraints(maxWidth: maxPageWidth),
                  child: Padding(
                    padding: const EdgeInsets.fromLTRB(
                      Spacing.md,
                      0,
                      Spacing.md,
                      Spacing.sm,
                    ),
                    child: QuestionPanel(
                      key: ValueKey(controller.question!.id),
                      controller: controller,
                      question: controller.question!,
                    ),
                  ),
                ),
              ),
            Center(
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxWidth: maxPageWidth),
                // composer：距左右 24px、距底部 16px（规格 6.5/6.10）
                child: Padding(
                  padding: const EdgeInsets.fromLTRB(
                    Spacing.lg,
                    Spacing.sm,
                    Spacing.lg,
                    Spacing.md,
                  ),
                  child: controller.readOnly
                      ? _ReadOnlyBar(controller: controller)
                      : InputBar(controller: controller),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// 顶部任务上下文栏：project / 任务标题 / 运行状态 + 更多操作
///（重命名 / 删除）。状态同时由图标与文本承载，不只依赖颜色。
class _ContextBar extends StatelessWidget {
  const _ContextBar({required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    WorkSummary? work;
    for (final w in controller.works) {
      if (w.mainSessionId == controller.sessionId) {
        work = w;
        break;
      }
    }
    final project = controller.project;
    final running = controller.running;
    final (statusIcon, statusColor, statusText) = controller.readOnly
        ? (LucideIcons.eye, tokens.secondary, '只读回溯')
        : running
        ? (null, tokens.business, '运行中')
        : controller.items.isEmpty
        ? (LucideIcons.circle, tokens.tertiary, '空闲')
        : (LucideIcons.check, tokens.success, '已完成');
    return Container(
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: tokens.border)),
      ),
      padding: const EdgeInsets.symmetric(horizontal: Spacing.lg, vertical: 10),
      child: Row(
        children: [
          if (project != null) ...[
            Flexible(
              flex: 0,
              child: Text(
                _basename(project),
                overflow: TextOverflow.ellipsis,
                style: AppText.xs(tokens.secondary),
              ),
            ),
            Text(' / ', style: AppText.xs(tokens.tertiary)),
          ],
          Flexible(
            child: Text(
              work?.displayTitle ?? '会话',
              overflow: TextOverflow.ellipsis,
              style: AppText.xs(
                tokens.foreground,
              ).copyWith(fontWeight: FontWeight.w500),
            ),
          ),
          const SizedBox(width: Spacing.sm),
          if (running)
            SizedBox(
              width: 12,
              height: 12,
              child: CircularProgressIndicator(
                strokeWidth: 2,
                color: tokens.business,
              ),
            )
          else if (statusIcon != null)
            Icon(statusIcon, size: 12, color: statusColor),
          const SizedBox(width: 4),
          Text(statusText, style: AppText.xxs(statusColor)),
          if (work != null) ...[
            const SizedBox(width: Spacing.sm),
            IconButton(
              icon: Icon(
                LucideIcons.moreHorizontal,
                size: 16,
                color: tokens.secondary,
              ),
              tooltip: '更多操作',
              visualDensity: VisualDensity.compact,
              onPressed: () => _showWorkMenu(context, work!),
            ),
          ],
        ],
      ),
    );
  }

  void _showWorkMenu(BuildContext context, WorkSummary work) {
    final tokens = tokensOf(context);
    final button = context.findRenderObject()! as RenderBox;
    final overlay =
        Overlay.of(context).context.findRenderObject()! as RenderBox;
    showMenu<String>(
      context: context,
      position: RelativeRect.fromRect(
        Rect.fromPoints(
          button.localToGlobal(
            button.size.bottomRight(Offset.zero),
            ancestor: overlay,
          ),
          button.localToGlobal(
            button.size.bottomRight(Offset.zero),
            ancestor: overlay,
          ),
        ),
        Offset.zero & overlay.size,
      ),
      items: [
        PopupMenuItem(
          value: 'rename',
          height: 32,
          child: Text('重命名', style: AppText.xs(tokens.foreground)),
        ),
        PopupMenuItem(
          value: 'delete',
          height: 32,
          child: Text('删除', style: AppText.xs(tokens.error)),
        ),
      ],
    ).then((value) {
      if (!context.mounted) return;
      if (value == 'rename') showRenameWorkDialog(context, controller, work);
      if (value == 'delete') showDeleteWorkDialog(context, controller, work);
    });
  }
}

/// project 路径末段（上下文栏显示用）。
String _basename(String path) {
  final segments = path.split(RegExp(r'[/\\]')).where((s) => s.isNotEmpty);
  return segments.isEmpty ? path : segments.last;
}

/// 只读回溯栏（子 agent 会话）：说明文案 + 返回父会话入口。
class _ReadOnlyBar extends StatelessWidget {
  const _ReadOnlyBar({required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final parentId = controller.parentSessionId;
    return Row(
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        Text('子 agent 会话（只读回溯）', style: AppText.xs(tokens.secondary)),
        if (parentId != null) ...[
          Text(' · ', style: AppText.xs(tokens.secondary)),
          InkWell(
            borderRadius: BorderRadius.circular(Radii.sm),
            onTap: () => controller.openSession(parentId),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(LucideIcons.arrowLeft, size: 12, color: tokens.foreground),
                const SizedBox(width: 4),
                Text(
                  '返回父会话',
                  style: AppText.xs(
                    tokens.foreground,
                  ).copyWith(decoration: TextDecoration.underline),
                ),
              ],
            ),
          ),
        ],
      ],
    );
  }
}

class _ErrorBanner extends StatelessWidget {
  const _ErrorBanner({required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Material(
      color: tokens.error.withValues(alpha: 0.08),
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Spacing.md,
          vertical: Spacing.sm,
        ),
        child: Row(
          children: [
            Icon(LucideIcons.alertCircle, size: 14, color: tokens.error),
            const SizedBox(width: Spacing.sm),
            Expanded(
              child: Text(controller.error!, style: AppText.xs(tokens.error)),
            ),
            IconButton(
              icon: const Icon(LucideIcons.x, size: 14),
              visualDensity: VisualDensity.compact,
              onPressed: controller.clearError,
            ),
          ],
        ),
      ),
    );
  }
}

class _GoalBanner extends StatelessWidget {
  const _GoalBanner({required this.goal});

  final String goal;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(
        horizontal: Spacing.md,
        vertical: Spacing.sm,
      ),
      color: tokens.tip,
      child: Text(
        '目标：$goal',
        overflow: TextOverflow.ellipsis,
        style: AppText.xxs(tokens.foreground),
      ),
    );
  }
}

class _QueueBar extends StatelessWidget {
  const _QueueBar({required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final queue = controller.queue;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.symmetric(
        horizontal: Spacing.md,
        vertical: Spacing.sm,
      ),
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: tokens.border)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('队列（${queue.length}）', style: AppText.xxs(tokens.secondary)),
          for (var i = 0; i < queue.length; i++)
            _QueueEntryRow(
              entry: queue[i],
              isFirst: i == 0,
              isLast: i == queue.length - 1,
              controller: controller,
            ),
        ],
      ),
    );
  }
}

/// 队列条目行：hover 露出上移 / 下移 / 删除（服务端权威，操作后
/// `queue_changed` 广播回填）。
class _QueueEntryRow extends StatefulWidget {
  const _QueueEntryRow({
    required this.entry,
    required this.isFirst,
    required this.isLast,
    required this.controller,
  });

  final QueueEntry entry;
  final bool isFirst;
  final bool isLast;
  final AppController controller;

  @override
  State<_QueueEntryRow> createState() => _QueueEntryRowState();
}

class _QueueEntryRowState extends State<_QueueEntryRow> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final entry = widget.entry;
    return MouseRegion(
      onEnter: (_) => setState(() => _hovered = true),
      onExit: (_) => setState(() => _hovered = false),
      child: Padding(
        padding: const EdgeInsets.only(top: 4),
        child: Row(
          children: [
            Expanded(
              child: Text(
                entry.text,
                overflow: TextOverflow.ellipsis,
                style: AppText.xs(tokens.foreground),
              ),
            ),
            if (_hovered) ...[
              if (!widget.isFirst)
                _QueueAction(
                  icon: LucideIcons.arrowUp,
                  tooltip: '上移',
                  onTap: () =>
                      widget.controller.moveQueueEntry(entry.id, up: true),
                ),
              if (!widget.isLast)
                _QueueAction(
                  icon: LucideIcons.arrowDown,
                  tooltip: '下移',
                  onTap: () =>
                      widget.controller.moveQueueEntry(entry.id, up: false),
                ),
              _QueueAction(
                icon: LucideIcons.x,
                tooltip: '移出队列',
                onTap: () => widget.controller.removeQueueEntry(entry.id),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _QueueAction extends StatelessWidget {
  const _QueueAction({
    required this.icon,
    required this.tooltip,
    required this.onTap,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Tooltip(
      message: tooltip,
      child: InkWell(
        borderRadius: BorderRadius.circular(Radii.sm),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(4),
          child: Icon(icon, size: 12, color: tokens.secondary),
        ),
      ),
    );
  }
}

/// 空会话引导（消息流为零且未运行时展示）。
class _EmptySessionHint extends StatelessWidget {
  const _EmptySessionHint();

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Text('输入消息开始对话', style: AppText.xs(tokens.secondary)),
          const SizedBox(height: Spacing.sm),
          Text(
            'Enter 发送 · Shift+Enter 换行 · Esc 中断',
            style: AppText.xxs(tokens.secondary),
          ),
        ],
      ),
    );
  }
}

/// 「回到底部」浮钮：用户上翻时出现在消息流底部中央（overlay 定位用，
/// 扁平 card + hairline border，不用阴影）。
class _ScrollToBottomButton extends StatelessWidget {
  const _ScrollToBottomButton({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Tooltip(
      message: '回到底部',
      child: Material(
        color: tokens.card,
        shape: CircleBorder(side: BorderSide(color: tokens.border)),
        child: InkWell(
          customBorder: const CircleBorder(),
          onTap: onTap,
          child: SizedBox(
            width: 32,
            height: 32,
            child: Icon(
              LucideIcons.arrowDown,
              size: 16,
              color: tokens.foreground,
            ),
          ),
        ),
      ),
    );
  }
}

/// Working 状态行（Codex 同款 `Working (12s · esc to interrupt)`）：
/// composer 上方一行，ink spinner + 已用秒数 + 中断提示；模型与 token
/// 信息已并入 composer 底部控制行。
class _WorkingLine extends StatefulWidget {
  const _WorkingLine({required this.startedAt});

  final DateTime startedAt;

  @override
  State<_WorkingLine> createState() => _WorkingLineState();
}

class _WorkingLineState extends State<_WorkingLine> {
  Timer? _timer;

  @override
  void initState() {
    super.initState();
    _timer = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  @override
  void dispose() {
    _timer?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final seconds = DateTime.now().difference(widget.startedAt).inSeconds;
    final elapsed = seconds >= 60
        ? '${seconds ~/ 60}m ${seconds % 60}s'
        : '${seconds}s';
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: maxPageWidth),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: Spacing.md),
          child: Row(
            children: [
              SizedBox(
                width: 12,
                height: 12,
                child: CircularProgressIndicator(
                  strokeWidth: 2,
                  color: tokens.primary,
                ),
              ),
              const SizedBox(width: Spacing.sm),
              Text(
                'Working（$elapsed · esc 中断）',
                style: AppText.xxs(tokens.secondary),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
