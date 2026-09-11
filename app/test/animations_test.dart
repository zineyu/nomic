/// 微动画原语回归：
/// - AnimatedColor 走预乘 alpha 溶解插值：经透明端点的过渡 RGB 保持端点
///   色、只有 alpha 变化（直通道插值的中间帧是半透明深灰，视觉上闪黑）；
/// - 不透明端点间的过渡中间帧保持不透明。
/// - AnimatedReveal 收起→展开由动画自身驱动重建（不依赖外部 setState）。
library;

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:nomic_app/ui/animations.dart';

void main() {
  testWidgets('AnimatedColor 经透明端点的过渡不经过深灰中间态', (tester) async {
    Color? seen;
    Widget frame(Color end) => Directionality(
      textDirection: TextDirection.ltr,
      child: AnimatedColor(
        color: end,
        builder: (context, color) {
          seen = color;
          return ColoredBox(color: color);
        },
      ),
    );

    await tester.pumpWidget(frame(Colors.transparent));
    await tester.pumpWidget(frame(const Color(0xFFF1F3F5)));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50)); // 过渡中段
    // 溶解插值：RGB 保持端点色，只有 alpha 变化；
    // 直通道插值此处 RGB 会被拖向黑（闪黑根源）
    expect(seen!.r, closeTo(0xF1 / 255, 0.05));
    expect(seen!.a, greaterThan(0.1));
    expect(seen!.a, lessThan(1.0));
    await tester.pumpAndSettle();
    expect(seen, const Color(0xFFF1F3F5));
  });

  testWidgets('AnimatedColor 不透明端点间过渡，中间帧保持不透明', (tester) async {
    Color? seen;
    Widget frame(Color end) => Directionality(
      textDirection: TextDirection.ltr,
      child: AnimatedColor(
        color: end,
        builder: (context, color) {
          seen = color;
          return ColoredBox(color: color);
        },
      ),
    );

    await tester.pumpWidget(frame(const Color(0xFFF9FAFB)));
    await tester.pumpWidget(frame(const Color(0xFFF1F3F5)));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50)); // 100ms 过渡的中点
    expect(seen!.a, 1.0);
    await tester.pumpAndSettle();
    expect(seen, const Color(0xFFF1F3F5));
  });

  testWidgets('AnimatedReveal 收起→展开由动画自身驱动挂载子树', (tester) async {
    Widget frame(bool visible) => Directionality(
      textDirection: TextDirection.ltr,
      child: AnimatedReveal(
        visible: visible,
        child: const SizedBox(height: 40, child: Text('内容')),
      ),
    );

    await tester.pumpWidget(frame(false));
    expect(find.text('内容'), findsNothing);

    // 展开：didUpdateWidget 启动动画的那一帧控制器值仍为 0（isDismissed），
    // 若 build 据此返回 shrink 且无人监听动画，子树永远不会重新挂载。
    await tester.pumpWidget(frame(true));
    await tester.pump();
    await tester.pump(const Duration(milliseconds: 50));
    expect(find.text('内容'), findsOneWidget);
    await tester.pumpAndSettle();
    expect(find.text('内容'), findsOneWidget);
    expect(tester.getSize(find.text('内容')).height, 40);
  });
}
