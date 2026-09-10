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
      duration: const Duration(milliseconds: 250),
      curve: Curves.easeOut,
    );
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
      SystemItem i => i.text.length,
    };
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    _maybeScrollToBottom();

    // 运行边沿记录开始时间（Working 状态行计时）
    if (controller.running && _runStartedAt == null) {
      _runStartedAt = DateTime.now();
    } else if (!controller.running) {
      _runStartedAt = null;
    }

    return CallbackShortcuts(
      bindings: {
        // Codex 同款：Esc 中断当前运行（排队消息保留）
        const SingleActivator(LogicalKeyboardKey.escape): () {
          if (controller.running) controller.cancel();
        },
      },
      child: Focus(
        autofocus: true,
        child: Column(
          children: [
            if (controller.error != null) _ErrorBanner(controller: controller),
            if (controller.goal != null) _GoalBanner(goal: controller.goal!),
            Expanded(
              child: Stack(
                children: [
                  Center(
                    child: ConstrainedBox(
                      constraints: const BoxConstraints(maxWidth: maxPageWidth),
                      child: ListView.builder(
                        controller: _scrollController,
                        padding: const EdgeInsets.symmetric(
                          horizontal: Spacing.lg,
                          vertical: Spacing.lg,
                        ),
                        itemCount: controller.items.length,
                        // ValueKey 锚定 item.id：插入新项时折叠/展开状态
                        //（_ThinkingFold / ToolCard）跟随数据而非位置
                        itemBuilder: (context, index) => MessageItemView(
                          key: ValueKey(controller.items[index].id),
                          item: controller.items[index],
                        ),
                      ),
                    ),
                  ),
                  if (!_atBottom)
                    Positioned(
                      left: 0,
                      right: 0,
                      bottom: Spacing.sm,
                      child: Center(
                        child: _ScrollToBottomButton(onTap: _scrollToBottom),
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
                child: Padding(
                  padding: const EdgeInsets.all(Spacing.md),
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
        Text('子 agent 会话（只读回溯）', style: AppText.ui(tokens.mutedForeground)),
        if (parentId != null) ...[
          Text(' · ', style: AppText.ui(tokens.mutedForeground)),
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
                  style: AppText.ui(
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
      color: tokens.destructive.withValues(alpha: 0.08),
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Spacing.md,
          vertical: Spacing.sm,
        ),
        child: Row(
          children: [
            Icon(LucideIcons.alertCircle, size: 14, color: tokens.destructive),
            const SizedBox(width: Spacing.sm),
            Expanded(
              child: Text(
                controller.error!,
                style: AppText.ui(tokens.destructive),
              ),
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
      color: tokens.muted,
      child: Text(
        '目标：$goal',
        overflow: TextOverflow.ellipsis,
        style: AppText.caption(tokens.foreground),
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
          Text(
            '队列（${queue.length}）',
            style: AppText.caption(tokens.mutedForeground),
          ),
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
                style: AppText.ui(tokens.foreground),
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
          child: Icon(icon, size: 12, color: tokens.mutedForeground),
        ),
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
    final elapsed = DateTime.now().difference(widget.startedAt).inSeconds;
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
                'Working（${elapsed}s · esc 中断）',
                style: AppText.caption(tokens.mutedForeground),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
