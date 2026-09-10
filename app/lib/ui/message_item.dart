/// 消息项渲染：用户气泡 / assistant（markdown + thinking 折叠）/ 工具行 /
/// 系统提示。
library;

import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown/flutter_markdown.dart';
import 'package:lucide_icons/lucide_icons.dart';
import 'package:url_launcher/url_launcher.dart';

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
      ToolRunItem i => ToolRunLedger(item: i),
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
    // 用户消息是浅灰纸块（bubble token）：无边框无阴影、限宽右对齐
    return LayoutBuilder(
      builder: (context, constraints) => Align(
        alignment: Alignment.centerRight,
        child: ConstrainedBox(
          constraints: BoxConstraints(maxWidth: constraints.maxWidth * 0.75),
          child: Container(
            margin: const EdgeInsets.only(bottom: Spacing.md),
            padding: const EdgeInsets.symmetric(
              horizontal: Spacing.md,
              vertical: Spacing.sm,
            ),
            decoration: BoxDecoration(
              color: tokens.bubble,
              borderRadius: BorderRadius.circular(Radii.xl),
            ),
            child: SelectableText(
              item.text,
              style: AppText.bodySm(tokens.foreground),
            ),
          ),
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
              onTapLink: (text, href, title) {
                final uri = href == null ? null : Uri.tryParse(href);
                if (uri != null) unawaited(launchUrl(uri));
              },
              styleSheet: MarkdownStyleSheet(
                p: AppText.bodySm(tokens.foreground),
                // 标题走 DESIGN.md 字号阶梯（h1 36 / h2 30 / h3 24）
                h1: AppText.h1(tokens.foreground),
                h2: AppText.h2(tokens.foreground),
                h3: AppText.h3(tokens.foreground),
                h4: AppText.bodySm(
                  tokens.foreground,
                ).copyWith(fontWeight: FontWeight.w600),
                h5: AppText.bodySm(
                  tokens.foreground,
                ).copyWith(fontWeight: FontWeight.w600),
                h6: AppText.bodySm(
                  tokens.foreground,
                ).copyWith(fontWeight: FontWeight.w600),
                // 链接不走彩色：ink + 下划线（DESIGN.md ≤10% accent 规则）
                a: AppText.bodySm(
                  tokens.foreground,
                ).copyWith(decoration: TextDecoration.underline),
                code: AppFonts.mono(
                  fontSize: 13,
                  color: tokens.foreground,
                ).copyWith(backgroundColor: tokens.muted),
                codeblockPadding: const EdgeInsets.all(Spacing.sm),
                codeblockDecoration: BoxDecoration(
                  color: tokens.muted,
                  borderRadius: BorderRadius.circular(Radii.md),
                ),
                listBullet: AppText.bodySm(tokens.foreground),
                blockquote: AppText.bodySm(tokens.mutedForeground),
                blockquoteDecoration: BoxDecoration(
                  border: Border(
                    left: BorderSide(color: tokens.border, width: 2),
                  ),
                ),
                blockquotePadding: const EdgeInsets.symmetric(
                  horizontal: Spacing.sm,
                ),
                tableHead: AppText.ui(
                  tokens.foreground,
                ).copyWith(fontWeight: FontWeight.w600),
                tableBody: AppText.ui(tokens.foreground),
                tableBorder: TableBorder.all(color: tokens.border),
                tableCellsPadding: const EdgeInsets.symmetric(
                  horizontal: Spacing.sm,
                  vertical: 4,
                ),
                horizontalRuleDecoration: BoxDecoration(
                  border: Border(top: BorderSide(color: tokens.border)),
                ),
              ),
            ),
          if (item.streaming)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: _ShimmerText(
                text: item.streamPhase == 'thinking' ? '思考中…' : '生成中…',
              ),
            ),
          if (failed && item.errorMessage != null)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: Text(
                item.errorMessage!,
                style: AppText.ui(tokens.destructive),
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
                Text('思考过程', style: AppText.caption(tokens.mutedForeground)),
              ],
            ),
          ),
          if (_expanded)
            Padding(
              padding: const EdgeInsets.only(top: 4),
              child: SelectableText(
                widget.text,
                style: AppText.ui(tokens.mutedForeground),
              ),
            ),
        ],
      ),
    );
  }
}

/// 工具流水 ledger 行（DESIGN.md「Tool ledger」）：连续工具调用折叠为
/// 一行 muted 摘要（按类别计数：已读取 N 个文件 · 已运行 M 条命令），
/// 组内有运行中调用时 chevron 换为 spinner，有失败时追加红色计数；
/// 点击展开为逐条 ToolCard。
class ToolRunLedger extends StatefulWidget {
  const ToolRunLedger({super.key, required this.item});

  final ToolRunItem item;

  @override
  State<ToolRunLedger> createState() => _ToolRunLedgerState();
}

class _ToolRunLedgerState extends State<ToolRunLedger> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final item = widget.item;
    final (icon, opacity) = _toolStyle(item.tools.first.name);
    final errorCount = item.errorCount;
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          InkWell(
            borderRadius: BorderRadius.circular(Radii.md),
            hoverColor: tokens.secondary,
            onTap: () => setState(() => _expanded = !_expanded),
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Spacing.sm,
                vertical: 4,
              ),
              child: Row(
                children: [
                  Icon(
                    icon,
                    size: 14,
                    color: tokens.foreground.withValues(alpha: opacity),
                  ),
                  const SizedBox(width: Spacing.sm),
                  Expanded(
                    child: Text(
                      _ledgerSummary(item.tools),
                      overflow: TextOverflow.ellipsis,
                      style: AppText.ui(tokens.mutedForeground),
                    ),
                  ),
                  if (errorCount > 0) ...[
                    Icon(LucideIcons.x, size: 12, color: tokens.destructive),
                    const SizedBox(width: 4),
                    Text(
                      '$errorCount 失败',
                      style: AppText.caption(tokens.destructive),
                    ),
                    const SizedBox(width: Spacing.sm),
                  ],
                  if (item.hasRunning)
                    SizedBox(
                      width: 12,
                      height: 12,
                      child: CircularProgressIndicator(
                        strokeWidth: 2,
                        color: tokens.mutedForeground,
                      ),
                    )
                  else
                    Icon(
                      _expanded
                          ? LucideIcons.chevronDown
                          : LucideIcons.chevronRight,
                      size: 12,
                      color: tokens.mutedForeground,
                    ),
                ],
              ),
            ),
          ),
          if (_expanded)
            Padding(
              // 与摘要文本左对齐（图标 14 + 间距 8）
              padding: const EdgeInsets.only(left: Spacing.sm + 14),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  for (final tool in item.tools)
                    ToolCard(key: ValueKey(tool.toolCallId), item: tool),
                ],
              ),
            ),
        ],
      ),
    );
  }
}

/// ledger 摘要：按类别聚合计数，固定顺序拼接（读取 → 搜索 → 运行 →
/// 修改 → 其他），与「叙事日志」的账本语感一致。
String _ledgerSummary(List<ToolItem> tools) {
  var read = 0;
  var search = 0;
  var command = 0;
  var modify = 0;
  var other = 0;
  for (final tool in tools) {
    switch (tool.name) {
      case 'read':
        read++;
      case 'grep' || 'find':
        search++;
      case 'bash':
        command++;
      case 'write' || 'edit':
        modify++;
      default:
        other++;
    }
  }
  return [
    if (read > 0) '已读取 $read 个文件',
    if (search > 0) '已搜索 $search 次',
    if (command > 0) '已运行 $command 条命令',
    if (modify > 0) '已修改 $modify 个文件',
    if (other > 0) '已执行 $other 项其他操作',
  ].join(' · ');
}

/// 工具行（DESIGN.md「quiet text rows」）：图标按类别走前景不透明度阶梯
/// （execute 100 > inspect 75 > modify 60 > interact 45 > agent 35，无彩色
/// 类别色）+ 名称 + 参数摘要 + 状态；点击展开参数与结果预览。
/// 完成态为中性 check，仅失败变红。
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
    final (icon, opacity) = _toolStyle(item.name);
    final summary = _toolArgSummary(item.args);
    final expandable = item.args.isNotEmpty || item.resultPreview.isNotEmpty;
    final failed = item.status == ToolStatus.error;
    return Padding(
      padding: const EdgeInsets.only(bottom: 4),
      child: InkWell(
        borderRadius: BorderRadius.circular(Radii.md),
        hoverColor: tokens.muted,
        onTap: expandable ? () => setState(() => _expanded = !_expanded) : null,
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: Spacing.sm,
            vertical: 4,
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(
                    icon,
                    size: 14,
                    color: failed
                        ? tokens.destructive
                        : tokens.foreground.withValues(alpha: opacity),
                  ),
                  const SizedBox(width: Spacing.sm),
                  Text(
                    item.name,
                    style: AppText.ui(
                      tokens.foreground,
                    ).copyWith(fontWeight: FontWeight.w500),
                  ),
                  if (summary.isNotEmpty) ...[
                    const SizedBox(width: Spacing.sm),
                    Flexible(
                      child: Text(
                        summary,
                        overflow: TextOverflow.ellipsis,
                        style: AppFonts.mono(
                          fontSize: 12,
                          color: tokens.mutedForeground,
                        ),
                      ),
                    ),
                  ],
                  const Spacer(),
                  _ToolStatus(status: item.status),
                  if (expandable) ...[
                    const SizedBox(width: 4),
                    Icon(
                      _expanded
                          ? LucideIcons.chevronDown
                          : LucideIcons.chevronRight,
                      size: 12,
                      color: tokens.mutedForeground,
                    ),
                  ],
                ],
              ),
              if (_expanded && expandable)
                Padding(
                  // 与名称文本左对齐（图标 14 + 间距 8）
                  padding: const EdgeInsets.only(left: Spacing.sm + 14, top: 4),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      if (item.args.isNotEmpty)
                        _CopyableCodeBlock(
                          // pretty-print JSON（原 Map.toString 单行不可读）
                          text: _prettyJson(item.args),
                          color: tokens.mutedForeground,
                        ),
                      if (item.resultPreview.isNotEmpty)
                        Padding(
                          padding: const EdgeInsets.only(top: 4),
                          child: _CopyableCodeBlock(
                            text: item.resultPreview.length > 2000
                                ? '${item.resultPreview.substring(0, 2000)}…'
                                : item.resultPreview,
                            color: item.isError
                                ? tokens.destructive
                                : tokens.foreground,
                          ),
                        ),
                    ],
                  ),
                ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 展开区的等宽文本块：可选中 + 右侧复制按钮。
class _CopyableCodeBlock extends StatelessWidget {
  const _CopyableCodeBlock({required this.text, required this.color});

  final String text;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Expanded(
          child: SelectableText(
            text,
            style: AppFonts.mono(fontSize: 12, color: color),
          ),
        ),
        IconButton(
          icon: Icon(
            LucideIcons.copy,
            size: 12,
            color: tokensOf(context).mutedForeground,
          ),
          tooltip: '复制',
          visualDensity: VisualDensity.compact,
          onPressed: () => Clipboard.setData(ClipboardData(text: text)),
        ),
      ],
    );
  }
}

/// pretty-print JSON（工具参数展示用）。
String _prettyJson(Object? value) =>
    const JsonEncoder.withIndent('  ').convert(value);

/// 工具行状态：运行中 spinner；完成中性 check（muted ink）；失败红 x + 文案
///（状态不由颜色单独承载）。
class _ToolStatus extends StatelessWidget {
  const _ToolStatus({required this.status});

  final ToolStatus status;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return switch (status) {
      ToolStatus.running => SizedBox(
        width: 12,
        height: 12,
        child: CircularProgressIndicator(
          strokeWidth: 2,
          color: tokens.mutedForeground,
        ),
      ),
      ToolStatus.done => Icon(
        LucideIcons.check,
        size: 12,
        color: tokens.mutedForeground,
      ),
      ToolStatus.error => Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(LucideIcons.x, size: 12, color: tokens.destructive),
          const SizedBox(width: 4),
          Text('失败', style: AppText.caption(tokens.destructive)),
        ],
      ),
    };
  }
}

/// 工具名 →（lucide 图标，前景不透明度）；不透明度阶梯表达类别
///（DESIGN.md：execute > inspect > modify > interact > agent）。
(IconData, double) _toolStyle(String name) => switch (name) {
  'bash' => (LucideIcons.terminal, 1.0),
  'read' => (LucideIcons.fileText, 0.75),
  'grep' => (LucideIcons.search, 0.75),
  'find' => (LucideIcons.folderSearch, 0.75),
  'list_agents' => (LucideIcons.users, 0.75),
  'todo_read' => (LucideIcons.listChecks, 0.75),
  'write' => (LucideIcons.filePlus, 0.6),
  'edit' => (LucideIcons.fileEdit, 0.6),
  'todo_write' => (LucideIcons.listTodo, 0.6),
  'ask_user_question' => (LucideIcons.helpCircle, 0.45),
  'send_message' => (LucideIcons.send, 0.45),
  'goal_done' => (LucideIcons.flag, 0.45),
  'create_agent' || 'close_agent' => (LucideIcons.bot, 0.35),
  'wait_result' || 'wait_all' => (LucideIcons.hourglass, 0.35),
  _ => (LucideIcons.wrench, 0.6),
};

/// 工具参数摘要：取最具辨识度的一个标量参数（首行），无则空串。
String _toolArgSummary(Map<String, dynamic> args) {
  const keys = [
    'command',
    'path',
    'pattern',
    'question',
    'goal',
    'message',
    'content',
  ];
  for (final key in keys) {
    final value = args[key];
    if (value is String && value.isNotEmpty) return value.split('\n').first;
  }
  return '';
}

/// 流式状态微光文本（Codex 的 Thinking shimmer；灰阶基色 + ink 高亮扫过）。
class _ShimmerText extends StatefulWidget {
  const _ShimmerText({required this.text});

  final String text;

  @override
  State<_ShimmerText> createState() => _ShimmerTextState();
}

class _ShimmerTextState extends State<_ShimmerText>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1500),
  )..repeat();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, child) {
        final t = _controller.value * 1.6 - 0.3; // 扫掠范围略超文本边界
        return ShaderMask(
          blendMode: BlendMode.srcIn,
          shaderCallback: (bounds) => LinearGradient(
            colors: [
              tokens.mutedForeground,
              tokens.foreground,
              tokens.mutedForeground,
            ],
            stops: [
              (t - 0.3).clamp(0.0, 1.0),
              t.clamp(0.0, 1.0),
              (t + 0.3).clamp(0.0, 1.0),
            ],
          ).createShader(bounds),
          child: child,
        );
      },
      child: Text(widget.text, style: AppText.caption(Colors.white)),
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
        child: Text(item.text, style: AppText.caption(tokens.mutedForeground)),
      ),
    );
  }
}
