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
  const MessageItemView({super.key, required this.item, this.isAnswer = false});

  final ChatItem item;

  /// 是否为「回答」区块（用户请求段内、最后一段执行过程之后的首条
  /// assistant 正文；由 chat_page 标注）。回答块前出现分隔与标签，
  /// 并使用 prose 排版档（15px / 1.7）。
  final bool isAnswer;

  @override
  Widget build(BuildContext context) {
    return switch (item) {
      UserItem i => _UserBubble(item: i),
      AssistantItem i => _AssistantBlock(item: i, isAnswer: isAnswer),
      ToolItem i => ToolCard(item: i),
      ExecutionItem i => ExecutionCard(item: i),
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
  const _AssistantBlock({required this.item, this.isAnswer = false});

  final AssistantItem item;
  final bool isAnswer;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    if (item.isEmpty) return const SizedBox.shrink();
    final failed = item.stopReason == 'error' || item.stopReason == 'aborted';
    // 回答块用 prose（15px / 1.7）；中间过程叙述用 bodySm（弱化）
    final textStyle = isAnswer
        ? AppText.prose(tokens.foreground)
        : AppText.bodySm(tokens.foreground);
    return Padding(
      padding: const EdgeInsets.only(bottom: Spacing.md),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 「回答」区块标记：标签 + 分隔线，与执行过程明确分层
          if (isAnswer)
            Padding(
              padding: const EdgeInsets.only(bottom: Spacing.sm),
              child: Row(
                children: [
                  Text(
                    '回答',
                    style: AppText.caption(
                      tokens.mutedForeground,
                    ).copyWith(fontWeight: FontWeight.w600),
                  ),
                  const SizedBox(width: Spacing.sm),
                  Expanded(child: Container(height: 1, color: tokens.border)),
                ],
              ),
            ),
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
                p: textStyle,
                // 标题走 DESIGN.md 字号阶梯（h1 24 / h2 20 / h3 18）
                h1: AppText.h1(tokens.foreground),
                h2: AppText.h2(tokens.foreground),
                h3: AppText.h3(tokens.foreground),
                h4: AppText.body(
                  tokens.foreground,
                ).copyWith(fontWeight: FontWeight.w600),
                h5: AppText.body(
                  tokens.foreground,
                ).copyWith(fontWeight: FontWeight.w600),
                h6: AppText.body(
                  tokens.foreground,
                ).copyWith(fontWeight: FontWeight.w600),
                // 链接不走彩色：ink + 下划线
                a: textStyle.copyWith(decoration: TextDecoration.underline),
                // 行内 code chip：等宽 + 浅灰底（crate 名 / 版本号 / 路径）
                code: AppFonts.mono(
                  fontSize: 13,
                  color: tokens.foreground,
                ).copyWith(backgroundColor: tokens.secondary),
                codeblockPadding: const EdgeInsets.all(Spacing.md),
                codeblockDecoration: BoxDecoration(
                  color: tokens.secondary,
                  borderRadius: BorderRadius.circular(Radii.lg),
                ),
                listBullet: textStyle,
                blockquote: textStyle.copyWith(color: tokens.mutedForeground),
                blockquoteDecoration: BoxDecoration(
                  border: Border(
                    left: BorderSide(color: tokens.borderStrong, width: 2),
                  ),
                ),
                blockquotePadding: const EdgeInsets.symmetric(
                  horizontal: Spacing.sm,
                ),
                // 表格：14px 正文、strong 细边框、斑马纹底（表头用字重
                // 区分）；Intrinsic 列宽 + 窄窗口横向滚动，代码文本不被
                // 压缩截断
                tableHead: AppText.bodySm(
                  tokens.foreground,
                ).copyWith(fontWeight: FontWeight.w600),
                tableBody: AppText.bodySm(tokens.foreground),
                tableBorder: TableBorder.all(color: tokens.borderStrong),
                tableColumnWidth: const IntrinsicColumnWidth(),
                tableScrollbarThumbVisibility: true,
                tableCellsDecoration: BoxDecoration(color: tokens.secondary),
                tableCellsPadding: const EdgeInsets.symmetric(
                  horizontal: Spacing.sm,
                  vertical: 6,
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
                Text('思考', style: AppText.caption(tokens.mutedForeground)),
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

/// 执行过程卡片（DESIGN.md「Execution card」）：连续工具调用与纯思考段
/// 折叠为一张卡片——默认一行摘要（执行过程 · N 步 · 搜索 X 次 ·
/// 命令 Y 条 · 用时 Zs），展开为逐步时间线（图标 + 名称 + 输入摘要 +
/// 状态 + 耗时 + 详情）。执行过程是弱化的次要信息，最终回答才是主体。
class ExecutionCard extends StatefulWidget {
  const ExecutionCard({super.key, required this.item});

  final ExecutionItem item;

  @override
  State<ExecutionCard> createState() => _ExecutionCardState();
}

class _ExecutionCardState extends State<ExecutionCard> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final item = widget.item;
    final errorCount = item.errorCount;
    return Container(
      margin: const EdgeInsets.only(bottom: Spacing.md),
      decoration: BoxDecoration(
        color: tokens.card,
        borderRadius: BorderRadius.circular(Radii.xl),
        border: Border.all(color: tokens.border),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 摘要头（整行可点，键盘可聚焦展开）
          InkWell(
            borderRadius: BorderRadius.circular(Radii.xl),
            hoverColor: tokens.secondary,
            onTap: () => setState(() => _expanded = !_expanded),
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Spacing.md,
                vertical: 10,
              ),
              child: Row(
                children: [
                  Icon(
                    LucideIcons.layers,
                    size: 14,
                    color: tokens.mutedForeground,
                  ),
                  const SizedBox(width: Spacing.sm),
                  Expanded(
                    child: Text(
                      _executionSummary(item),
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
                        color: tokens.accent,
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
          if (_expanded) ...[
            Divider(height: 1, color: tokens.border),
            Padding(
              padding: const EdgeInsets.symmetric(vertical: 4),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  for (final step in item.steps)
                    switch (step) {
                      ToolItem t => _ToolStepRow(
                        key: ValueKey(t.toolCallId),
                        item: t,
                      ),
                      AssistantItem a => _ThinkingStepRow(
                        key: ValueKey(a.id),
                        item: a,
                      ),
                      _ => const SizedBox.shrink(),
                    },
                ],
              ),
            ),
          ],
        ],
      ),
    );
  }
}

/// 执行过程摘要：「执行过程 · N 步 · 读取 X 次 · 搜索 X 次 · 命令 X 条 ·
/// 用时 Zs」，零计数类别不出现；历史快照无时间戳时省略用时。
String _executionSummary(ExecutionItem item) {
  var read = 0;
  var search = 0;
  var command = 0;
  var modify = 0;
  for (final tool in item.tools) {
    switch (tool.name) {
      case 'read':
        read++;
      case 'grep' || 'find':
        search++;
      case 'bash':
        command++;
      case 'write' || 'edit':
        modify++;
    }
  }
  final elapsed = item.elapsed;
  return [
    '执行过程 · ${item.steps.length} 步',
    if (read > 0) '读取 $read 次',
    if (search > 0) '搜索 $search 次',
    if (command > 0) '命令 $command 条',
    if (modify > 0) '修改 $modify 次',
    if (elapsed != null) '用时 ${_formatDuration(elapsed)}',
  ].join(' · ');
}

/// 工具名 → 中文步骤名（时间线展示用）。
String _toolLabel(String name) => switch (name) {
  'read' => '读取',
  'grep' || 'find' => '搜索',
  'bash' => '命令',
  'write' || 'edit' => '修改',
  'todo_read' || 'todo_write' => '任务清单',
  'ask_user_question' => '提问',
  'create_agent' || 'close_agent' => '子代理',
  'wait_result' || 'wait_all' => '等待子代理',
  'send_message' => '发送消息',
  'goal_done' => '目标完成',
  'list_agents' => '子代理列表',
  _ => name,
};

/// 耗时紧凑格式：<1s 毫秒级、<10s 一位小数、更长取整秒。
String _formatDuration(Duration d) {
  final ms = d.inMilliseconds;
  if (ms < 1000) return '${ms}ms';
  if (ms < 10000) return '${(ms / 1000).toStringAsFixed(1)}s';
  return '${d.inSeconds}s';
}

/// 时间线中的工具步骤行：图标 + 中文步骤名 + 输入摘要 + 耗时 + 状态，
/// 可展开参数与结果预览。
class _ToolStepRow extends StatefulWidget {
  const _ToolStepRow({super.key, required this.item});

  final ToolItem item;

  @override
  State<_ToolStepRow> createState() => _ToolStepRowState();
}

class _ToolStepRowState extends State<_ToolStepRow> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final item = widget.item;
    final (icon, opacity) = _toolStyle(item.name);
    final summary = _toolArgSummary(item.args);
    final expandable = item.args.isNotEmpty || item.resultPreview.isNotEmpty;
    final failed = item.status == ToolStatus.error;
    final duration = item.duration;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        InkWell(
          hoverColor: tokens.secondary,
          onTap: expandable
              ? () => setState(() => _expanded = !_expanded)
              : null,
          child: Padding(
            padding: const EdgeInsets.symmetric(
              horizontal: Spacing.md,
              vertical: 6,
            ),
            child: Row(
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
                  _toolLabel(item.name),
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
                if (duration != null) ...[
                  Text(
                    _formatDuration(duration),
                    style: AppFonts.mono(fontSize: 11, color: tokens.tertiary),
                  ),
                  const SizedBox(width: Spacing.sm),
                ],
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
          ),
        ),
        if (_expanded && expandable)
          Padding(
            // 与步骤名左对齐（左右 padding 16 + 图标 14 + 间距 8）
            padding: const EdgeInsets.only(
              left: Spacing.md + 14 + Spacing.sm,
              right: Spacing.md,
              bottom: Spacing.sm,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (item.args.isNotEmpty)
                  _CopyableCodeBlock(
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
    );
  }
}

/// 时间线中的思考步骤行（合并进执行过程，不再单独出现「思考过程」标签）。
class _ThinkingStepRow extends StatefulWidget {
  const _ThinkingStepRow({super.key, required this.item});

  final AssistantItem item;

  @override
  State<_ThinkingStepRow> createState() => _ThinkingStepRowState();
}

class _ThinkingStepRowState extends State<_ThinkingStepRow> {
  bool _expanded = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        InkWell(
          hoverColor: tokens.secondary,
          onTap: () => setState(() => _expanded = !_expanded),
          child: Padding(
            padding: const EdgeInsets.symmetric(
              horizontal: Spacing.md,
              vertical: 6,
            ),
            child: Row(
              children: [
                Icon(
                  LucideIcons.brain,
                  size: 14,
                  color: tokens.foreground.withValues(alpha: 0.45),
                ),
                const SizedBox(width: Spacing.sm),
                Text('思考', style: AppText.ui(tokens.mutedForeground)),
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
          ),
        ),
        if (_expanded)
          Padding(
            padding: const EdgeInsets.only(
              left: Spacing.md + 14 + Spacing.sm,
              right: Spacing.md,
              bottom: Spacing.sm,
            ),
            child: SelectableText(
              widget.item.thinking,
              style: AppText.ui(tokens.mutedForeground),
            ),
          ),
      ],
    );
  }
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
