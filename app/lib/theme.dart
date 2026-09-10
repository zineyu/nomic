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
    required this.tertiary,
    required this.border,
    required this.borderStrong,
    required this.destructive,
    required this.success,
    required this.warning,
    required this.accent,
    required this.accentSoft,
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

  /// 次级表面（hover 填充）。
  final Color secondary;
  final Color muted;

  /// 次级文字（时间戳 / ledger / 副标题）。
  final Color mutedForeground;

  /// 三级文字（占位符 / 极弱提示）。
  final Color tertiary;

  final Color border;

  /// 强调边线（输入框聚焦外框、表格边框等需要更清晰分界的场合）。
  final Color borderStrong;

  final Color destructive;
  final Color success;

  /// 警告色：仅 token 用量接近上限等「需要注意」场景。
  final Color warning;

  /// 品牌蓝：选中强调条、未读圆点、focus ring、启用态主按钮。
  /// 点状/线状使用，不作大面填充。
  final Color accent;

  /// 品牌蓝的 8% 底（选中填充、accent 元素 hover 底）。
  final Color accentSoft;

  /// 用户消息纸块底色（浅灰，区别于画布白；无边框无阴影）。
  final Color bubble;

  final Color sidebar;
  final Color sidebarAccent;
  final Color sidebarBorder;

  /// light：DESIGN.md front matter。
  static const light = NomicTokens(
    background: Color(0xFFFFFFFF),
    foreground: Color(0xFF1F1F1C),
    card: Color(0xFFFFFFFF),
    primary: Color(0xFF1F1F1C),
    primaryForeground: Color(0xFFFFFFFF),
    secondary: Color(0xFFF1F1EE),
    muted: Color(0xFFF1F1EE),
    mutedForeground: Color(0xFF6F6F6A),
    tertiary: Color(0xFF9A9A94),
    border: Color(0xFFE7E5DF),
    borderStrong: Color(0xFFD8D6D0),
    destructive: Color(0xFFDC2626),
    success: Color(0xFF16A34A),
    warning: Color(0xFFD97706),
    accent: Color(0xFF3B82F6),
    accentSoft: Color(0xFFEAF2FF),
    bubble: Color(0xFFF4F4F2),
    sidebar: Color(0xFFF7F7F5),
    sidebarAccent: Color(0xFFECECE8),
    sidebarBorder: Color(0xFFE7E5DF),
  );

  /// dark：色阶反转（token 结构相同；accent 提亮保持暗底可辨识）。
  static const dark = NomicTokens(
    background: Color(0xFF161615),
    foreground: Color(0xFFECECE8),
    card: Color(0xFF1C1C1A),
    primary: Color(0xFFECECE8),
    primaryForeground: Color(0xFF1F1F1C),
    secondary: Color(0xFF242422),
    muted: Color(0xFF242422),
    mutedForeground: Color(0xFF9A9A94),
    tertiary: Color(0xFF6F6F6A),
    border: Color(0xFF2C2C29),
    borderStrong: Color(0xFF3A3A36),
    destructive: Color(0xFFE85D52),
    success: Color(0xFF3FB970),
    warning: Color(0xFFE09543),
    accent: Color(0xFF60A5FA),
    accentSoft: Color(0xFF1C2B4A),
    bubble: Color(0xFF232321),
    sidebar: Color(0xFF1A1A18),
    sidebarAccent: Color(0xFF262624),
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
/// 字号阶梯：h1 24 / h2 20 / h3 18 / body 16 / prose 15 / bodySm 14 /
/// ui 13 / caption 12；正文阅读（prose）行高 1.7，heading 1.2–1.4、
/// UI 文字 1.5；颜色由调用方按语义给（foreground / mutedForeground /
/// tertiary / destructive…）。正文字族走平台默认（macOS 上即
/// .AppleSystemUIFont，中文由系统 PingFang SC 回退覆盖）。
abstract final class AppText {
  static TextStyle h1(Color? color) => TextStyle(
    fontSize: 24,
    height: 1.3,
    fontWeight: FontWeight.w700,
    color: color,
  );

  static TextStyle h2(Color? color) => TextStyle(
    fontSize: 20,
    height: 1.35,
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

  /// 技术回答正文（markdown 段落）：15px / 1.7 行高，长时间阅读档。
  static TextStyle prose(Color? color) =>
      TextStyle(fontSize: 15, height: 1.7, color: color);

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
