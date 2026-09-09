/// 提问弹层：`ask_user_question`（单选/多选/填空 + 自定义填写），
/// 回答经 `answer_question` 事件回填（ADR-0029 修订 / ADR-0030）。
library;

import 'package:flutter/material.dart';

import '../app_controller.dart';
import '../protocol/models.dart';
import '../theme.dart';

class QuestionSheet extends StatefulWidget {
  const QuestionSheet({super.key, required this.question});

  final PendingQuestion question;

  /// 幂等弹层：同一提问不重复弹出。
  static String? _showingId;

  static void maybeShow(
    BuildContext context,
    AppController controller,
    PendingQuestion question,
  ) {
    if (_showingId == question.id) return;
    _showingId = question.id;
    showDialog<_QuestionAnswer>(
      context: context,
      barrierDismissible: false,
      builder: (context) => QuestionSheet(question: question),
    ).then((answer) {
      _showingId = null;
      // 弹层关闭即视为回答提交（QuestionSheet 内部在提交时 pop 携带答案；
      // 无答案返回（不应发生：barrierDismissible=false 且按钮必填）时忽略）
      if (answer is _QuestionAnswer) {
        controller.answerQuestion(answer.answers, custom: answer.custom);
      }
    });
  }

  @override
  State<QuestionSheet> createState() => _QuestionSheetState();
}

class _QuestionAnswer {
  _QuestionAnswer(this.answers, this.custom);

  final List<String> answers;
  final String? custom;
}

class _QuestionSheetState extends State<QuestionSheet> {
  final _selected = <String>{};
  final _customController = TextEditingController();

  AskUserQuestion get q => widget.question.question;

  @override
  void dispose() {
    _customController.dispose();
    super.dispose();
  }

  bool get _canSubmit {
    if (q.isSingle) return _selected.isNotEmpty;
    if (q.isMultiple) return _selected.isNotEmpty;
    return _customController.text.trim().isNotEmpty;
  }

  void _submit() {
    if (!_canSubmit) return;
    if (q.isSingle || q.isMultiple) {
      final custom = _customController.text.trim();
      Navigator.of(context).pop(
        _QuestionAnswer(_selected.toList(), custom.isEmpty ? null : custom),
      );
    } else {
      final text = _customController.text.trim();
      Navigator.of(context).pop(_QuestionAnswer([text], text));
    }
  }

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return AlertDialog(
      title: Text(q.question, style: const TextStyle(fontSize: 15)),
      content: SizedBox(
        width: 420,
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            if (q.isSingle)
              for (final option in q.options)
                RadioListTile<String>(
                  dense: true,
                  title: Text(option, style: const TextStyle(fontSize: 13)),
                  value: option,
                  // RadioGroup 废弃过渡期：直接用 groupValue/onChanged 旧 API
                  // ignore: deprecated_member_use
                  groupValue: _selected.isEmpty ? null : _selected.first,
                  // ignore: deprecated_member_use
                  onChanged: (value) => setState(() {
                    _selected
                      ..clear()
                      ..add(value!);
                  }),
                )
            else if (q.isMultiple)
              for (final option in q.options)
                CheckboxListTile(
                  dense: true,
                  title: Text(option, style: const TextStyle(fontSize: 13)),
                  value: _selected.contains(option),
                  onChanged: (checked) => setState(() {
                    if (checked ?? false) {
                      _selected.add(option);
                    } else {
                      _selected.remove(option);
                    }
                  }),
                )
            else
              const SizedBox.shrink(),
            if (!q.isSingle && !q.isMultiple || q.options.isNotEmpty)
              Padding(
                padding: const EdgeInsets.only(top: Spacing.sm),
                child: TextField(
                  controller: _customController,
                  decoration: InputDecoration(
                    hintText: q.isSingle || q.isMultiple
                        ? '自定义填写（可选）…'
                        : '输入回答…',
                  ),
                  onChanged: (_) => setState(() {}),
                  onSubmitted: (_) => _submit(),
                ),
              ),
          ],
        ),
      ),
      actions: [
        FilledButton(
          style: FilledButton.styleFrom(
            backgroundColor: tokens.primary,
            foregroundColor: tokens.primaryForeground,
          ),
          onPressed: _canSubmit ? _submit : null,
          child: const Text('提交'),
        ),
      ],
    );
  }
}
