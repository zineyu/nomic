/// 输入区：多行输入 + 发送/停止（运行中提交的消息进 steering 队列，
/// 与服务端同一语义：当前步骤完成后注入本轮）。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../theme.dart';

class InputBar extends StatefulWidget {
  const InputBar({super.key, required this.controller});

  final AppController controller;

  @override
  State<InputBar> createState() => _InputBarState();
}

class _InputBarState extends State<InputBar> {
  final _textController = TextEditingController();
  final _focusNode = FocusNode();

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

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final running = widget.controller.running;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.end,
      children: [
        Expanded(
          child: KeyboardListener(
            focusNode: FocusNode(),
            child: TextField(
              controller: _textController,
              focusNode: _focusNode,
              minLines: 1,
              maxLines: 8,
              textInputAction: TextInputAction.send,
              onSubmitted: (_) => _submit(),
              decoration: InputDecoration(
                hintText: running ? '运行中，发送将进入队列…' : '输入消息…',
              ),
            ),
            onKeyEvent: (event) {
              // Shift+Enter 换行；Enter 发送（TextInputAction.send 已覆盖
              // 常规情况，这里兜底桌面键盘）
              if (event is KeyDownEvent &&
                  event.logicalKey == LogicalKeyboardKey.enter &&
                  !HardwareKeyboard.instance.isShiftPressed) {
                _submit();
              }
            },
          ),
        ),
        const SizedBox(width: Spacing.sm),
        if (running)
          IconButton.filled(
            icon: const Icon(LucideIcons.square, size: 14),
            tooltip: '停止当前运行',
            style: IconButton.styleFrom(
              backgroundColor: tokens.secondary,
              foregroundColor: tokens.foreground,
            ),
            onPressed: widget.controller.cancel,
          )
        else
          IconButton.filled(
            icon: const Icon(LucideIcons.arrowUp, size: 16),
            tooltip: '发送',
            style: IconButton.styleFrom(
              backgroundColor: tokens.primary,
              foregroundColor: tokens.primaryForeground,
            ),
            onPressed: _submit,
          ),
      ],
    );
  }
}
