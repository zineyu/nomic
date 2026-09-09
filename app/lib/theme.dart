/// 视觉 token：单一事实来源为仓库根 `DESIGN.md`（本文件与其同步；
/// 原 web 前端的 `web/src/index.css` `@theme` 已随 ADR-0046 移除）。
///
/// 色彩为纯灰阶（oklch chroma 0）+ 两个功能色（destructive / success）；
/// 强调机制是明暗反转（primary = ink），不是色相。Dark mode 反转整条色阶。
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
  final Color sidebar;
  final Color sidebarAccent;
  final Color sidebarBorder;

  /// light：DESIGN.md front matter（oklch 值换算为 sRGB 灰阶）。
  static const light = NomicTokens(
    background: Color(0xFFFFFFFF), // oklch(1 0 0)
    foreground: Color(0xFF141414), // oklch(0.19 0 0)
    card: Color(0xFFFFFFFF),
    primary: Color(0xFF141414),
    primaryForeground: Color(0xFFFAFAFA), // oklch(0.98 0 0)
    secondary: Color(0xFFF0F0F0), // oklch(0.955 0 0)
    muted: Color(0xFFF0F0F0),
    mutedForeground: Color(0xFF636363), // oklch(0.5 0 0)
    border: Color(0xFFE3E3E3), // oklch(0.915 0 0)
    destructive: Color(0xFFB8433A), // oklch(0.5 0.17 27)
    success: Color(0xFF2F8F5B), // oklch(0.52 0.11 155)
    sidebar: Color(0xFFF7F7F7), // oklch(0.975 0 0)
    sidebarAccent: Color(0xFFEBEBEB), // oklch(0.94 0 0)
    sidebarBorder: Color(0xFFE3E3E3),
  );

  /// dark：DESIGN.md「Dark Mode」表（色阶反转，token 结构相同）。
  static const dark = NomicTokens(
    background: Color(0xFF0D0D0D), // oklch(0.16 0 0)
    foreground: Color(0xFFEEEEEE), // oklch(0.95 0 0)
    card: Color(0xFF141414), // oklch(0.19 0 0)
    primary: Color(0xFFEEEEEE),
    primaryForeground: Color(0xFF141414),
    secondary: Color(0xFF1D1D1D), // oklch(0.23 0 0)
    muted: Color(0xFF1D1D1D),
    mutedForeground: Color(0xFF8F8F8F), // oklch(0.65 0 0)
    border: Color(0x1AFFFFFF), // oklch(1 0 0 / 10%)
    destructive: Color(0xFFD95D54), // oklch(0.62 0.17 25)
    success: Color(0xFF4FC08D), // oklch(0.68 0.12 155)
    sidebar: Color(0xFF121212), // oklch(0.18 0 0)
    sidebarAccent: Color(0xFF212121), // oklch(0.25 0 0)
    sidebarBorder: Color(0x1AFFFFFF),
  );
}

/// 间距 token（8/16/24/32；DESIGN.md「Proportion and Rhythm」）。
abstract final class Spacing {
  static const double sm = 8;
  static const double md = 16;
  static const double lg = 24;
  static const double xl = 32;
}

/// 圆角 token（4/6/8/12/full）。
abstract final class Radii {
  static const double sm = 4;
  static const double md = 6;
  static const double lg = 8;
  static const double xl = 12;
}

/// 页面与消息流共享的列宽（920px；不引入新列宽）。
const double maxPageWidth = 920;

/// 文本样式 token（DESIGN.md typography 的 Dart 表达）。
///
/// 字号阶梯：h1 36 / h2 30 / h3 24 / body 16 / bodySm 14 / ui 13 / caption 12，
/// 行高 heading 1.2–1.4、正文 1.5；颜色由调用方按语义给（foreground /
/// mutedForeground / destructive…）。正文字族走平台默认（即 DESIGN 字体栈
/// 中的 system-ui 档位；未打包 Noto Sans，中文由系统 CJK 回退覆盖）。
abstract final class AppText {
  static TextStyle h1(Color? color) => TextStyle(
    fontSize: 36,
    height: 1.2,
    fontWeight: FontWeight.w700,
    color: color,
  );

  static TextStyle h2(Color? color) => TextStyle(
    fontSize: 30,
    height: 1.3,
    fontWeight: FontWeight.w600,
    color: color,
  );

  static TextStyle h3(Color? color) => TextStyle(
    fontSize: 24,
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

/// 等宽字体（代码块 / 工具参数 / token 计数）：跨平台按序回退，
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
