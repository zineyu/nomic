/// 启动页回归：project 列表 + 「添加项目目录…」系统文件选择器入口；
/// project 行 hover 浮现删除 icon 按钮（确认对话框取消后关闭）。
///
/// 注意：未连接时 HomePage 顶部横幅含常转 CircularProgressIndicator，
/// pumpAndSettle 永不 settle，一律用定长 pump。
library;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:lucide_icons/lucide_icons.dart';
import 'package:nomic_app/app_controller.dart';
import 'package:nomic_app/protocol/models.dart';
import 'package:nomic_app/theme.dart';
import 'package:nomic_app/ui/home_page.dart';

void main() {
  for (final dark in [false, true]) {
    final tag = dark ? 'dark' : 'light';
    final tokens = dark ? NomicTokens.dark : NomicTokens.light;

    testWidgets('启动页 project 交互（$tag）', (tester) async {
      final controller = AppController(url: 'ws://127.0.0.1:1/ws');
      addTearDown(controller.dispose);
      controller.projects = [
        const ProjectSummary(id: 'p1', path: '/tmp/alpha', sessionCount: 0),
      ];

      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(tokens, dark: dark),
          home: HomePage(controller: controller),
        ),
      );
      await pumpFrames(tester);

      expect(find.text('选择一个 project 开始'), findsOneWidget);
      expect(find.text('添加项目目录…'), findsOneWidget);
      expect(find.text('alpha'), findsOneWidget);
      expect(find.text('0 个会话'), findsOneWidget);

      // hover 行 → 会话数让位给删除 icon
      expect(find.byIcon(LucideIcons.trash2), findsNothing);
      final gesture = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await gesture.addPointer(location: Offset.zero);
      addTearDown(gesture.removePointer);
      await gesture.moveTo(tester.getCenter(find.text('alpha')));
      await pumpFrames(tester);
      expect(find.byIcon(LucideIcons.trash2), findsOneWidget);
      expect(find.text('0 个会话'), findsNothing);

      // 空 project：删除确认无级联文案，取消后关闭
      await tester.tap(find.byIcon(LucideIcons.trash2));
      await pumpFrames(tester);
      expect(find.text('删除这个 project？'), findsOneWidget);
      expect(find.textContaining('将从项目列表移除'), findsOneWidget);
      await tester.tap(find.text('取消'));
      await pumpFrames(tester);
      expect(find.text('删除这个 project？'), findsNothing);

      // 「添加项目目录…」：测试环境无平台 handler，静默取消（不崩溃）
      await tester.tap(find.text('添加项目目录…'));
      await pumpFrames(tester);
      expect(find.text('选择一个 project 开始'), findsOneWidget);
    });
  }
}

/// 定长帧推进（横幅 spinner 常转，不能 pumpAndSettle）。
Future<void> pumpFrames(WidgetTester tester) async {
  for (var i = 0; i < 10; i++) {
    await tester.pump(const Duration(milliseconds: 50));
  }
}
