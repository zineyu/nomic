/// 消息项渲染：用户气泡 / assistant（markdown + thinking 步骤行）/
/// 步骤行（工具调用与纯思考，与执行过程卡片内部同一组件）/ 系统提示。
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
  /// 并使用 prose 排版档（markdown body，14/24）。
  final bool isAnswer;

  @override
  Widget build(BuildContext context) {
    return switch (item) {
      UserItem i => _UserBubble(item: i),
      // 纯思考（组外未折叠的尾部段、流式中的思考阶段）与工具调用
      // 都复用执行过程卡片内部的步骤行组件
      AssistantItem i =>
        i.text.isEmpty && i.thinking.isNotEmpty
            ? Padding(
                padding: const EdgeInsets.only(bottom: Spacing.sm),
                child: _StepRow(step: i),
              )
            : _AssistantBlock(item: i, isAnswer: isAnswer),
      ToolItem i => Padding(
        padding: const EdgeInsets.only(bottom: 4),
        child: _StepRow(step: i),
      ),
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
    // 用户消息气泡（bubble token，light 为浅蓝 #EDF3FE）：无边框无阴影、限宽右对齐
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
              borderRadius: BorderRadius.circular(Radii.xxl),
            ),
            child: SelectableText(
              item.text,
              style: AppText.s(tokens.foreground),
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
    // 回答块用 prose（markdown body 档）；中间过程叙述用 s 档（弱化）
    final textStyle = isAnswer
        ? AppText.prose(tokens.foreground)
        : AppText.s(tokens.foreground);
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
                    style: AppText.xxs(
                      tokens.secondary,
                    ).copyWith(fontWeight: FontWeight.w600),
                  ),
                  const SizedBox(width: Spacing.sm),
                  Expanded(child: Container(height: 1, color: tokens.border)),
                ],
              ),
            ),
          // thinking 与工具调用共用同一步骤行组件（组内外形态一致）
          if (item.thinking.isNotEmpty)
            Padding(
              padding: const EdgeInsets.only(bottom: Spacing.sm),
              child: _StepRow(step: item),
            ),
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
                h4: AppText.h4(tokens.foreground),
                h5: AppText.h4(tokens.foreground),
                h6: AppText.h4(tokens.foreground),
                // 链接不走彩色：ink + 下划线
                a: textStyle.copyWith(decoration: TextDecoration.underline),
                // 行内 code chip：等宽 + 浅灰底（crate 名 / 版本号 / 路径）
                code: AppFonts.mono(
                  fontSize: 12,
                  color: tokens.foreground,
                ).copyWith(backgroundColor: tokens.codeSurface),
                codeblockPadding: const EdgeInsets.all(Spacing.md),
                codeblockDecoration: BoxDecoration(
                  color: tokens.codeSurface,
                  borderRadius: BorderRadius.circular(Radii.lg),
                ),
                listBullet: textStyle,
                blockquote: textStyle.copyWith(color: tokens.secondary),
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
                tableHead: AppText.tableHead(tokens.foreground),
                tableBody: AppText.table(tokens.foreground),
                tableBorder: TableBorder.all(color: tokens.borderStrong),
                tableColumnWidth: const IntrinsicColumnWidth(),
                tableScrollbarThumbVisibility: true,
                tableCellsDecoration: BoxDecoration(color: tokens.codeSurface),
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
              child: Text(item.errorMessage!, style: AppText.xs(tokens.error)),
            ),
        ],
      ),
    );
  }
}

/// 执行过程卡片（DESIGN.md「Execution card」）：text 收尾的一段连续工具
/// 调用与纯思考段折叠为一张卡片——默认一行摘要（执行过程 · N 步 ·
/// 搜索 X 次 · 命令 Y 条 · 用时 Zs），展开为逐步时间线（图标 + 名称 +
/// 输入摘要 + 状态 + 耗时 + 详情）。执行过程是弱化的次要信息，
/// 最终回答才是主体。
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
        borderRadius: BorderRadius.circular(Radii.xxl),
        border: Border.all(color: tokens.border),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          // 摘要头（整行可点，键盘可聚焦展开）
          InkWell(
            borderRadius: BorderRadius.circular(Radii.xxl),
            hoverColor: tokens.primaryDimmed,
            onTap: () => setState(() => _expanded = !_expanded),
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: Spacing.md,
                vertical: 10,
              ),
              child: Row(
                children: [
                  Icon(LucideIcons.layers, size: 14, color: tokens.secondary),
                  const SizedBox(width: Spacing.sm),
                  Expanded(
                    child: Text(
                      _executionSummary(item),
                      overflow: TextOverflow.ellipsis,
                      style: AppText.xs(tokens.secondary),
                    ),
                  ),
                  if (errorCount > 0) ...[
                    Icon(LucideIcons.x, size: 12, color: tokens.error),
                    const SizedBox(width: 4),
                    Text('$errorCount 失败', style: AppText.xxs(tokens.error)),
                    const SizedBox(width: Spacing.sm),
                  ],
                  if (item.hasRunning)
                    SizedBox(
                      width: 12,
                      height: 12,
                      child: CircularProgressIndicator(
                        strokeWidth: 2,
                        color: tokens.business,
                      ),
                    )
                  else
                    Icon(
                      _expanded
                          ? LucideIcons.chevronDown
                          : LucideIcons.chevronRight,
                      size: 12,
                      color: tokens.secondary,
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
                    _StepRow(
                      key: ValueKey(
                        step is ToolItem ? step.toolCallId : step.id,
                      ),
                      step: step,
                      inCard: true,
                    ),
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

/// 时间线步骤行（工具调用与纯思考段共用同一组件）：图标 + 步骤名 +
/// 摘要 + 耗时 + 状态，点击展开详情。摘要内容：工具取调用参数
/// （`_toolArgSummary`），思考取首行；耗时与状态仅工具步骤有。
class _StepRow extends StatefulWidget {
  const _StepRow({super.key, required this.step, this.inCard = false});

  /// ToolItem 或纯思考 AssistantItem（text 为空、thinking 非空）。
  final ChatItem step;

  /// 是否在执行过程卡片内：卡片内保留 Spacing.md 水平 inset（与摘要头对齐）；
  /// 直接落在消息列的步骤行无 inset，图标与 agent 正文左对齐。
  final bool inCard;

  @override
  State<_StepRow> createState() => _StepRowState();
}

class _StepRowState extends State<_StepRow> {
  bool _expanded = false;

  /// 思考摘要：trim 后的首行。
  static String _thinkingSummary(String thinking) =>
      thinking.trim().split('\n').first;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final step = widget.step;
    final tool = step is ToolItem ? step : null;
    final thinking = step is AssistantItem ? step : null;
    final failed = tool?.status == ToolStatus.error;
    final icon = tool != null ? _toolIcon(tool.name) : LucideIcons.brain;
    final label = tool != null ? _toolLabel(tool.name) : '思考';
    final summary = tool != null
        ? _toolArgSummary(tool.args)
        : _thinkingSummary(thinking?.thinking ?? '');
    final expandable = tool != null
        ? tool.args.isNotEmpty || tool.resultPreview.isNotEmpty
        : (thinking?.thinking.isNotEmpty ?? false);
    final duration = tool?.duration;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        InkWell(
          hoverColor: tokens.primaryDimmed,
          onTap: expandable
              ? () => setState(() => _expanded = !_expanded)
              : null,
          child: Padding(
            padding: EdgeInsets.symmetric(
              horizontal: widget.inCard ? Spacing.md : 0,
              vertical: 6,
            ),
            child: Row(
              children: [
                Icon(
                  icon,
                  size: 14,
                  color: failed ? tokens.error : tokens.secondary,
                ),
                const SizedBox(width: Spacing.sm),
                Text(
                  label,
                  style: AppText.xs(
                    tokens.secondary,
                  ).copyWith(fontWeight: FontWeight.w500),
                ),
                if (summary.isNotEmpty) ...[
                  const SizedBox(width: Spacing.sm),
                  // Expanded（tight）独占全部剩余空间，右侧耗时/状态/折叠
                  // 按钮钉在最右，不随 summary 长度漂移（Flexible + Spacer
                  // 双 flex 会均分剩余空间，短 summary 的未用配额会漏到行尾）
                  Expanded(
                    child: Text(
                      summary,
                      overflow: TextOverflow.ellipsis,
                      style: AppFonts.mono(
                        fontSize: 12,
                        color: tokens.secondary,
                      ),
                    ),
                  ),
                ] else
                  const Spacer(),
                if (duration != null) ...[
                  Text(
                    _formatDuration(duration),
                    style: AppFonts.mono(fontSize: 11, color: tokens.tertiary),
                  ),
                  const SizedBox(width: Spacing.sm),
                ],
                if (tool != null) _ToolStatus(status: tool.status),
                if (expandable) ...[
                  const SizedBox(width: 4),
                  Icon(
                    _expanded
                        ? LucideIcons.chevronDown
                        : LucideIcons.chevronRight,
                    size: 12,
                    color: tokens.secondary,
                  ),
                ],
              ],
            ),
          ),
        ),
        if (_expanded && expandable)
          Padding(
            // 与步骤名左对齐（行水平 inset + 图标 14 + 间距 8）
            padding: EdgeInsets.only(
              left: (widget.inCard ? Spacing.md : 0) + 14 + Spacing.sm,
              right: widget.inCard ? Spacing.md : 0,
              bottom: Spacing.sm,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                if (tool != null && tool.args.isNotEmpty)
                  _CopyableCodeBlock(
                    text: _prettyJson(tool.args),
                    color: tokens.secondary,
                  ),
                if (tool != null && tool.resultPreview.isNotEmpty)
                  Padding(
                    padding: const EdgeInsets.only(top: 4),
                    child: _CopyableCodeBlock(
                      text: tool.resultPreview.length > 2000
                          ? '${tool.resultPreview.substring(0, 2000)}…'
                          : tool.resultPreview,
                      color: tool.isError ? tokens.error : tokens.foreground,
                    ),
                  ),
                if (thinking != null)
                  SelectableText(
                    thinking.thinking,
                    style: AppText.xs(tokens.secondary),
                  ),
              ],
            ),
          ),
      ],
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
            color: tokensOf(context).secondary,
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
          color: tokens.secondary,
        ),
      ),
      ToolStatus.done => Icon(
        LucideIcons.check,
        size: 12,
        color: tokens.secondary,
      ),
      ToolStatus.error => Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(LucideIcons.x, size: 12, color: tokens.error),
          const SizedBox(width: 4),
          Text('失败', style: AppText.xxs(tokens.error)),
        ],
      ),
    };
  }
}

/// 工具名 → lucide 图标（类别由图标图形表达；步骤行内容统一灰阶，
/// 仅失败态用 error 红）。
IconData _toolIcon(String name) => switch (name) {
  'bash' => LucideIcons.terminal,
  'read' => LucideIcons.fileText,
  'grep' => LucideIcons.search,
  'find' => LucideIcons.folderSearch,
  'list_agents' => LucideIcons.users,
  'todo_read' => LucideIcons.listChecks,
  'write' => LucideIcons.filePlus,
  'edit' => LucideIcons.fileEdit,
  'todo_write' => LucideIcons.listTodo,
  'ask_user_question' => LucideIcons.helpCircle,
  'send_message' => LucideIcons.send,
  'goal_done' => LucideIcons.flag,
  'create_agent' || 'close_agent' => LucideIcons.bot,
  'wait_result' || 'wait_all' => LucideIcons.hourglass,
  _ => LucideIcons.wrench,
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
            colors: [tokens.secondary, tokens.foreground, tokens.secondary],
            stops: [
              (t - 0.3).clamp(0.0, 1.0),
              t.clamp(0.0, 1.0),
              (t + 0.3).clamp(0.0, 1.0),
            ],
          ).createShader(bounds),
          child: child,
        );
      },
      child: Text(widget.text, style: AppText.xxs(Colors.white)),
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
        child: Text(item.text, style: AppText.xxs(tokens.secondary)),
      ),
    );
  }
}
