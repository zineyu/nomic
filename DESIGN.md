---
version: alpha
name: DeepSeek Harness
description: >-
  Design tokens and styling rules extracted from deepseek-ai/deepseek-harness
  (packages/client/ui-theme/src/styles + docs/web-styling.md). Tokens follow a
  three-layer architecture: static palette scales (--dsw-static-*), semantic
  aliases (--dsw-alias-*, themed light/dark), and component-specific bindings
  (--dsw-specific-*). Feature components consume semantic aliases only; the
  token sheets are the sole color authority.
source: https://github.com/deepseek-ai/deepseek-harness
omitted:
  - section: spacing
    reason: >-
      The upstream theme defines no spacing scale; layout rhythm is
      component-owned and deliberately not invented here.
colors:
  # ---- Core semantic palette (light theme canonical values) ----
  primary: "#0F1115"
  primary-foreground: "#FFFFFF"
  primary-hover: "#43454A"
  primary-dimmed: "#EBEEF2"
  secondary: "#61666B"
  tertiary: "#81858C"
  caption: "#ADB2B8"
  neutral: "#F9FAFB"
  background: "#FFFFFF"
  foreground: "#0F1115"
  surface-layer-1: "#FFFFFF"
  surface-layer-2: "#FFFFFF"
  surface-layer-3: "#FFFFFF"
  surface-overlay: "#E9ECF2"
  surface-skeleton: "#0000000A"
  link: "#4176E6"
  error: "#EC1313"
  error-secondary: "#F25A5A"
  success: "#22C55E"
  success-secondary: "#4ED17E"
  success-tertiary: "#E6FAED"
  warning: "#F59E0B"
  warning-secondary: "#F7AD31"
  warning-tertiary: "#FEF5E7"
  warning-label: "#DD8629"
  business: "#4176E6"
  business-tertiary: "#E4EDFD"
  border-l1: "#0000000A"
  border-l2: "#0000001A"
  border-l3: "#0000001F"
  border-l4: "#00000029"
  interactive-hover: "#2631480F"
  interactive-hover-accent: "#26314824"
  interactive-active: "#2631481A"
  interactive-hover-danger: "#EC13130D"
  button-contrast-fill: "#61666B"
  button-elevated-fill: "#FFFFFF"
  button-floating-fill: "#FFFFFF"
  button-floating-hover: "#F1F3F5"
  button-ghost-active-fill: "#EBEEF2"
  button-ghost-active-hover: "#E9ECF2"
  button-ghost-active-border: "#979DA6"
  button-info-fill: "#4176E6"
  button-info-hover: "#679EFE"
  bubble: "#EDF3FE"
  bubble-highlight: "#D3E2FF"
  input-major: "#FFFFFF"
  sidebar-fill: "#F9FAFB"
  sidebar-nav-item-active: "#EBEEF2"
  sidebar-nav-item-active-accent: "#E4EDFD"
  sidebar-nav-item-hover: "#F1F3F5"
  selector: "#F9FAFB"
  menu: "#FFFFFF"
  tip: "#F9FAFB"
  toast-bg: "#353638"
  tooltip-bg: "#2C2C2E"
  markdown-code-block: "#F9FAFB"
  markdown-code-block-banner: "#F9FAFB"
  markdown-inline-code: "#FAFAFA"
  markdown-citation: "#EBEEF2"
  markdown-tag: "#F1F3F5"
  markdown-placeholder: "#F9FAFB"
  mask-1: "#0000003D"
  mask-2: "#0000001F"
  mask-3: "#0000007A"
  mask-photo: "#000000E0"
  mask-drop: "#FFFFFFB2"
  # ---- Dark theme overrides (body[data-ds-dark-theme]) ----
  primary-dark: "#F9FAFB"
  primary-foreground-dark: "#0F1115"
  primary-hover-dark: "#EBEEF2"
  primary-dimmed-dark: "#43454A"
  secondary-dark: "#CFD3D6"
  tertiary-dark: "#ADB2B8"
  caption-dark: "#81858C"
  background-dark: "#151517"
  foreground-dark: "#F9FAFB"
  surface-layer-1-dark: "#232324"
  surface-layer-2-dark: "#2C2C2E"
  surface-layer-3-dark: "#353638"
  surface-overlay-dark: "#61666B"
  surface-skeleton-dark: "#FFFFFF14"
  link-dark: "#679EFE"
  error-dark: "#F25A5A"
  error-secondary-dark: "#F25A5A"
  success-dark: "#22C55E"
  success-secondary-dark: "#4ED17E"
  success-tertiary-dark: "#233C2C"
  warning-dark: "#F59E0B"
  warning-secondary-dark: "#F7AD31"
  warning-tertiary-dark: "#27241F"
  warning-label-dark: "#DD8629"
  business-dark: "#679EFE"
  business-tertiary-dark: "#34415B"
  border-l1-dark: "#FFFFFF0F"
  border-l2-dark: "#FFFFFF1F"
  border-l3-dark: "#FFFFFF29"
  border-l4-dark: "#FFFFFF33"
  interactive-hover-dark: "#FFFFFF14"
  interactive-hover-accent-dark: "#FFFFFF3D"
  interactive-active-dark: "#FFFFFF24"
  interactive-hover-danger-dark: "#F25A5A26"
  button-contrast-fill-dark: "#F9FAFB"
  button-elevated-fill-dark: "#43454A"
  button-floating-fill-dark: "#2C2C2E"
  button-floating-hover-dark: "#353638"
  button-ghost-active-fill-dark: "#43454A"
  button-ghost-active-hover-dark: "#61666B"
  button-ghost-active-border-dark: "#81858C"
  button-info-fill-dark: "#679EFE"
  button-info-hover-dark: "#4176E6"
  bubble-dark: "#2C2C2E"
  bubble-highlight-dark: "#43454A"
  input-major-dark: "#2C2C2E"
  sidebar-fill-dark: "#1B1B1C"
  sidebar-nav-item-active-dark: "#43454A"
  sidebar-nav-item-active-accent-dark: "#353638"
  sidebar-nav-item-hover-dark: "#2C2C2E"
  selector-dark: "#353638"
  menu-dark: "#353638"
  tip-dark: "#353638"
  toast-bg-dark: "#43454A"
  tooltip-bg-dark: "#43454A"
  markdown-code-block-dark: "#1B1B1C"
  markdown-code-block-banner-dark: "#2C2C2E"
  markdown-inline-code-dark: "#292929"
  markdown-citation-dark: "#353638"
  markdown-tag-dark: "#2C2C2E"
  markdown-placeholder-dark: "#2C2C2E"
  mask-1-dark: "#00000080"
  mask-2-dark: "#00000033"
  mask-3-dark: "#0000007A"
  mask-photo-dark: "#000000E0"
  mask-drop-dark: "#272730B2"
  # ---- Static scales (theme-invariant) ----
  deepseek-50: "#EDF3FE"
  deepseek-100: "#E4EDFD"
  deepseek-200: "#D3E2FF"
  deepseek-300: "#B7C8FE"
  deepseek-400: "#679EFE"
  deepseek-450: "#5686FE"
  deepseek-500: "#4176E6"
  deepseek-600: "#4868B2"
  deepseek-800: "#34415B"
  deepseek-900: "#283142"
  blue-50: "#EFF6FF"
  blue-50p: "#EAF3FF"
  blue-75: "#E5F0FF"
  blue-100: "#DBEAFE"
  blue-300: "#93C5FD"
  blue-400: "#60A5FA"
  blue-450: "#4D93F8"
  blue-500: "#3B82F6"
  blue-600: "#2563EB"
  blue-800: "#1E40AF"
  blue-900: "#0E3074"
  blue-950: "#172554"
  neutral-bluish-00: "#FFFFFF"
  neutral-bluish-50: "#F9FAFB"
  neutral-bluish-60: "#F9FAFB"
  neutral-bluish-75: "#F1F3F5"
  neutral-bluish-100: "#EBEEF2"
  neutral-bluish-150: "#E9ECF2"
  neutral-bluish-200: "#E1E5EE"
  neutral-bluish-300: "#CFD3D6"
  neutral-bluish-400: "#ADB2B8"
  neutral-bluish-500: "#979DA6"
  neutral-bluish-600: "#81858C"
  neutral-bluish-700: "#61666B"
  neutral-bluish-750: "#43454A"
  neutral-bluish-800: "#353638"
  neutral-bluish-850: "#2C2C2E"
  neutral-bluish-875: "#232324"
  neutral-bluish-900: "#1B1B1C"
  neutral-bluish-950: "#151517"
  neutral-bluish-1000: "#0F1115"
  neutral-00: "#FFFFFF"
  neutral-50: "#FAFAFA"
  neutral-100: "#F5F5F5"
  neutral-150: "#EDEDED"
  neutral-200: "#E5E5E5"
  neutral-250: "#DCDCDC"
  neutral-300: "#D4D4D4"
  neutral-400: "#A2A4A6"
  neutral-500: "#7F8287"
  neutral-550: "#65676B"
  neutral-600: "#545557"
  neutral-700: "#3C3C3D"
  neutral-800: "#292929"
  neutral-850: "#212123"
  neutral-900: "#0F0F0F"
  neutral-1000: "#000000"
  red-50: "#FEF2F2"
  red-100: "#FEE2E2"
  red-400: "#F25A5A"
  red-500: "#EF4444"
  red-600: "#EC1313"
  red-900: "#570C0C"
  green-100: "#E6FAED"
  green-400: "#4ED17E"
  green-500: "#22C55E"
  green-900: "#233C2C"
  amber-100: "#FEF5E7"
  amber-400: "#F7AD31"
  amber-500: "#F59E0B"
  amber-600: "#DD8629"
  amber-900: "#27241F"
typography:
  h1:
    fontFamily: "{typography.body.fontFamily}"
    fontSize: 21px
    fontWeight: 700
    lineHeight: 30px
  h2:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 19px
    fontWeight: 700
    lineHeight: 28px
  h3:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 18px
    fontWeight: 700
    lineHeight: 26px
  h4:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 14px
    fontWeight: 600
    lineHeight: 24px
  body:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 14px
    fontWeight: 400
    lineHeight: 24px
  body-strong:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 14px
    fontWeight: 600
    lineHeight: 24px
  table:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 13px
    fontWeight: 400
    lineHeight: 22px
  table-head:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 13px
    fontWeight: 500
    lineHeight: 22px
  small:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 12px
    fontWeight: 400
    lineHeight: 20px
  small-strong:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 12px
    fontWeight: 600
    lineHeight: 20px
  code:
    fontFamily: "'SF Mono', 'JetBrains Mono', 'Fira Code', Consolas, 'Liberation Mono', Menlo, Courier, 'PingFang SC', 'Microsoft YaHei'"
    fontSize: 12px
    fontWeight: 400
    lineHeight: 19px
  code-block:
    fontFamily: "'SF Mono', 'JetBrains Mono', 'Fira Code', Consolas, 'Liberation Mono', Menlo, Courier, 'PingFang SC', 'Microsoft YaHei'"
    fontSize: 11px
    fontWeight: 400
    lineHeight: 19px
  code-block-small:
    fontFamily: "'SF Mono', 'JetBrains Mono', 'Fira Code', Consolas, 'Liberation Mono', Menlo, Courier, 'PingFang SC', 'Microsoft YaHei'"
    fontSize: 11px
    fontWeight: 400
    lineHeight: 16px
  xl-24:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 24px
    fontWeight: 600
    lineHeight: 32px
  l-20:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 20px
    fontWeight: 500
    lineHeight: 28px
  m-18:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 16px
    fontWeight: 500
    lineHeight: 28px
  base-16:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 16px
    fontWeight: 400
    lineHeight: 24px
  base-strong-16:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 16px
    fontWeight: 500
    lineHeight: 24px
  s-14:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 14px
    fontWeight: 400
    lineHeight: 22px
  s-strong-14:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 14px
    fontWeight: 500
    lineHeight: 22px
  xs-13:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 13px
    fontWeight: 400
    lineHeight: 20px
  xs-strong-13:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 13px
    fontWeight: 500
    lineHeight: 20px
  xxs-12:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 12px
    fontWeight: 400
    lineHeight: 18px
  xxs-strong-12:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 12px
    fontWeight: 500
    lineHeight: 18px
  xxxs-11:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 11px
    fontWeight: 400
    lineHeight: 14px
  xxxs-strong-11:
    fontFamily: "-apple-system, BlinkMacSystemFont, 'Segoe UI', 'PingFang SC', 'Hiragino Sans GB', 'Microsoft YaHei', 'Helvetica Neue', Helvetica, Arial, sans-serif"
    fontSize: 11px
    fontWeight: 500
    lineHeight: 14px
rounded:
  sm: 4px
  md: 6px
  lg: 8px
  xl: 10px
  2xl: 12px
  3xl: 16px
  pill: 999px
components:
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.primary-foreground}"
    rounded: "{rounded.md}"
  button-primary-hover:
    backgroundColor: "{colors.primary-hover}"
    textColor: "{colors.primary-foreground}"
    rounded: "{rounded.md}"
  button-primary-dimmed:
    backgroundColor: "{colors.primary-dimmed}"
    textColor: "{colors.primary}"
    rounded: "{rounded.md}"
  button-info:
    backgroundColor: "{colors.button-info-fill}"
    textColor: "#FFFFFF"
    rounded: "{rounded.md}"
  button-info-hover:
    backgroundColor: "{colors.button-info-hover}"
    textColor: "#FFFFFF"
    rounded: "{rounded.md}"
  button-ghost-active:
    backgroundColor: "{colors.button-ghost-active-fill}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.md}"
  button-floating:
    backgroundColor: "{colors.button-floating-fill}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.pill}"
  button-floating-hover:
    backgroundColor: "{colors.button-floating-hover}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.pill}"
  link:
    textColor: "{colors.link}"
    typography: "{typography.s-14}"
  tag:
    backgroundColor: "{colors.markdown-tag}"
    textColor: "{colors.secondary}"
    rounded: "{rounded.sm}"
  tooltip:
    backgroundColor: "{colors.tooltip-bg}"
    textColor: "#FFFFFF"
    rounded: "{rounded.md}"
  toast:
    backgroundColor: "{colors.toast-bg}"
    textColor: "#FFFFFF"
    rounded: "{rounded.lg}"
  card:
    backgroundColor: "{colors.surface-layer-1}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.2xl}"
  input-major:
    backgroundColor: "{colors.input-major}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.lg}"
  sidebar-nav-item:
    backgroundColor: "{colors.sidebar-fill}"
    textColor: "{colors.secondary}"
    rounded: "{rounded.md}"
  sidebar-nav-item-hover:
    backgroundColor: "{colors.sidebar-nav-item-hover}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.md}"
  sidebar-nav-item-active:
    backgroundColor: "{colors.sidebar-nav-item-active}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.md}"
  bubble:
    backgroundColor: "{colors.bubble}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.2xl}"
---

# DeepSeek Harness Design System

Design tokens and styling rules extracted from the DeepSeek Harness web client
theme package (`packages/client/ui-theme`) and its authoritative styling
reference (`docs/web-styling.md`).

## Overview

DeepSeek Harness is a professional, content-first agent workbench: quiet,
achromatic surfaces with a single brand-blue accent reserved for interaction
and business state. The interface should feel dense but calm — hierarchy comes
from typographic weight, neutral layering, and hairline strokes, not from
color blocks or heavy shadows.

Tokens follow a strict three-layer architecture, consumed top-down:

1. **Static scales** (`--dsw-static-*`) — raw palettes: `deepseek` (brand
   blue), `blue`, `neutral-bluish` (the working neutral), `neutral` (pure
   gray, used by scrollbars and inline code), `red`, `green`, `amber`.
   Theme-invariant; identical in light and dark.
2. **Semantic aliases** (`--dsw-alias-*`) — the only tokens feature components
   may consume. Rebound per theme: light values on `body`, dark overrides on
   `body[data-ds-dark-theme]`. Groups: `bg-*`, `border-*`, `brand-*`,
   `button-*`, `interactive-*`, `label-*`, `link`, `markdown-*`, `scrollbar-*`,
   `state-*`, `toast/tooltip`.
3. **Specific bindings** (`--dsw-specific-*`) — named surfaces: `bubble`,
   `input-major`, `sidebar-fill`, `menu`, `selector`, `tip`.

The token sheets are the sole color authority: values absent from the system
are deliberately not invented; the nearest semantic token wins. New values
enter as a static step plus a semantic alias in the same change.

## Colors

The palette is rooted in the cool `neutral-bluish` grays with a single
DeepSeek-blue accent.

- **Primary (#0F1115 light / #F9FAFB dark):** near-black ink in light mode,
  near-white in dark mode — inversion, not hue, is the emphasis mechanism.
  Used for primary text, brand marks, and primary button fills.
- **DeepSeek blue ({colors.deepseek-500}, dark {colors.deepseek-400}):** a
  point, never a fill. Appears only as links, info buttons, and business-state
  accents; it never floods large surfaces.
- **Neutral-bluish (00–1000):** the working neutral for every surface, label,
  and border. Dark mode lifts surfaces through `background` → `layer-1` →
  `layer-2` → `layer-3` (#151517 → #232324 → #2C2C2E → #353638); light mode
  keeps all layers white and separates with hairline borders.
- **State colors** keep their hue across themes: error shifts from
  `{colors.error}` (light) to `{colors.error-dark}` (dark); success and warn
  use the same 500-level anchor in both, with tertiary washes for panels.

## Typography

System font stack for UI and prose; a dedicated code stack (SF Mono /
JetBrains Mono / Fira Code …) deliberately omits a bare `monospace` tail so
Windows CJK does not fall back to SimSun.

- **UI ladder** (`xl-24` … `xxxs-11`): fixed sizes 24/20/16/14/13/12/11 px,
  each with a `strong` (+100 weight) variant. Figma weight 510 always renders
  as 500 on the web.
- **Markdown ladder** (`h1`–`h4`, `body`, `table`, `small`, `code`): the Figma
  16 px-base export scaled by 0.875 and rounded to integer pixels. The values
  in the front matter are the defaults at a 14 px content size.
- **Content font size is user-adjustable** (12–17 px, default 14). Headings
  and body shift by the same pixel delta to preserve hierarchy; the secondary
  tier (tables, flow-row titles/summaries) reads `setting − 1` at ≤14 and
  `setting − 2` above (bottoming out at 11 px). Dense small and code variants
  stay fixed at 12/20 and 12/19 (inline) / 11/19 (block).
- Every font size pairs with its line height; never set one without the other.

## Elevation & Depth

Depth is conveyed by hairline strokes plus faint layered shadows, never by
layout-consuming borders.

- **Elevated surfaces** (menus, popovers, modals, panels, floating buttons,
  the composer) set `border: 0` and take one of the elevation shadows. The
  first layer is always a 0.5 px stroke drawn inside the box-shadow; its color
  (`{colors.border-l4}` by default) rebinds per surface and can be suppressed
  (`transparent`) for soft states:
  - `stroke`: `0 0 0 0.5px <stroke-color>`
  - `panel`: `stroke, 0 3px 8px 0 rgba(0,0,0,0.03), 0 0 16px 0 rgba(0,0,0,0.02)`
  - `prominent`: `stroke, 0 3px 8px 0 rgba(0,0,0,0.04), 0 0 20px 0 rgba(0,0,0,0.05)`
  - `soft` (composer): `stroke, 0 4px 16px 0 rgba(0,0,0,0.03), 0 0 24px 0 rgba(0,0,0,0.03)`
- **Legacy shadow scale:** `lv1` `0 2px 4px rgba(0,0,0,0.05)`; `lv1-blur`
  `0 4px 12px rgba(0,0,0,0.02)`; `lv2` `0 4px 12px rgba(0,0,0,0.02), 0 2px
  8px rgba(0,0,0,0.04)`; `lv3` `0 0 1px rgba(0,0,0,0.2), 0 0 4px
  rgba(0,0,0,0.02), 0 12px 32px rgba(0,0,0,0.08)`.
- **Flat neutral borders and separators draw at 0.5 px** (one device pixel):
  buttons, inputs, cards, row dividers, filled-box separators. Only dashed
  affordances and state-colored borders keep 1 px. Intensity climbs
  `border-l1` → `border-l4`; light mode uses black at 4–16 % alpha, dark mode
  white at 6–20 %.
- Never pair a neutral `border-*` border with an lv/elevation shadow — the
  stroke is the border. State-colored borders (warn panels) stay real borders.
- Modal scrims use the `mask-*` tokens; `mask-blur` is `blur(2px)`.

## Shapes

- Corner radii come only from the `rounded` scale (4–16 px plus 999 px pills).
- On engines supporting `corner-shape`, every rounded corner renders as
  `superellipse(1.5)` — between a circular arc and a squircle. Full-round
  shapes (`50%`, `100%`, pill radii far above the box size) must pair
  `corner-shape: round` with their `border-radius`, because a superellipse
  deforms circles and squares off capsule ends.

## Components

- **Buttons:** primary (ink fill, inverts per theme), dimmed-primary, ghost
  (transparent at rest, neutral fills on active/hover, 500-level border when
  active), info (DeepSeek blue), floating/elevated (surface fill + elevation
  shadow, pill radius), contrast, and toolbar (translucent gray overlays).
- **Links:** `{colors.link}` at `font-weight: 500`, no underline at rest,
  dotted 3 px-offset underline on hover/focus. Text-leading anchors lead with
  a category glyph riding `currentColor`; image-only anchors carry no glyph.
- **Scrollbars:** 8 px WebKit thumbs on a transparent track; only the thumb
  carries a token color (`{colors.neutral-200}` / hover `{colors.neutral-300}`,
  dark `{colors.neutral-700}` / `{colors.neutral-600}`). Elevated surfaces
  rebind the thumb to the l2 pair on their own container; `transparent` is the
  other legal target. Firefox takes the standard `scrollbar-width: thin` path
  with no hover counterpart.
- **Code blocks:** background/foreground alias the markdown code-block tokens
  so highlighted and plain blocks agree. Syntax palette (shiki css-variables
  theme), light → dark: constant `#1C7ED6→#4DABF7`, string `#2F9E44→#69DB7C`,
  comment `#868E96→#ADB5BD`, keyword `#D6336C→#FAA2C1`, parameter
  `#E8590C→#FFA94D`, function `#6741D9→#B197FC`, string-expression
  `#2B8A3E→#8CE99A`, punctuation `#495057→#CED4DA`, link `#1971C2→#74C0FC`.
- **Thinking gradients:** a fade-out linear gradient (`#FFF → transparent`
  light, `#151517 → transparent` dark) masks streaming think content.

## Motion

All transitions ride `cubic-bezier(0.4, 0, 0.2, 1)` at 100 ms (fast) / 200 ms
(default) / 300 ms (slow). Preserve keyboard focus visibility and
reduced-motion behavior when adding transitions or hover-only controls.

Feature-level micro-animation conventions (shared primitives in
`app/lib/ui/animations.dart`):

- **Entrance** (`FadeSlideIn`): one-shot fade + ≤8 px upward drift at 200 ms
  for newly appearing content (message items, banners, panels, empty states).
  Paint-time transform only — never perturbs list layout or scroll metrics.
- **Reveal** (`AnimatedReveal`): paired size + fade at 200 ms for in-place
  expand/collapse (execution card details, banners, queue, working line);
  the subtree stays mounted through the exit animation, so callers cache the
  last non-empty content for the collapsing frame.
- **Hover/focus color**: 100 ms color interpolation (row fills, icon
  foreground ladder, composer focus ring, button fills) — state changes
  interpolate, never snap.
- **Slot swaps**: cross-fade (optionally with a subtle scale) at 100 ms for
  hover-revealed actions and status-indicator exchanges (spinner ↔ icon ↔
  dot); chevrons express expand state by a continuous 90° rotation instead of
  an icon swap.
- **Reduced motion**: when the platform requests reduced motion
  (`MediaQuery.disableAnimations`), entrance and reveal animations jump
  straight to their end state.

## Do's and Don'ts

- Do consume semantic alias tokens in feature components; don't copy static
  palette values or write literal colors there.
- Do keep light/dark overrides in the theme owner; don't put theme selectors
  in feature component CSS.
- Don't invent values absent from the token sheets — use the nearest semantic
  token; additions require a static step plus a semantic alias.
- Do keep presentation in CSS (CSS Modules + clsx); inline styles may pass
  component-local custom-property values but must not encode theme branches.
- Don't pair a neutral border with an elevation/lv shadow on the same surface.
- Do keep source text, terminal output, and diff lines unwrapped when the
  component contract requires column preservation.
- Do reuse the shared control primitives before restyling; a deliberate visual
  difference belongs in a prop, not a second copy.
