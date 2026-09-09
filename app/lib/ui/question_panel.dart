/// 提问面板：`ask_user_question`（单选/多选/填空 + 自定义填写）内嵌在
/// composer 上方——不阻塞浏览消息流（与 TUI 底部面板同形态），回答经
/// `answer_question` 事件回填（ADR-0029 修订 / ADR-0030）。
library;

import 'package:flutter/material.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../app_controller.dart';
import '../protocol/models.dart';
import '../theme.dart';

class QuestionPanel extends StatefulWidget {
  const QuestionPanel({
    super.key,
    required this.controller,
    required this.question,
  });

  final AppController controller;
  final PendingQuestion question;

  @override
  State<QuestionPanel> createState() => _QuestionPanelState();
}

class _QuestionPanelState extends State<QuestionPanel> {
  final _selected = <String>{};
  final _customController = TextEditingController();

  AskUserQuestion get q => widget.question.question;

  @override
  void dispose() {
    _customController.dispose();
    super.dispose();
  }

  bool get _canSubmit {
    if (q.isSingle || q.isMultiple) return _selected.isNotEmpty;
    return _customController.text.trim().isNotEmpty;
  }

  void _submit() {
    if (!_canSubmit) return;
    if (q.isSingle || q.isMultiple) {
      final custom = _customController.text.trim();
      widget.controller.answerQuestion(
        _selected.toList(),
        custom: custom.isEmpty ? null : custom,
      );
    } else {
      final text = _customController.text.trim();
      widget.controller.answerQuestion([text], custom: text);
    }
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Container(
      width: double.infinity,
      decoration: BoxDecoration(
        color: tokens.card,
        borderRadius: BorderRadius.circular(Radii.lg),
        border: Border.all(color: tokens.border),
      ),
      padding: const EdgeInsets.all(Spacing.md),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(
                LucideIcons.helpCircle,
                size: 14,
                color: tokens.mutedForeground,
              ),
              const SizedBox(width: Spacing.sm),
              Expanded(
                child: Text(
                  q.question,
                  style: TextStyle(
                    fontSize: 13,
                    fontWeight: FontWeight.w500,
                    color: tokens.foreground,
                  ),
                ),
              ),
            ],
          ),
          if (q.options.isNotEmpty) ...[
            const SizedBox(height: Spacing.sm),
            for (final option in q.options)
              _OptionRow(
                label: option,
                multiple: q.isMultiple,
                selected: _selected.contains(option),
                onTap: () => setState(() {
                  if (q.isMultiple) {
                    if (!_selected.remove(option)) _selected.add(option);
                  } else {
                    _selected
                      ..clear()
                      ..add(option);
                  }
                }),
              ),
          ],
          const SizedBox(height: Spacing.sm),
          TextField(
            controller: _customController,
            decoration: InputDecoration(
              hintText: q.isSingle || q.isMultiple ? '自定义填写（可选）…' : '输入回答…',
            ),
            onChanged: (_) => setState(() {}),
            onSubmitted: (_) => _submit(),
          ),
          const SizedBox(height: Spacing.sm),
          Row(
            mainAxisAlignment: MainAxisAlignment.end,
            children: [
              FilledButton(
                style: FilledButton.styleFrom(
                  backgroundColor: tokens.primary,
                  foregroundColor: tokens.primaryForeground,
                ),
                onPressed: _canSubmit ? _submit : null,
                child: const Text('提交'),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

/// 选项行：整行可点，图标表达单选（circle / circleDot）与
/// 多选（square / checkSquare）两种语义。
class _OptionRow extends StatelessWidget {
  const _OptionRow({
    required this.label,
    required this.multiple,
    required this.selected,
    required this.onTap,
  });

  final String label;
  final bool multiple;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    final icon = multiple
        ? (selected ? LucideIcons.checkSquare : LucideIcons.square)
        : (selected ? LucideIcons.circleDot : LucideIcons.circle);
    return InkWell(
      borderRadius: BorderRadius.circular(Radii.md),
      hoverColor: tokens.muted,
      onTap: onTap,
      child: Padding(
        padding: const EdgeInsets.symmetric(
          horizontal: Spacing.sm,
          vertical: Spacing.sm,
        ),
        child: Row(
          children: [
            Icon(
              icon,
              size: 14,
              color: selected ? tokens.foreground : tokens.mutedForeground,
            ),
            const SizedBox(width: Spacing.sm),
            Expanded(
              child: Text(
                label,
                style: TextStyle(fontSize: 13, color: tokens.foreground),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
