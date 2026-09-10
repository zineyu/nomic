/// 视觉 token：单一事实来源为仓库根 `DESIGN.md`（本文件与其同步）。
///
/// 设计语言「叙事日志」（2025 重设计）：暖白纸面 + 墨色叙事 + 唯一的
/// 信号蓝（signal）标记活跃状态；用户消息是浅灰纸块而非墨块；强调靠
/// 字重与留白，色彩仅 signal / destructive / success 三个功能色。
/// Dark mode 反转整条色阶，signal 提亮保持可辨识。
library;

import 'package:flutter/material.dart';

/// DESIGN.md token 的 Dart 表达（light / dark 成对）。
class NomicTokens {
  const NomicTokens({
    required this.background,
    required this.foreground,
    required this.card,
    required this.primary,
    required this.primaryForeground,
    required this.secondary,
    required this.muted,
    required this.mutedForeground,
    required this.border,
    required this.destructive,
    required this.success,
    required this.signal,
    required this.bubble,
    required this.sidebar,
    required this.sidebarAccent,
    required this.sidebarBorder,
  });

  final Color background;
  final Color foreground;
  final Color card;
  final Color primary;
  final Color primaryForeground;
  final Color secondary;
  final Color muted;
  final Color mutedForeground;
  final Color border;
  final Color destructive;
  final Color success;

  /// 信号蓝：唯一彩色状态色，仅标记「正在发生 / 活跃」（运行圆点、
  /// 当前会话标记、活跃指示）。不用于装饰，不用于大面填充。
  final Color signal;

  /// 用户消息纸块底色（浅灰，区别于画布白；无边框无阴影）。
  final Color bubble;

  final Color sidebar;
  final Color sidebarAccent;
  final Color sidebarBorder;

  /// light：DESIGN.md front matter。
  static const light = NomicTokens(
    background: Color(0xFFFFFFFF),
    foreground: Color(0xFF1A1A1A),
    card: Color(0xFFFFFFFF),
    primary: Color(0xFF1A1A1A),
    primaryForeground: Color(0xFFFFFFFF),
    secondary: Color(0xFFF0F0EC),
    muted: Color(0xFFF0F0EC),
    mutedForeground: Color(0xFF8F959E),
    border: Color(0xFFE9E9E4),
    destructive: Color(0xFFB8433A),
    success: Color(0xFF2F8F5B),
    signal: Color(0xFF3B7FFF),
    bubble: Color(0xFFF4F4F2),
    sidebar: Color(0xFFF7F7F5),
    sidebarAccent: Color(0xFFECECE8),
    sidebarBorder: Color(0xFFE9E9E4),
  );

  /// dark：色阶反转（token 结构相同；signal 提亮保持暗底可辨识）。
  static const dark = NomicTokens(
    background: Color(0xFF161615),
    foreground: Color(0xFFECECE8),
    card: Color(0xFF1C1C1A),
    primary: Color(0xFFECECE8),
    primaryForeground: Color(0xFF1A1A1A),
    secondary: Color(0xFF242422),
    muted: Color(0xFF242422),
    mutedForeground: Color(0xFF9A9FA6),
    border: Color(0xFF2C2C29),
    destructive: Color(0xFFD95D54),
    success: Color(0xFF4FC08D),
    signal: Color(0xFF6B97FF),
    bubble: Color(0xFF232321),
    sidebar: Color(0xFF1A1A18),
    sidebarAccent: Color(0xFF242422),
    sidebarBorder: Color(0xFF2C2C29),
  );
}

/// 间距 token（8/16/24/32；DESIGN.md「Proportion and Rhythm」）。
abstract final class Spacing {
  static const double sm = 8;
  static const double md = 16;
  static const double lg = 24;
  static const double xl = 32;
}

/// 圆角 token（4/6/8/12/16/full）。xxl 为悬浮 composer 专用。
abstract final class Radii {
  static const double sm = 4;
  static const double md = 6;
  static const double lg = 8;
  static const double xl = 12;
  static const double xxl = 16;
}

/// 页面与消息流共享的列宽（760px；DESIGN.md「Proportion and Rhythm」）。
const double maxPageWidth = 760;

/// 文本样式 token（DESIGN.md typography 的 Dart 表达）。
///
/// 字号阶梯：h1 28 / h2 22 / h3 18 / body 16 / bodySm 14 / ui 13 / caption 12，
/// 行高 heading 1.2–1.4、正文 1.5；颜色由调用方按语义给（foreground /
/// mutedForeground / destructive…）。正文字族走平台默认（macOS 上即
/// .AppleSystemUIFont，中文由系统 PingFang SC 回退覆盖）。
abstract final class AppText {
  static TextStyle h1(Color? color) => TextStyle(
    fontSize: 28,
    height: 1.25,
    fontWeight: FontWeight.w700,
    color: color,
  );

  static TextStyle h2(Color? color) => TextStyle(
    fontSize: 22,
    height: 1.3,
    fontWeight: FontWeight.w600,
    color: color,
  );

  static TextStyle h3(Color? color) => TextStyle(
    fontSize: 18,
    height: 1.4,
    fontWeight: FontWeight.w600,
    color: color,
  );

  static TextStyle body(Color? color) =>
      TextStyle(fontSize: 16, height: 1.5, color: color);

  static TextStyle bodySm(Color? color) =>
      TextStyle(fontSize: 14, height: 1.5, color: color);

  static TextStyle ui(Color? color) =>
      TextStyle(fontSize: 13, height: 1.5, color: color);

  static TextStyle caption(Color? color) =>
      TextStyle(fontSize: 12, height: 1.5, color: color);
}

/// 等宽字体（代码块 / 工具参数 / 耗时与计数）：跨平台按序回退，
/// 不再使用不可靠的裸 `'monospace'` 族名。
abstract final class AppFonts {
  static const monoFallback = ['Menlo', 'Monaco', 'Consolas', 'Courier New'];

  static TextStyle mono({required double fontSize, required Color color}) =>
      TextStyle(
        fontSize: fontSize,
        height: 1.5,
        fontFamily: 'Menlo',
        fontFamilyFallback: monoFallback,
        color: color,
      );
}

/// 从 token 构建 Material 主题。
ThemeData buildTheme(NomicTokens tokens, {required bool dark}) {
  final scheme = ColorScheme(
    brightness: dark ? Brightness.dark : Brightness.light,
    primary: tokens.primary,
    onPrimary: tokens.primaryForeground,
    secondary: tokens.secondary,
    onSecondary: tokens.foreground,
    error: tokens.destructive,
    onError: tokens.primaryForeground,
    surface: tokens.background,
    onSurface: tokens.foreground,
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: tokens.background,
    dividerColor: tokens.border,
    cardColor: tokens.card,
    // overlay 组件走 token：dialog 圆角 xl（M3 默认 28 过大），背景 card
    dialogTheme: DialogThemeData(
      backgroundColor: tokens.card,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(Radii.xl),
      ),
    ),
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      backgroundColor: tokens.primary,
      contentTextStyle: AppText.ui(tokens.primaryForeground),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(Radii.md),
      ),
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: tokens.background,
      contentPadding: const EdgeInsets.symmetric(
        horizontal: Spacing.md,
        vertical: Spacing.sm,
      ),
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.md),
        borderSide: BorderSide(color: tokens.border),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.md),
        borderSide: BorderSide(color: tokens.border),
      ),
      focusedBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.md),
        borderSide: BorderSide(color: tokens.primary.withValues(alpha: 0.5)),
      ),
    ),
  );
}

/// 经 context 访问 token（挂在 ThemeExtension 上太重，直接用静态 + brightness）。
NomicTokens tokensOf(BuildContext context) =>
    Theme.of(context).brightness == Brightness.dark
    ? NomicTokens.dark
    : NomicTokens.light;
