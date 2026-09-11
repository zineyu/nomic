/// 微动画原语：全界面共享的三类小动作（DESIGN.md「Motion」——统一
/// `AppMotion.curve`，时长只取 100/200/300ms 三档）。
///
/// - [FadeSlideIn]：一次性进入动画（淡入 + ≤8px 上浮），用于消息项、
///   面板、空态引导等「新内容出现」；
/// - [AnimatedReveal]：可见性收展（高度 + 透明度），用于横幅、执行卡片
///   详情等「就地展开/收起」，退出动画期间子树保持挂载；
/// - [AnimatedChevron]：展开态箭头旋转（right ↔ down，90° 连续转动）。
///
/// 所有动画尊重系统减弱动效（`MediaQuery.disableAnimations`）：进入与收展
/// 动画直接跳到终态，不做运动。
library;

import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:lucide_icons/lucide_icons.dart';

import '../theme.dart';

/// 一次性进入动画：淡入 + 轻微上浮（绘制期 transform，不改变布局尺寸，
/// 列表滚动度量不受入场位移影响）。State 随 widget key 存活，同一条目
/// 重建不重复播放；减弱动效下直接呈现终态。
class FadeSlideIn extends StatefulWidget {
  const FadeSlideIn({super.key, required this.child, this.offsetY = 8});

  final Widget child;

  /// 入场位移（px，向上为正）。
  final double offsetY;

  @override
  State<FadeSlideIn> createState() => _FadeSlideInState();
}

class _FadeSlideInState extends State<FadeSlideIn>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: AppMotion.normal,
  );

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    if (MediaQuery.of(context).disableAnimations) {
      _controller.value = 1;
    } else if (_controller.isDismissed) {
      _controller.forward();
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final animation = CurvedAnimation(
      parent: _controller,
      curve: AppMotion.curve,
    );
    return AnimatedBuilder(
      animation: animation,
      builder: (context, child) => Opacity(
        opacity: animation.value,
        child: Transform.translate(
          offset: Offset(0, (1 - animation.value) * widget.offsetY),
          child: child,
        ),
      ),
      child: widget.child,
    );
  }
}

/// 可见性收展：透明度 + 高度同步过渡（顶部对齐）。子树在退出动画期间保持
/// 挂载（内容方可安全地继续读取已失效的上游状态——由调用方缓存最后值）；
/// 完全收起后子树退出树，不占布局。减弱动效下瞬时切换。
class AnimatedReveal extends StatefulWidget {
  const AnimatedReveal({
    super.key,
    required this.visible,
    required this.child,
    this.duration = AppMotion.normal,
  });

  final bool visible;
  final Widget child;

  /// 收展时长（默认 normal/200ms；小幅收展可用 fast/100ms）。
  final Duration duration;

  @override
  State<AnimatedReveal> createState() => _AnimatedRevealState();
}

class _AnimatedRevealState extends State<AnimatedReveal>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: widget.duration,
    value: widget.visible ? 1 : 0,
  );

  @override
  void didUpdateWidget(AnimatedReveal oldWidget) {
    super.didUpdateWidget(oldWidget);
    _controller.duration = widget.duration;
    if (widget.visible == oldWidget.visible) return;
    if (MediaQuery.of(context).disableAnimations) {
      _controller.value = widget.visible ? 1 : 0;
    } else if (widget.visible) {
      _controller.forward();
    } else {
      _controller.reverse();
    }
  }

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    if (_controller.isDismissed) return const SizedBox.shrink();
    final curved = CurvedAnimation(parent: _controller, curve: AppMotion.curve);
    return ClipRect(
      child: SizeTransition(
        sizeFactor: curved,
        alignment: Alignment.topCenter,
        child: FadeTransition(opacity: curved, child: widget.child),
      ),
    );
  }
}

/// 展开态箭头：chevronRight 连续旋转 90° 表示展开（替代图标硬切）。
class AnimatedChevron extends StatelessWidget {
  const AnimatedChevron({
    super.key,
    required this.expanded,
    required this.color,
    this.size = 12,
  });

  final bool expanded;
  final Color color;
  final double size;

  @override
  Widget build(BuildContext context) {
    return TweenAnimationBuilder<double>(
      tween: Tween(begin: 0, end: expanded ? 0.25 : 0),
      duration: AppMotion.fast,
      curve: AppMotion.curve,
      builder: (context, turns, child) =>
          Transform.rotate(angle: turns * 2 * math.pi, child: child),
      child: Icon(LucideIcons.chevronRight, size: size, color: color),
    );
  }
}

/// hover 驱动的颜色过渡（100ms）：行底色 / 图标色等需要平滑切换颜色、
/// 又不便用 AnimatedContainer 的场景（如 Material.color、Icon.color）。
class AnimatedColor extends StatelessWidget {
  const AnimatedColor({
    super.key,
    required this.color,
    required this.builder,
    this.duration = AppMotion.fast,
  });

  final Color color;
  final Duration duration;

  /// 以当前插值颜色构建子树。
  final Widget Function(BuildContext context, Color color) builder;

  @override
  Widget build(BuildContext context) {
    return TweenAnimationBuilder<Color?>(
      tween: ColorTween(end: color),
      duration: duration,
      curve: AppMotion.curve,
      builder: (context, value, _) => builder(context, value ?? color),
    );
  }
}
