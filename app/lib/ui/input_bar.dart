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
  KeyEventResult _onKeyEvent(FocusNode node, KeyEvent event) {
    if (event is! KeyDownEvent) return KeyEventResult.ignored;
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
    final dark = Theme.of(context).brightness == Brightness.dark;
    final controller = widget.controller;
    final running = controller.running;
    final canSend = _textController.text.trim().isNotEmpty;
    return Container(
      decoration: BoxDecoration(
        color: tokens.card,
        // 悬浮 composer：全界面唯一带阴影的在流元素（DESIGN.md
        // 「Shadow」例外），radius 走 2xl 胶囊档
        borderRadius: BorderRadius.circular(Radii.xxl),
        // focus ring：清晰的 accent 描边（不只依赖阴影变化）
        border: Border.all(
          color: _focused ? tokens.accent : tokens.border,
          width: _focused ? 1.5 : 1,
        ),
        boxShadow: [
          BoxShadow(
            color: Colors.black.withValues(alpha: dark ? 0.24 : 0.06),
            blurRadius: 16,
            offset: const Offset(0, 4),
          ),
        ],
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
                    background: canSend ? tokens.accent : tokens.secondary,
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
              Text(
                name.isEmpty ? '选择模型' : name,
                style: AppText.caption(tokens.mutedForeground),
              ),
              const SizedBox(width: 4),
              Icon(
                LucideIcons.chevronDown,
                size: 12,
                color: tokens.mutedForeground,
              ),
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

/// 上下文用量：「9.7k / 256k tokens」；上限从候选模型的 contextWindow
/// 推导（未加载候选列表时只显示已用量）。接近上限走警告色阶：
/// <75% muted、75–90% ink、≥90% warning。
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
    final ratio = window == null || window == 0 ? 0.0 : used / window;
    final color = ratio >= 0.9
        ? tokens.warning
        : ratio >= 0.75
        ? tokens.foreground
        : tokens.mutedForeground;
    return Text(
      window == null
          ? '${_compactTokens(used)} tokens'
          : '${_compactTokens(used)} / ${_compactTokens(window)} tokens',
      style: AppText.caption(color),
    );
  }
}

/// token 数紧凑显示（`9.7k` / `256k` / `970`）。
String _compactTokens(int tokens) {
  if (tokens >= 1000) {
    return '${(tokens / 1000).toStringAsFixed(1)}k';
  }
  return '$tokens';
}
