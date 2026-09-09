/// 聊天页：消息流（含工具卡片）+ 队列区 + 状态栏 + 输入区。
library;

import 'package:flutter/material.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../theme.dart';
import 'input_bar.dart';
import 'message_item.dart';
import 'model_picker.dart';
import 'question_sheet.dart';

class ChatPage extends StatefulWidget {
  const ChatPage({super.key, required this.controller});

  final AppController controller;

  @override
  State<ChatPage> createState() => _ChatPageState();
}

class _ChatPageState extends State<ChatPage> {
  final _scrollController = ScrollController();
  int _lastItemCount = 0;

  @override
  void dispose() {
    _scrollController.dispose();
    super.dispose();
  }

  /// 新消息到达或流式增长时贴底滚动（用户在底部时）。
  void _maybeScrollToBottom() {
    if (widget.controller.items.length == _lastItemCount) return;
    _lastItemCount = widget.controller.items.length;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!_scrollController.hasClients) return;
      final position = _scrollController.position;
      if (position.maxScrollExtent - position.pixels < 400) {
        _scrollController.jumpTo(position.maxScrollExtent);
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final controller = widget.controller;
    final tokens = tokensOf(context);
    _maybeScrollToBottom();

    // 提问弹层（单选/多选/填空 + 自定义填写）
    final question = controller.question;
    if (question != null) {
      WidgetsBinding.instance.addPostFrameCallback((_) {
        if (!context.mounted) return;
        QuestionSheet.maybeShow(context, controller, question);
      });
    }

    return Column(
      children: [
        if (controller.error != null) _ErrorBanner(controller: controller),
        if (controller.goal != null) _GoalBanner(goal: controller.goal!),
        Expanded(
          child: Center(
            child: ConstrainedBox(
              constraints: const BoxConstraints(maxWidth: maxPageWidth),
              child: ListView.builder(
                controller: _scrollController,
                padding: const EdgeInsets.symmetric(
                  horizontal: Spacing.lg,
                  vertical: Spacing.lg,
                ),
                itemCount: controller.items.length,
                itemBuilder: (context, index) =>
                    MessageItemView(item: controller.items[index]),
              ),
            ),
          ),
        ),
        // steering 队列区（服务端权威：queue_changed / 快照驱动）
        if (controller.queue.isNotEmpty) _QueueBar(controller: controller),
        _StatusBar(controller: controller),
        Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: maxPageWidth),
            child: Padding(
              padding: const EdgeInsets.all(Spacing.md),
              child: controller.readOnly
                  ? Text(
                      '子 agent 会话（只读回溯）',
                      style: TextStyle(color: tokens.mutedForeground),
                    )
                  : InputBar(controller: controller),
            ),
          ),
        ),
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
                style: TextStyle(fontSize: 13, color: tokens.destructive),
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
        style: TextStyle(fontSize: 12, color: tokens.foreground),
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
            '队列（${controller.queue.length}）',
            style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
          ),
          for (final entry in controller.queue)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                entry.text,
                overflow: TextOverflow.ellipsis,
                style: TextStyle(fontSize: 13, color: tokens.foreground),
              ),
            ),
        ],
      ),
    );
  }
}

class _StatusBar extends StatelessWidget {
  const _StatusBar({required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final c = controller;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: Spacing.md, vertical: 4),
      decoration: BoxDecoration(
        border: Border(top: BorderSide(color: tokens.border)),
      ),
      child: Row(
        children: [
          // 模型选择器（点击弹候选列表）
          TextButton.icon(
            icon: const Icon(LucideIcons.cpu, size: 14),
            label: Text(
              c.model.name.isEmpty ? '选择模型' : c.model.name,
              style: const TextStyle(fontSize: 12),
            ),
            style: TextButton.styleFrom(
              foregroundColor: tokens.mutedForeground,
              visualDensity: VisualDensity.compact,
            ),
            onPressed: () => ModelPicker.show(context, c),
          ),
          const SizedBox(width: Spacing.md),
          Text(
            '${c.contextTokens} tokens',
            style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
          ),
          const Spacer(),
          if (c.running)
            Row(
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
                  '运行中',
                  style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
                ),
              ],
            ),
        ],
      ),
    );
  }
}
