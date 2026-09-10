/// InputBar 回归：IME 组词期间按键属于输入法——Enter 确认候选而不是
/// 发送、↑ 移动候选而不是召回；组词提交后 Enter 才发送。
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:nomic_app/app_controller.dart';
import 'package:nomic_app/theme.dart';
import 'package:nomic_app/ui/input_bar.dart';

class _RecordingController extends AppController {
  _RecordingController() : super(url: 'ws://127.0.0.1:1/ws');

  final List<String> sent = [];

  @override
  void send(String text) => sent.add(text);
}

void main() {
  testWidgets('IME 组词期间 Enter 确认候选（不发送、不清空）', (tester) async {
    final controller = _RecordingController();
    addTearDown(controller.dispose);

    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(NomicTokens.light, dark: false),
        home: Scaffold(body: InputBar(controller: controller)),
      ),
    );
    await tester.tap(find.byType(TextField));
    await tester.pump();

    // 模拟 IME 组词：composing region 覆盖整段拼音
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: 'nihao',
        selection: TextSelection.collapsed(offset: 5),
        composing: TextRange(start: 0, end: 5),
      ),
    );
    await tester.pump();

    // 组词中按 Enter：确认候选，不发送、不丢文本
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pump();
    expect(controller.sent, isEmpty);
    expect(find.textContaining('nihao'), findsOneWidget);

    // IME 提交文本（composing 失效）后 Enter 才发送
    tester.testTextInput.updateEditingValue(
      const TextEditingValue(
        text: '你好',
        selection: TextSelection.collapsed(offset: 2),
      ),
    );
    await tester.pump();
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pump();
    expect(controller.sent, ['你好']);
  });

  testWidgets('无 IME 组词时 Enter 正常发送、空输入 ↑ 召回', (tester) async {
    final controller = _RecordingController();
    addTearDown(controller.dispose);

    await tester.pumpWidget(
      MaterialApp(
        theme: buildTheme(NomicTokens.light, dark: false),
        home: Scaffold(body: InputBar(controller: controller)),
      ),
    );
    await tester.tap(find.byType(TextField));
    await tester.pump();

    await tester.enterText(find.byType(TextField), 'hello');
    await tester.sendKeyEvent(LogicalKeyboardKey.enter);
    await tester.pump();
    expect(controller.sent, ['hello']);

    // 空输入 ↑ 召回上次发送的文本
    await tester.sendKeyEvent(LogicalKeyboardKey.arrowUp);
    await tester.pump();
    expect(find.text('hello'), findsOneWidget);
  });
}
