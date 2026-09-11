/// 输入区：Codex 式统一 composer——圆角容器内嵌多行输入与底部控制行
/// （模型选择 chip + 上下文用量环 + 圆形发送/停止），聚焦时边框
/// 变为 DeepSeek 蓝 ring。
///
/// 运行中提交的消息进 steering 队列（与服务端同一语义：当前步骤完成后
/// 注入本轮）；Esc 取消当前运行的快捷键绑定在 chat_page。
library;

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../theme.dart';
import 'model_picker.dart';

class InputBar extends StatefulWidget {
  const InputBar({super.key, required this.controller});

  final AppController controller;

  @override
  State<InputBar> createState() => _InputBarState();
}

class _InputBarState extends State<InputBar> {
  final _textController = TextEditingController();
  late final _focusNode = FocusNode(onKeyEvent: _onKeyEvent);
  bool _focused = false;

  /// 上次发送的文本（空输入时 ↑ 召回）。
  String? _lastSent;

  @override
  void initState() {
    super.initState();
    _focusNode.addListener(
      () => setState(() => _focused = _focusNode.hasFocus),
    );
    // 空文本时禁用发送按钮（Codex 同款：按钮常驻，空输入置灰）
    _textController.addListener(() => setState(() {}));
  }

  @override
  void dispose() {
    _textController.dispose();
    _focusNode.dispose();
    super.dispose();
  }

  void _submit() {
    final text = _textController.text.trim();
    if (text.isEmpty) return;
    _lastSent = text;
    widget.controller.send(text);
    _textController.clear();
    _focusNode.requestFocus();
  }

  /// Enter / ⌘Enter 发送（桌面多行 TextField 默认对 Enter 插入换行，这里
  /// 消费事件拦截）；Shift+Enter 换行；空输入时 ↑ 召回上次发送的文本。
  /// 移动端软键盘发送走 onSubmitted。
  ///
  /// IME 组词期间（composing region 有效）按键属于输入法：Enter 是确认
  /// 候选、方向键是移动候选，一律放行——否则中/日文输入法按 Enter 确认
  /// 候选时会把半成品文本直接发送出去，IME 不可用。
  KeyEventResult _onKeyEvent(FocusNode node, KeyEvent event) {
    if (event is! KeyDownEvent) return KeyEventResult.ignored;
    final composing = _textController.value.composing;
    if (composing.isValid && !composing.isCollapsed) {
      return KeyEventResult.ignored;
    }
    if ((event.logicalKey == LogicalKeyboardKey.enter ||
            event.logicalKey == LogicalKeyboardKey.numpadEnter) &&
        !HardwareKeyboard.instance.isShiftPressed) {
      _submit();
      return KeyEventResult.handled;
    }
    final lastSent = _lastSent;
    if (event.logicalKey == LogicalKeyboardKey.arrowUp &&
        _textController.text.isEmpty &&
        lastSent != null) {
      _textController.text = lastSent;
      _textController.selection = TextSelection.collapsed(
        offset: lastSent.length,
      );
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final controller = widget.controller;
    final running = controller.running;
    final canSend = _textController.text.trim().isNotEmpty;
    return Container(
      decoration: BoxDecoration(
        color: tokens.inputMajor,
        // 悬浮 composer：全界面唯一带阴影的在流元素（DESIGN.md
        // 「Shadow」例外），radius 走 2xl 胶囊档
        borderRadius: BorderRadius.circular(Radii.xxxl),
        // focus ring：DeepSeek 蓝描边（不只依赖阴影变化）
        border: Border.all(
          color: _focused ? tokens.business : tokens.borderStrong,
          width: _focused ? 1.5 : 1,
        ),
        // 悬浮 composer：全界面唯一带阴影的在流元素，走 DESIGN.md soft 档
        boxShadow: AppShadows.soft(),
      ),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          TextField(
            controller: _textController,
            focusNode: _focusNode,
            minLines: 1,
            maxLines: 8,
            textInputAction: TextInputAction.send,
            onSubmitted: (_) => _submit(),
            decoration: InputDecoration(
              hintText: running ? '运行中，发送将进入队列…' : '描述任务，输入 / 调用技能',
              filled: false,
              border: InputBorder.none,
              enabledBorder: InputBorder.none,
              focusedBorder: InputBorder.none,
              contentPadding: const EdgeInsets.fromLTRB(
                Spacing.md,
                Spacing.md,
                Spacing.md,
                Spacing.sm,
              ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.fromLTRB(
              Spacing.sm,
              0,
              Spacing.sm,
              Spacing.sm,
            ),
            child: Row(
              children: [
                _ModelChip(controller: controller),
                const SizedBox(width: Spacing.sm),
                _ContextUsage(controller: controller),
                const Spacer(),
                // 发送三态：空输入禁用（灰）/ 有效启用（品牌蓝）/
                // 运行中切换为停止按钮（Enter 仍可提交进队列）
                if (running)
                  _CircleButton(
                    icon: LucideIcons.square,
                    iconSize: 12,
                    tooltip: '停止当前运行（Esc）',
                    background: tokens.primary,
                    foreground: tokens.primaryForeground,
                    onPressed: controller.cancel,
                  )
                else
                  _CircleButton(
                    icon: LucideIcons.arrowUp,
                    iconSize: 18,
                    tooltip: '发送（Enter / ⌘Enter）',
                    background: canSend
                        ? tokens.business
                        : tokens.primaryDimmed,
                    foreground: canSend ? Colors.white : tokens.tertiary,
                    onPressed: canSend ? _submit : null,
                  ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

/// composer 底部控制行的模型选择 chip（点击弹候选列表，与 TUI `/models`
/// 同一口径）。
class _ModelChip extends StatelessWidget {
  const _ModelChip({required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final name = controller.model.name;
    return Material(
      color: Colors.transparent,
      borderRadius: BorderRadius.circular(Radii.md),
      child: InkWell(
        borderRadius: BorderRadius.circular(Radii.md),
        hoverColor: tokens.primaryDimmed,
        onTap: () => ModelPicker.show(context, controller),
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: Spacing.sm,
            vertical: 4,
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                name.isEmpty ? '选择模型' : name,
                style: AppText.xxs(tokens.secondary),
              ),
              const SizedBox(width: 4),
              Icon(LucideIcons.chevronDown, size: 12, color: tokens.secondary),
            ],
          ),
        ),
      ),
    );
  }
}

/// 圆形发送/停止按钮（36px；composer 主操作位）。
class _CircleButton extends StatelessWidget {
  const _CircleButton({
    required this.icon,
    required this.iconSize,
    required this.tooltip,
    required this.background,
    required this.foreground,
    required this.onPressed,
  });

  final IconData icon;
  final double iconSize;
  final String tooltip;
  final Color background;
  final Color foreground;
  final VoidCallback? onPressed;

  @override
  Widget build(BuildContext context) {
    return Tooltip(
      message: tooltip,
      child: Material(
        color: background,
        shape: const CircleBorder(),
        child: InkWell(
          customBorder: const CircleBorder(),
          onTap: onPressed,
          child: SizedBox(
            width: 36,
            height: 36,
            child: Icon(icon, size: iconSize, color: foreground),
          ),
        ),
      ),
    );
  }
}

/// 上下文用量：常态为环形进度条（14px，轨道 border、进度按占比走
/// 色阶）；光标悬停时经 Tooltip 展开具体数值。窗口未知（候选列表未
/// 加载）时降级为纯文本。接近上限走警告色阶：<75% muted、
/// 75–90% ink、≥90% warning。
class _ContextUsage extends StatelessWidget {
  const _ContextUsage({required this.controller});

  final AppController controller;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final used = controller.contextTokens;
    int? window;
    for (final c in controller.modelCandidates) {
      if (c.id == controller.model.id &&
          c.provider == controller.model.provider) {
        window = c.contextWindow;
        break;
      }
    }
    if (window == null || window == 0) {
      return Text(
        '${_compactTokens(used)} tokens',
        style: AppText.xxs(tokens.secondary),
      );
    }
    final ratio = (used / window).clamp(0.0, 1.0);
    final color = ratio >= 0.9
        ? tokens.warning
        : ratio >= 0.75
        ? tokens.foreground
        : tokens.secondary;
    final percent = (ratio * 100).round();
    return Tooltip(
      message:
          '上下文 ${_compactTokens(used)} / ${_compactTokens(window)} '
          'tokens · $percent%',
      child: Semantics(
        label: '上下文用量 $percent%',
        child: CustomPaint(
          size: const Size.square(_ContextRing.size),
          painter: _ContextRingPainter(
            ratio: ratio,
            color: color,
            track: tokens.border,
          ),
        ),
      ),
    );
  }
}

/// 环形进度条的尺寸与绘制（细描边圆环，起点 12 点方向顺时针）。
abstract final class _ContextRing {
  static const double size = 14;
  static const double strokeWidth = 2;
}

class _ContextRingPainter extends CustomPainter {
  _ContextRingPainter({
    required this.ratio,
    required this.color,
    required this.track,
  });

  final double ratio;
  final Color color;
  final Color track;

  @override
  void paint(Canvas canvas, Size size) {
    final center = size.center(Offset.zero);
    final radius = (size.shortestSide - _ContextRing.strokeWidth) / 2;
    final rect = Rect.fromCircle(center: center, radius: radius);
    const start = -math.pi / 2;
    const sweep = 2 * math.pi;
    final stroke = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = _ContextRing.strokeWidth;
    canvas.drawArc(rect, start, sweep, false, stroke..color = track);
    if (ratio > 0) {
      canvas.drawArc(rect, start, sweep * ratio, false, stroke..color = color);
    }
  }

  @override
  bool shouldRepaint(_ContextRingPainter old) =>
      old.ratio != ratio || old.color != color || old.track != track;
}

/// token 数紧凑显示（`9.7k` / `256k` / `970`）。
String _compactTokens(int tokens) {
  if (tokens >= 1000) {
    return '${(tokens / 1000).toStringAsFixed(1)}k';
  }
  return '$tokens';
}
