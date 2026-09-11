/// 24px 迷你图标按钮：列表行内 / 区块标题右侧的操作位（新建、删除等）。
///
/// 视觉反馈走无色相前景阶梯（hover 时图标 muted → foreground），不引入
/// 底色——按钮常出现在已有 hover 底色的行上，再叠底色反而糊掉层级。
library;

import 'package:flutter/material.dart';

import '../theme.dart';
import 'animations.dart';

class MiniIconButton extends StatefulWidget {
  const MiniIconButton({
    super.key,
    required this.icon,
    required this.tooltip,
    required this.onTap,
  });

  final IconData icon;
  final String tooltip;
  final VoidCallback onTap;

  @override
  State<MiniIconButton> createState() => _MiniIconButtonState();
}

class _MiniIconButtonState extends State<MiniIconButton> {
  bool _hovered = false;

  @override
  Widget build(BuildContext context) {
    final tokens = tokensOf(context);
    return Tooltip(
      message: widget.tooltip,
      child: MouseRegion(
        cursor: SystemMouseCursors.click,
        onEnter: (_) => setState(() => _hovered = true),
        onExit: (_) => setState(() => _hovered = false),
        child: GestureDetector(
          onTap: widget.onTap,
          behavior: HitTestBehavior.opaque,
          // 前景色随 hover 100ms 过渡（muted → foreground，不闪现）
          child: SizedBox(
            width: 24,
            height: 24,
            child: AnimatedColor(
              color: _hovered ? tokens.foreground : tokens.secondary,
              builder: (context, color) =>
                  Icon(widget.icon, size: 14, color: color),
            ),
          ),
        ),
      ),
    );
  }
}
