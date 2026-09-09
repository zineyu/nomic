/// 输入区：Codex 式统一 composer——圆角容器内嵌多行输入与底部控制行
/// （模型选择 chip + 上下文 token 计数 + 圆形发送/停止），聚焦时边框
/// 变为 ring/50。
///
/// 运行中提交的消息进 steering 队列（与服务端同一语义：当前步骤完成后
/// 注入本轮）；Esc 取消当前运行的快捷键绑定在 chat_page。
library;

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
    widget.controller.send(text);
    _textController.clear();
    _focusNode.requestFocus();
  }

  /// Enter 发送（桌面多行 TextField 默认对 Enter 插入换行，这里消费事件
  /// 拦截）；Shift+Enter 换行。移动端软键盘发送走 onSubmitted。
  KeyEventResult _onKeyEvent(FocusNode node, KeyEvent event) {
    if (event is KeyDownEvent &&
        (event.logicalKey == LogicalKeyboardKey.enter ||
            event.logicalKey == LogicalKeyboardKey.numpadEnter) &&
        !HardwareKeyboard.instance.isShiftPressed) {
      _submit();
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
        color: tokens.card,
        borderRadius: BorderRadius.circular(Radii.xl),
        border: Border.all(
          color: _focused
              ? tokens.primary.withValues(alpha: 0.5)
              : tokens.border,
        ),
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
              hintText: running ? '运行中，发送将进入队列…' : '给 Nomic 发送消息…',
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
                Text(
                  _formatTokens(controller.contextTokens),
                  style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
                ),
                const Spacer(),
                // 停止与发送并存：运行中发送即排队（与 Enter 提交同语义）
                if (running) ...[
                  _CircleButton(
                    icon: LucideIcons.square,
                    iconSize: 12,
                    tooltip: '停止当前运行（Esc）',
                    background: tokens.secondary,
                    foreground: tokens.foreground,
                    onPressed: controller.cancel,
                  ),
                  const SizedBox(width: Spacing.sm),
                ],
                _CircleButton(
                  icon: LucideIcons.arrowUp,
                  iconSize: 16,
                  tooltip: running ? '发送（进入队列）' : '发送',
                  background: canSend ? tokens.primary : tokens.muted,
                  foreground: canSend
                      ? tokens.primaryForeground
                      : tokens.mutedForeground,
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
        hoverColor: tokens.muted,
        onTap: () => ModelPicker.show(context, controller),
        child: Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: Spacing.sm,
            vertical: 4,
          ),
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(LucideIcons.cpu, size: 14, color: tokens.mutedForeground),
              const SizedBox(width: 4),
              Text(
                name.isEmpty ? '选择模型' : name,
                style: TextStyle(fontSize: 12, color: tokens.mutedForeground),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 圆形发送/停止按钮（32px；icon button 尺寸阶梯内的 sm 档）。
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
            width: 32,
            height: 32,
            child: Icon(icon, size: iconSize, color: foreground),
          ),
        ),
      ),
    );
  }
}

/// 上下文 token 紧凑显示（`12.3k tokens`）。
String _formatTokens(int tokens) {
  if (tokens >= 1000) {
    return '${(tokens / 1000).toStringAsFixed(1)}k tokens';
  }
  return '$tokens tokens';
}
