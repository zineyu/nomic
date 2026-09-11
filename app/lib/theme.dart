/// 视觉 token：单一事实来源为仓库根 `DESIGN.md`（DeepSeek Harness 设计系统，
/// 本文件与其同步）。
///
/// 三层架构的 Dart 表达：DESIGN.md front matter 的语义色板（neutral-bluish
/// 冷灰 + 唯一 DeepSeek 蓝）按 light/dark 成对定义在 [NomicTokens]；圆角、
/// 阴影、动效、文本阶梯为静态 token（主题不变或成对重绑定）。特性组件只消费
/// 这里的语义 token，不写字面颜色；token 表之外的值取最近的语义 token。
///
/// 设计语言：内容优先的安静工作台——层级来自字重、中性分层与 0.5px 发丝线，
/// 不用色块与重阴影；DeepSeek 蓝是「点」不是「面」，只出现在链接、info 按钮与
/// 业务状态指示（运行/未读圆点）；dark mode 反转整条色阶（#151517 →
/// #232324 → #2C2C2E → #353638 抬升），蓝提亮保持可辨识。
library;

import 'package:flutter/material.dart';

/// DESIGN.md colors 的 Dart 表达（light / dark 成对；值与 front matter 一致）。
class NomicTokens {
  const NomicTokens({
    required this.background,
    required this.foreground,
    required this.primary,
    required this.primaryForeground,
    required this.primaryDimmed,
    required this.secondary,
    required this.tertiary,
    required this.caption,
    required this.card,
    required this.inputMajor,
    required this.tip,
    required this.surfaceOverlay,
    required this.codeSurface,
    required this.bubble,
    required this.border,
    required this.borderStrong,
    required this.sidebarBorder,
    required this.error,
    required this.success,
    required this.warning,
    required this.business,
    required this.sidebar,
    required this.sidebarHover,
    required this.sidebarActive,
    required this.toastBg,
    required this.tooltipBg,
  });

  /// 画布底色（dark：#151517 最底层）。
  final Color background;

  /// 主文字墨色（light 近黑 #0F1115；dark 近白 #F9FAFB——反转即强调）。
  final Color foreground;

  /// Primary ink：主按钮填充 / 品牌标记（与 foreground 同值，语义独立）。
  final Color primary;

  final Color primaryForeground;

  /// Dimmed primary：浅色填充按钮 / 行 hover 的中性填充（ghost-active-fill）。
  final Color primaryDimmed;

  /// 次级文字（时间戳 / 摘要 / 副标题；#61666B / #CFD3D6）。
  final Color secondary;

  /// 三级文字（图标 / 弱提示；#81858C / #ADB2B8）。
  final Color tertiary;

  /// 最弱文字（占位符 / 禁用；#ADB2B8 / #81858C）。
  final Color caption;

  /// 卡片表面（surface-layer-1：light 全白靠发丝线分层，dark #232324）。
  final Color card;

  /// composer / 主输入框表面（surface specific `input-major`）。
  final Color inputMajor;

  /// 提示条 / 状态条底（surface specific `tip`）。
  final Color tip;

  /// 抬升覆盖层（`surface-overlay`；亦作 ghost 按钮 hover 加深档）。
  final Color surfaceOverlay;

  /// 代码块 / 行内 code / 表格条纹底（markdown-code-block）。
  final Color codeSurface;

  /// 用户消息气泡底（specific `bubble`：light 浅蓝 #EDF3FE，dark #2C2C2E）。
  final Color bubble;

  /// 发丝线分隔（border-l1，0.5px 档；divider / 行分隔）。
  final Color border;

  /// 强调边线（border-l3；输入框、表格、描边控件的可见分界）。
  final Color borderStrong;

  /// 侧栏与内容区的分界（border-l2）。
  final Color sidebarBorder;

  final Color error;
  final Color success;

  /// 仅 token 用量接近上限等「需要注意」场景。
  final Color warning;

  /// DeepSeek 蓝（`business` / `link` / button-info-fill：#4176E6 / #679EFE）。
  /// 点状/线状使用（链接、info 按钮、运行/未读指示、focus ring），不作大面填充。
  final Color business;

  /// 侧栏底（specific `sidebar-fill`）。
  final Color sidebar;

  /// 侧栏条目 hover 底（sidebar-nav-item-hover）。
  final Color sidebarHover;

  /// 侧栏条目选中底（sidebar-nav-item-active）。
  final Color sidebarActive;

  /// Toast 底（#353638 / #43454A，白字）。
  final Color toastBg;

  /// Tooltip 底（#2C2C2E / #43454A，白字）。
  final Color tooltipBg;

  /// light：DESIGN.md front matter 原值。
  static const light = NomicTokens(
    background: Color(0xFFFFFFFF),
    foreground: Color(0xFF0F1115),
    primary: Color(0xFF0F1115),
    primaryForeground: Color(0xFFFFFFFF),
    primaryDimmed: Color(0xFFEBEEF2),
    secondary: Color(0xFF61666B),
    tertiary: Color(0xFF81858C),
    caption: Color(0xFFADB2B8),
    card: Color(0xFFFFFFFF),
    inputMajor: Color(0xFFFFFFFF),
    tip: Color(0xFFF9FAFB),
    surfaceOverlay: Color(0xFFE9ECF2),
    codeSurface: Color(0xFFF9FAFB),
    bubble: Color(0xFFEDF3FE),
    border: Color(0x0A000000),
    borderStrong: Color(0x1F000000),
    sidebarBorder: Color(0x1A000000),
    error: Color(0xFFEC1313),
    success: Color(0xFF22C55E),
    warning: Color(0xFFF59E0B),
    business: Color(0xFF4176E6),
    sidebar: Color(0xFFF9FAFB),
    sidebarHover: Color(0xFFF1F3F5),
    sidebarActive: Color(0xFFEBEEF2),
    toastBg: Color(0xFF353638),
    tooltipBg: Color(0xFF2C2C2E),
  );

  /// dark：`body[data-ds-dark-theme]` 覆盖值（accent 提亮、表面抬升）。
  static const dark = NomicTokens(
    background: Color(0xFF151517),
    foreground: Color(0xFFF9FAFB),
    primary: Color(0xFFF9FAFB),
    primaryForeground: Color(0xFF0F1115),
    primaryDimmed: Color(0xFF43454A),
    secondary: Color(0xFFCFD3D6),
    tertiary: Color(0xFFADB2B8),
    caption: Color(0xFF81858C),
    card: Color(0xFF232324),
    inputMajor: Color(0xFF2C2C2E),
    tip: Color(0xFF353638),
    surfaceOverlay: Color(0xFF61666B),
    codeSurface: Color(0xFF1B1B1C),
    bubble: Color(0xFF2C2C2E),
    border: Color(0x0FFFFFFF),
    borderStrong: Color(0x29FFFFFF),
    sidebarBorder: Color(0x1FFFFFFF),
    error: Color(0xFFF25A5A),
    success: Color(0xFF22C55E),
    warning: Color(0xFFF59E0B),
    business: Color(0xFF679EFE),
    sidebar: Color(0xFF1B1B1C),
    sidebarHover: Color(0xFF2C2C2E),
    sidebarActive: Color(0xFF43454A),
    toastBg: Color(0xFF43454A),
    tooltipBg: Color(0xFF43454A),
  );
}

/// 间距 token（8/16/24/32）。DESIGN.md 刻意不定义间距体系（上游 omitted：
/// spacing——布局节奏由组件自持），此处为组件层约定，非 DESIGN.md token。
abstract final class Spacing {
  static const double sm = 8;
  static const double md = 16;
  static const double lg = 24;
  static const double xl = 32;
}

/// 圆角 token（DESIGN.md `rounded`：4/6/8/10/12/16 + pill 999）。
abstract final class Radii {
  static const double sm = 4;
  static const double md = 6;
  static const double lg = 8;
  static const double xl = 10;
  static const double xxl = 12;
  static const double xxxl = 16;
  static const double full = 999;
}

/// 页面与消息流共享的列宽（组件层约定；DESIGN.md 未定义布局宽度）。
const double maxPageWidth = 760;

/// 海拔阴影（DESIGN.md「Elevation & Depth」）。
///
/// 抬升面（菜单 / popover / dialog / 浮动按钮 / composer）`border: 0`，取
/// stroke + panel/prominent/soft 三档之一；首层为 0.5px 描边（颜色按表面
/// 重绑定，默认 border-l4）。中性 border 与 elevation 阴影不叠加——stroke
/// 即边框。CSS `0 0 0 0.5px` 描边以 spreadRadius 0.5 近似。
abstract final class AppShadows {
  /// 0.5px 描边层（颜色由调用方按表面给定）。
  static BoxShadow stroke(Color color) =>
      BoxShadow(color: color, blurRadius: 0, spreadRadius: 0.5);

  /// `panel`：菜单 / popover / 面板。
  static List<BoxShadow> panel({Color? strokeColor}) => [
    if (strokeColor != null) stroke(strokeColor),
    const BoxShadow(
      color: Color(0x08000000),
      blurRadius: 8,
      offset: Offset(0, 3),
    ),
    const BoxShadow(color: Color(0x05000000), blurRadius: 16),
  ];

  /// `prominent`：dialog 等需要更强存在感的浮层。
  static List<BoxShadow> prominent({Color? strokeColor}) => [
    if (strokeColor != null) stroke(strokeColor),
    const BoxShadow(
      color: Color(0x0A000000),
      blurRadius: 8,
      offset: Offset(0, 3),
    ),
    const BoxShadow(color: Color(0x0D000000), blurRadius: 20),
  ];

  /// `soft`：composer 专用（`0 4px 16px / 0 0 24px`，alpha 0.03）。
  static List<BoxShadow> soft({Color? strokeColor}) => [
    if (strokeColor != null) stroke(strokeColor),
    const BoxShadow(
      color: Color(0x08000000),
      blurRadius: 16,
      offset: Offset(0, 4),
    ),
    const BoxShadow(color: Color(0x08000000), blurRadius: 24),
  ];
}

/// 动效（DESIGN.md「Motion」）：所有过渡统一
/// `cubic-bezier(0.4, 0, 0.2, 1)`，时长只取 100/200/300ms 三档。
abstract final class AppMotion {
  static const fast = Duration(milliseconds: 100);
  static const normal = Duration(milliseconds: 200);
  static const slow = Duration(milliseconds: 300);
  static const curve = Cubic(0.4, 0, 0.2, 1);
}

/// 文本样式 token（DESIGN.md typography 的 Dart 表达）。
///
/// 两条阶梯：markdown 内容档（h1–h4 / body / table / small，默认 14px 内容
/// 字号下的值）与 UI 档（xl-24 … xxxs-11，各有 strong 变体）。每个字号配对
/// 行高；颜色由调用方按语义给（foreground / secondary / tertiary /
/// caption / error…）。正文字族走平台默认（macOS 上即 .AppleSystemUIFont，
/// 中文由系统 PingFang SC 回退覆盖）。
abstract final class AppText {
  // ── Markdown 内容阶梯 ────────────────────────────────────────────────────

  static TextStyle h1(Color? color) => _style(21, 30, FontWeight.w700, color);

  static TextStyle h2(Color? color) => _style(19, 28, FontWeight.w700, color);

  static TextStyle h3(Color? color) => _style(18, 26, FontWeight.w700, color);

  static TextStyle h4(Color? color) => _style(14, 24, FontWeight.w600, color);

  /// markdown 正文（默认档 14/24）。
  static TextStyle body(Color? color) => _style(14, 24, FontWeight.w400, color);

  static TextStyle bodyStrong(Color? color) =>
      _style(14, 24, FontWeight.w600, color);

  static TextStyle table(Color? color) =>
      _style(13, 22, FontWeight.w400, color);

  static TextStyle tableHead(Color? color) =>
      _style(13, 22, FontWeight.w500, color);

  static TextStyle small(Color? color) =>
      _style(12, 20, FontWeight.w400, color);

  static TextStyle smallStrong(Color? color) =>
      _style(12, 20, FontWeight.w600, color);

  /// 技术回答正文（markdown 段落档，长时间阅读）。
  static TextStyle prose(Color? color) => body(color);

  // ── UI 阶梯（xl-24 … xxxs-11 + strong）──────────────────────────────────

  /// 页面标题档（xl-24：24/32，semibold）。
  static TextStyle xl(Color? color) => _style(24, 32, FontWeight.w600, color);

  static TextStyle l(Color? color) => _style(20, 28, FontWeight.w500, color);

  static TextStyle m(Color? color) => _style(16, 28, FontWeight.w500, color);

  /// dialog 标题 / 区块正文档（base-16）。
  static TextStyle base(Color? color) => _style(16, 24, FontWeight.w400, color);

  static TextStyle baseStrong(Color? color) =>
      _style(16, 24, FontWeight.w500, color);

  static TextStyle s(Color? color) => _style(14, 22, FontWeight.w400, color);

  static TextStyle sStrong(Color? color) =>
      _style(14, 22, FontWeight.w500, color);

  /// 列表条目 / 控件文字档（xs-13）。
  static TextStyle xs(Color? color) => _style(13, 20, FontWeight.w400, color);

  static TextStyle xsStrong(Color? color) =>
      _style(13, 20, FontWeight.w500, color);

  /// 辅助说明档（xxs-12：时间戳 / 计数 / 标签）。
  static TextStyle xxs(Color? color) => _style(12, 18, FontWeight.w400, color);

  static TextStyle xxsStrong(Color? color) =>
      _style(12, 18, FontWeight.w500, color);

  static TextStyle xxxs(Color? color) => _style(11, 14, FontWeight.w400, color);

  static TextStyle xxxsStrong(Color? color) =>
      _style(11, 14, FontWeight.w500, color);

  static TextStyle _style(
    double fontSize,
    double lineHeight,
    FontWeight weight,
    Color? color,
  ) => TextStyle(
    fontSize: fontSize,
    height: lineHeight / fontSize,
    fontWeight: weight,
    color: color,
  );
}

/// 等宽字体（代码块 / 工具参数 / 耗时与计数）：DESIGN.md code 字栈
/// （SF Mono / JetBrains Mono / Fira Code / Consolas / Liberation Mono /
/// Menlo / Courier），刻意不用裸 `monospace` 族名（Windows CJK 不至回退
/// SimSun）。行内 code 12/19，code block 11/19。
abstract final class AppFonts {
  static const monoFallback = [
    'JetBrains Mono',
    'Fira Code',
    'Consolas',
    'Liberation Mono',
    'Menlo',
    'Courier',
  ];

  static TextStyle mono({required double fontSize, required Color color}) =>
      TextStyle(
        fontSize: fontSize,
        height: fontSize == 12
            ? 19 / 12
            : fontSize == 11
            ? 19 / 11
            : 1.5,
        fontFamily: 'SF Mono',
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
    secondary: tokens.business,
    onSecondary: Colors.white,
    error: tokens.error,
    onError: Colors.white,
    surface: tokens.background,
    onSurface: tokens.foreground,
  );
  return ThemeData(
    useMaterial3: true,
    colorScheme: scheme,
    scaffoldBackgroundColor: tokens.background,
    dividerColor: tokens.border,
    cardColor: tokens.card,
    // overlay 组件走 token：dialog 圆角 2xl(12)（DESIGN.md card 组件），
    // 背景 surface-layer-1
    dialogTheme: DialogThemeData(
      backgroundColor: tokens.card,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(Radii.xxl),
      ),
    ),
    // toast：toast-bg + 白字，圆角 lg（DESIGN.md toast 组件）
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      backgroundColor: tokens.toastBg,
      contentTextStyle: AppText.xxs(Colors.white),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(Radii.lg),
      ),
    ),
    tooltipTheme: TooltipThemeData(
      decoration: BoxDecoration(
        color: tokens.tooltipBg,
        borderRadius: BorderRadius.circular(Radii.md),
      ),
      textStyle: AppText.xxs(Colors.white),
      padding: const EdgeInsets.symmetric(horizontal: Spacing.sm, vertical: 4),
    ),
    // DESIGN.md「Scrollbars」：透明轨道、8px 拇指；拇指色取 static neutral
    // 阶（主题不变）：light neutral-200 / hover neutral-300，
    // dark neutral-700 / hover neutral-600。
    scrollbarTheme: ScrollbarThemeData(
      thumbColor: WidgetStateProperty.resolveWith((states) {
        final hovered = states.contains(WidgetState.hovered);
        if (dark) {
          return hovered ? const Color(0xFF545557) : const Color(0xFF3C3C3D);
        }
        return hovered ? const Color(0xFFD4D4D4) : const Color(0xFFE5E5E5);
      }),
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: tokens.inputMajor,
      // placeholder 语义：tertiary + xs 字号，弱于输入正文；不显式给时
      // Flutter 默认回落到 bodyLarge + onSurfaceVariant，即黑而大
      hintStyle: AppText.xs(tokens.tertiary),
      contentPadding: const EdgeInsets.symmetric(
        horizontal: Spacing.md,
        vertical: Spacing.sm,
      ),
      // 圆角 lg(8)：DESIGN.md `input-major` 组件绑定（rounded.lg）
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.lg),
        borderSide: BorderSide(color: tokens.borderStrong),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.lg),
        borderSide: BorderSide(color: tokens.borderStrong),
      ),
      // focus ring：DeepSeek 蓝（DESIGN.md：蓝用于交互态指示）
      focusedBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(Radii.lg),
        borderSide: BorderSide(color: tokens.business),
      ),
    ),
  );
}

/// 经 context 访问 token（挂在 ThemeExtension 上太重，直接用静态 + brightness）。
NomicTokens tokensOf(BuildContext context) =>
    Theme.of(context).brightness == Brightness.dark
    ? NomicTokens.dark
    : NomicTokens.light;
