/// 侧栏会话瓦片回归：选中 + 悬停不得引入半透明 hover 叠加层。
///
/// 背景：`_WorkTile` 曾对选中项传 `hoverColor: null`，回落到主题默认的
/// 4% 半透明黑 hover 色，在 macOS Impeller 下渲染成实心黑块（点击会话后
/// 背景变黑、鼠标移开恢复）。修复后 hover 底色走 Material 的不透明颜色，
/// InkWell 的 hoverColor 必须为透明（与 `_NewTaskButton` 同构）。
library;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:nomic_app/app_controller.dart';
import 'package:nomic_app/protocol/models.dart';
import 'package:nomic_app/theme.dart';
import 'package:nomic_app/ui/sidebar.dart';

void main() {
  for (final dark in [false, true]) {
    final tag = dark ? 'dark' : 'light';
    final tokens = dark ? NomicTokens.dark : NomicTokens.light;

    testWidgets('hover 不叠加半透明层（$tag）', (tester) async {
      final controller = AppController(url: 'ws://127.0.0.1:1/ws');
      addTearDown(controller.dispose);
      controller.works = [_work('w1', 's1', '会话甲'), _work('w2', 's2', '会话乙')];
      controller.sessionId = 's2';

      await tester.pumpWidget(
        MaterialApp(
          theme: buildTheme(tokens, dark: dark),
          home: Scaffold(
            body: ListenableBuilder(
              listenable: controller,
              builder: (context, _) => Sidebar(controller: controller),
            ),
          ),
        ),
      );
      await tester.pumpAndSettle();

      final selectedText = find.text('会话乙');
      final unselectedText = find.text('会话甲');

      Material tileMaterial(Finder text) => tester.widget(
        find.ancestor(of: text, matching: find.byType(Material)).first,
      );
      InkWell tileInkWell(Finder text) => tester.widget(
        find.ancestor(of: text, matching: find.byType(InkWell)).first,
      );

      // 悬停叠加层透明：渲染层不会画出半透明黑（Impeller 下会呈实心黑）
      expect(tileInkWell(selectedText).hoverColor, Colors.transparent);
      expect(tileInkWell(unselectedText).hoverColor, Colors.transparent);

      // 选中项底色即 sidebarAccent，不随悬停变化
      expect(tileMaterial(selectedText).color, tokens.sidebarAccent);

      // 未选中项：悬停前透明，悬停后不透明 sidebarAccent，移开后还原
      expect(tileMaterial(unselectedText).color, Colors.transparent);
      final gesture = await tester.createGesture(kind: PointerDeviceKind.mouse);
      await gesture.addPointer(location: Offset.zero);
      addTearDown(gesture.removePointer);
      await gesture.moveTo(tester.getCenter(unselectedText));
      await tester.pumpAndSettle();
      expect(tileMaterial(unselectedText).color, tokens.sidebarAccent);
      expect(tileMaterial(selectedText).color, tokens.sidebarAccent);

      await gesture.moveTo(Offset.zero);
      await tester.pumpAndSettle();
      expect(tileMaterial(unselectedText).color, Colors.transparent);
      expect(tileMaterial(selectedText).color, tokens.sidebarAccent);
    });
  }
}

WorkSummary _work(String id, String sessionId, String title) => WorkSummary(
  id: id,
  mainSessionId: sessionId,
  title: title,
  project: '/tmp/project',
  sessionCount: 1,
  messageCount: 1,
  lastMessageAt: 1,
);
