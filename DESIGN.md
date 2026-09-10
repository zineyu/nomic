---
name: Nomic
colors:
  primary: "#1F1F1C"
  primary-foreground: "#FFFFFF"
  secondary: "#F1F1EE"
  secondary-foreground: "#1F1F1C"
  accent: "#3B82F6"
  accent-soft: "#EAF2FF"
  destructive: "#DC2626"
  destructive-foreground: "#FFFFFF"
  background: "#FFFFFF"
  foreground: "#1F1F1C"
  card: "#FFFFFF"
  card-foreground: "#1F1F1C"
  muted: "#F1F1EE"
  muted-foreground: "#6F6F6A"
  tertiary: "#9A9A94"
  border: "#E7E5DF"
  border-strong: "#D8D6D0"
  input: "#E7E5DF"
  ring: "#3B82F6"
  success: "#16A34A"
  warning: "#D97706"
  bubble: "#F4F4F2"
  sidebar: "#F7F7F5"
  sidebar-foreground: "#1F1F1C"
  sidebar-primary: "#1F1F1C"
  sidebar-primary-foreground: "#FFFFFF"
  sidebar-accent: "#ECECE8"
  sidebar-accent-foreground: "#1F1F1C"
  sidebar-border: "#E7E5DF"
  sidebar-ring: "#3B82F6"
typography:
  h1:
    fontFamily: "system-ui, PingFang SC, sans-serif"
    fontSize: "1.75rem"
    fontWeight: "700"
    lineHeight: "1.25"
  h2:
    fontFamily: "system-ui, PingFang SC, sans-serif"
    fontSize: "1.375rem"
    fontWeight: "600"
    lineHeight: "1.3"
  h3:
    fontFamily: "system-ui, PingFang SC, sans-serif"
    fontSize: "1.125rem"
    fontWeight: "600"
    lineHeight: "1.4"
  body:
    fontFamily: "system-ui, PingFang SC, sans-serif"
    fontSize: "1rem"
    fontWeight: "400"
    lineHeight: "1.5"
  body-sm:
    fontFamily: "system-ui, PingFang SC, sans-serif"
    fontSize: "0.875rem"
    fontWeight: "400"
    lineHeight: "1.5"
  ui:
    fontFamily: "system-ui, PingFang SC, sans-serif"
    fontSize: "0.8125rem"
    fontWeight: "400"
    lineHeight: "1.5"
  caption:
    fontFamily: "system-ui, PingFang SC, sans-serif"
    fontSize: "0.75rem"
    fontWeight: "400"
    lineHeight: "1.5"
rounded:
  sm: "4px"
  md: "6px"
  lg: "8px"
  xl: "12px"
  2xl: "16px"
  full: "9999px"
spacing:
  sm: "8px"
  md: "16px"
  lg: "24px"
  xl: "32px"
components:
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.primary-foreground}"
    rounded: "{rounded.md}"
    padding: "8px 16px"
  button-secondary:
    backgroundColor: "{colors.secondary}"
    textColor: "{colors.secondary-foreground}"
    rounded: "{rounded.md}"
    padding: "8px 16px"
  button-destructive:
    backgroundColor: "{colors.destructive}"
    textColor: "{colors.destructive-foreground}"
    rounded: "{rounded.md}"
    padding: "8px 16px"
  card:
    backgroundColor: "{colors.card}"
    textColor: "{colors.card-foreground}"
    rounded: "{rounded.lg}"
    padding: "24px"
  badge:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.primary-foreground}"
    rounded: "{rounded.full}"
    padding: "2px 8px"
    typography: "{typography.caption}"
  badge-muted:
    backgroundColor: "{colors.muted}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.full}"
    padding: "2px 8px"
    typography: "{typography.caption}"
  input:
    backgroundColor: "{colors.background}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.md}"
    padding: "4px 12px"
  dialog:
    backgroundColor: "{colors.background}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.lg}"
    padding: "24px"
  tooltip:
    backgroundColor: "{colors.foreground}"
    textColor: "{colors.background}"
    rounded: "{rounded.md}"
    padding: "6px 12px"
  separator:
    backgroundColor: "{colors.border}"
  skeleton:
    backgroundColor: "{colors.accent}"
    rounded: "{rounded.md}"
  sidebar:
    backgroundColor: "{colors.sidebar}"
    textColor: "{colors.sidebar-foreground}"
    rounded: "{rounded.lg}"
  user-bubble:
    backgroundColor: "{colors.bubble}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.xl}"
    padding: "8px 16px"
  composer:
    backgroundColor: "{colors.card}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.2xl}"
    padding: "16px"
  accent-dot:
    backgroundColor: "{colors.accent}"
    rounded: "{rounded.full}"
---

## Overview

Nomic is an ADE (AI Development Environment). Its design language is the
**narrative log**: a warm-paper canvas on which the agent's work reads as a
continuous record — bold narrative sentences carry the story, and the noise of
tool execution collapses into quiet, countable ledger rows ("已读取 3 个文件 ·
已运行 1 条命令") that expand only on demand.

Design principles, in order:

1. **The narrative is the interface.** The agent's prose is set full-width like a
   document, not wrapped in bubbles or cards. Everything else (tool calls, status,
   timestamps) is marginalia: muted, compact, collapsible.
2. **Content first.** Any border, badge, icon, or color block that serves no
   functional or semantic purpose is removed.
3. **Whitespace over separators.** Spacing expresses hierarchy; hairline borders
   appear only where spacing alone cannot.
4. **One focal point per screen.** A single primary action per view; everything
   else degrades to secondary or ghost treatments.
5. **Copy as interface.** Typographic hierarchy (weight / size / gray level)
   carries state instead of badges and icons.

## Colors

The palette is a warm-leaning neutral ramp (paper whites, soft gray fills) plus
exactly three functional colors.

- **Primary = ink** (`#1F1F1C`): the high-contrast element — primary buttons and
  the circular send/stop button. On dark mode it inverts to near-white.
- **Secondary / Muted / Accent**: light warm grays for hover states, sunken
  surfaces, and code backgrounds.
- **Bubble** (`#F4F4F2`): the user's own messages render as borderless,
  shadowless gray paper blocks — visibly "mine" without shouting.
- **Muted-foreground** (`#6F6F6A`): secondary text — timestamps, ledger rows,
  subtitles. **Tertiary** (`#9A9A94`) is reserved for placeholders and the
  weakest hints; nothing weaker exists.
- **Border** (`#E7E5DF`): hairline only, 1px. **Border-strong** (`#D8D6D0`) is
  for boundaries that must stay legible against fills (table grids, focused
  inputs).
- **Accent** (`#3B82F6`): the single chromatic *state* color — the selected
  session's 3px left bar, the unread dot, the focus ring, the enabled send
  button. Accent is a point or a line, never a fill; `accent-soft` (`#EAF2FF`)
  is its only fill-grade companion. Selected, unread, and running are three
  *separate* indicators (bar / dot / spinner) — never one dot reused.
- **Destructive** (`#DC2626`): errors and destructive actions.
- **Warning** (`#D97706`): exactly one role — the token-usage readout
  approaching the context limit.
- **Success** (`#16A34A`): transient confirmations only (e.g. the copy-button
  checkmark flash). Never used for persistent decoration.

Tool-call categories are expressed by an **opacity ladder of foreground**
(100 / 75 / 60 / 45 / 35 percent): stronger ink means more consequential action
(execute > inspect > modify > interact > agent).

### Dark Mode

Dark mode inverts the ramp: near-black warm canvas, ink near-white, `accent`
brightened to stay legible. Token structure is identical; only values change:

| Token | Dark value |
| --- | --- |
| `background` | `#161615` |
| `foreground` / `card-foreground` | `#ECECE8` |
| `card` / `popover` | `#1C1C1A` |
| `primary` / `sidebar-primary` | `#ECECE8` |
| `primary-foreground` / `sidebar-primary-foreground` | `#1F1F1C` |
| `secondary` / `muted` | `#242422` |
| `sidebar-accent` | `#262624` |
| `muted-foreground` | `#9A9A94` |
| `tertiary` | `#6F6F6A` |
| `destructive` | `#E85D52` |
| `success` | `#3FB970` |
| `warning` | `#E09543` |
| `accent` / `ring` / `sidebar-ring` | `#60A5FA` |
| `accent-soft` | `#1C2B4A` |
| `bubble` | `#232321` |
| `border` / `input` / `sidebar-border` | `#2C2C29` |
| `border-strong` | `#3A3A36` |
| `sidebar` | `#1A1A18` |
| `sidebar-foreground` | `#ECECE8` |

These values are kept in sync with the dark theme in `app/lib/theme.dart`.

## Typography

A single **system-ui** voice (PingFang SC covers CJK via platform fallback) for
all UI and reading content: sidebar, buttons, inputs, dialogs, user blocks, and
assistant markdown. **Menlo** (with Monaco / Consolas / Courier New fallbacks)
covers code blocks, inline code, and quantitative readouts (elapsed time,
counts, token numbers). Hierarchy comes from weight and the modular scale only:
h1 = 1.75rem/700, h2 = 1.375rem/600, h3 = 1.125rem/600, body = 1rem/400,
body-sm = 0.875rem/400 (the message-flow default), ui = 0.8125rem/400,
caption = 0.75rem/400. Line heights: headings 1.25–1.4, body and UI text 1.5.

## Proportion and Rhythm

- **Column width**: page and message flow share `maxPageWidth` (760px, defined
  in `app/lib/theme.dart`); no other column widths.
- **Spacing**: only the spacing tokens (8 / 16 / 24 / 32). Card padding 24,
  section gaps 16–24, control gaps 8. Every padding / gap value maps to a token
  step.
- **Radius**: sm 4 / md 6 / lg 8 / xl 12 / 2xl 16 / full. User blocks xl,
  controls md, badges full; 2xl is reserved for the floating composer.
- **Shadow**: shadows are reserved for overlays (dropdowns, dialogs) and the
  floating composer — the one element that hovers above the transcript.
  Everything else in-flow is flat and uses hairline borders.
- **Border**: uniform 1px `border`; focus state is always `ring` + 50% opacity.

## Components

- **Primary button / send button**: solid ink; the composer's send/stop is a
  36px ink circle — the one high-contrast element on screen.
- **User message**: a borderless `bubble` gray block, right-aligned and
  width-capped; no avatar, no border, no shadow.
- **Tool ledger**: consecutive tool calls collapse into a single muted ledger
  row (icon + counts per category + chevron). While any call in the group runs,
  a spinner replaces the chevron; any failure turns the row's count red.
  Expanded, each call renders as a quiet text row; completion is a neutral
  check, only failures turn red. Tool icons differentiate category by the
  foreground opacity ladder — no chromatic category colors.
- **Selected / active states**: `surface-selected` fill plus a 3px `accent`
  left bar; unread carries a separate `accent` dot; running carries a spinner.
  The three indicators are never merged into one.
- **Context-usage readout**: grayscale escalation — `muted-foreground` below
  75%, `foreground` from 75–90%, `destructive` above 90%.
- **Links**: `foreground` with underline, instead of a colored link.
- **Overlays** (dropdown / dialog / tooltip / popover / composer): the only
  elements with shadow; everything in-flow is flat.

## Accessibility

- All text/background combinations meet WCAG AA contrast
- Focus states use the ink ring token for visibility
- State is never carried by color alone: errors pair red with an icon + label,
  running uses a spinner, selection pairs the neutral fill with the accent bar
