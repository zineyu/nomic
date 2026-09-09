/// 消息项渲染：用户气泡 / assistant（markdown + thinking 折叠）/ 工具卡片 /
/// 系统提示。
library;

import 'package:flutter/material.dart';
import 'package:flutter_markdown/flutter_markdown.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../state/chat_items.dart';
import '../theme.dart';

class MessageItemView extends StatelessWidget {
  const MessageItemView({super.key, required this.item});

  final ChatItem item;

  @override
  Widget build(BuildContext context) {
    return switch (item) {
      UserItem i => _UserBubble(item: i),
      AssistantItem i => _AssistantBlock(item: i),
      ToolItem i => ToolCard(item: i),
      SystemItem i => _SystemLine(item: i),
    };
  }
}

class _UserBubble extends StatelessWidget {
  const _UserBubble({required this.item});

  final UserItem item;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Align(
      alignment: Alignment.centerRight,
      child: Container(
        margin: const EdgeInsets.only(bottom: Spacing.md),
        padding: const EdgeInsets.symmetric(
          horizontal: Spacing.md,
          vertical: Spacing.sm,
        ),
        decoration: BoxDecoration(
          color: tokens.primary,
          borderRadius: BorderRadius.circular(Radii.xl),
        ),
        child: Text(
          item.text,
          style: TextStyle(color: tokens.primaryForeground, height: 1.5),
        ),
      ),
    );
  }
}

class _AssistantBlock extends StatelessWidget {
  const _AssistantBlock({required this.item});

  final AssistantItem item;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    if (item.isEmpty) return const SizedBox.shrink();
    final failed = item.stopReason == 'error' || item.stopReason == 'aborted';
    return Padding(
      padding: const EdgeInsets.only(bottom: Spacing.md),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (item.thinking.isNotEmpty) _ThinkingFold(text: item.thinking),
          if (item.text.isNotEmpty)
            MarkdownBody(
              data: item.text,
              selectable: true,
              styleSheet: MarkdownStyleSheet(
                p: TextStyle(
                  fontSize: 14,
                  height: 1.5,
                  color: tokens.foreground,
                ),
                code: TextStyle(
                  fontSize: 13,
                  backgroundColor: tokens.muted,
                  color: tokens.foreground,
                ),
                codeblockDecoration: BoxDecoration(
                  color: tokens.muted,
                  borderRadius: BorderRadius.circular(Radii.md),
                ),
              ),
            ),
          if (item.streaming)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                item.streamPhase == 'thinking' ? '思考中…' : '生成中…',
                style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
              ),
            ),
          if (failed && item.errorMessage != null)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                item.errorMessage!,
                style: TextStyle(fontSize: 13, color: tokens.destructive),
              ),
            ),
        ],
      ),
    );
  }
}

class _ThinkingFold extends StatefulWidget {
  const _ThinkingFold({required this.text});

  final String text;

  @override
  State<_ThinkingFold> createState() => _ThinkingFoldState();
}

class _ThinkingFoldState extends State<_ThinkingFold> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: Spacing.sm),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          InkWell(
            onTap: () => setState(() => _expanded = !_expanded),
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Icon(
                  _expanded
                      ? LucideIcons.chevronDown
                      : LucideIcons.chevronRight,
                  size: 12,
                  color: tokens.mutedForeground,
                ),
                const SizedBox(width: 4),
                Text(
                  '思考过程',
                  style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
                ),
              ],
            ),
          ),
          if (_expanded)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                widget.text,
                style: TextStyle(
                  fontSize: 13,
                  height: 1.5,
                  color: tokens.mutedForeground,
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// 工具卡片：名称 + 状态；点击展开参数与结果预览。
class ToolCard extends StatefulWidget {
  const ToolCard({super.key, required this.item});

  final ToolItem item;

  @override
  State<ToolCard> createState() => _ToolCardState();
}

class _ToolCardState extends State<ToolCard> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final item = widget.item;
    final statusColor = switch (item.status) {
      ToolStatus.running => tokens.mutedForeground,
      ToolStatus.done => tokens.foreground.withValues(alpha: 0.6),
      ToolStatus.error => tokens.destructive,
    };
    return Padding(
      padding: const EdgeInsets.only(bottom: Spacing.sm),
      child: Material(
        color: tokens.card,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(Radii.lg),
          side: BorderSide(color: tokens.border),
        ),
        child: InkWell(
          borderRadius: BorderRadius.circular(Radii.lg),
          onTap: () => setState(() => _expanded = !_expanded),
          child: Padding(
            padding: const EdgeInsets.all(Spacing.sm + 4),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  children: [
                    Icon(LucideIcons.wrench, size: 14, color: statusColor),
                    const SizedBox(width: Spacing.sm),
                    Text(
                      item.name,
                      style: TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w500,
                        color: tokens.foreground,
                      ),
                    ),
                    const SizedBox(width: Spacing.sm),
                    Text(switch (item.status) {
                      ToolStatus.running => '执行中',
                      ToolStatus.done => '完成',
                      ToolStatus.error => '失败',
                    }, style: TextStyle(fontSize: 12, color: statusColor)),
                    const Spacer(),
                    Icon(
                      _expanded
                          ? LucideIcons.chevronDown
                          : LucideIcons.chevronRight,
                      size: 12,
                      color: tokens.mutedForeground,
                    ),
                  ],
                ),
                if (_expanded) ...[
                  if (item.args.isNotEmpty)
                    Padding(
                      padding: const EdgeInsets.only(top: Spacing.sm),
                      child: SelectableText(
                        item.args.toString(),
                        style: TextStyle(
                          fontSize: 12,
                          fontFamily: 'monospace',
                          color: tokens.mutedForeground,
                        ),
                      ),
                    ),
                  if (item.resultPreview.isNotEmpty)
                    Padding(
                      padding: const EdgeInsets.only(top: Spacing.sm),
                      child: SelectableText(
                        item.resultPreview.length > 2000
                            ? '${item.resultPreview.substring(0, 2000)}…'
                            : item.resultPreview,
                        style: TextStyle(
                          fontSize: 12,
                          fontFamily: 'monospace',
                          color: item.isError
                              ? tokens.destructive
                              : tokens.foreground,
                        ),
                      ),
                    ),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _SystemLine extends StatelessWidget {
  const _SystemLine({required this.item});

  final SystemItem item;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Padding(
      padding: const EdgeInsets.only(bottom: Spacing.md),
      child: Center(
        child: Text(
          item.text,
          style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
        ),
      ),
    );
  }
}
