---
name: Nomic
colors:
  primary: "oklch(0.52 0.16 245)"
  primary-foreground: "oklch(0.985 0 0)"
  secondary: "oklch(0.967 0.008 250)"
  secondary-foreground: "oklch(0.21 0.02 250)"
  accent: "oklch(0.967 0.008 250)"
  accent-foreground: "oklch(0.21 0.02 250)"
  destructive: "oklch(0.577 0.245 27.325)"
  destructive-foreground: "oklch(0.985 0 0)"
  background: "oklch(1 0 0)"
  foreground: "oklch(0.21 0.02 250)"
  card: "oklch(1 0 0)"
  card-foreground: "oklch(0.21 0.02 250)"
  muted: "oklch(0.967 0.008 250)"
  muted-foreground: "oklch(0.55 0.03 250)"
  border: "oklch(0.9 0.012 250)"
  input: "oklch(0.9 0.012 250)"
  ring: "oklch(0.52 0.16 245)"
  success: "oklch(0.596 0.145 163.225)"
  warning: "oklch(0.62 0.16 60)"
  warning-foreground: "oklch(0.985 0 0)"
  chart-1: "oklch(0.62 0.19 45)"
  chart-2: "oklch(0.6 0.13 195)"
  chart-3: "oklch(0.55 0.18 300)"
  chart-4: "oklch(0.6 0.15 150)"
  chart-5: "oklch(0.58 0.18 330)"
  sidebar: "oklch(0.985 0 0)"
  sidebar-foreground: "oklch(0.21 0.02 250)"
  sidebar-primary: "oklch(0.52 0.16 245)"
  sidebar-primary-foreground: "oklch(0.985 0 0)"
  sidebar-accent: "oklch(0.967 0.008 250)"
  sidebar-accent-foreground: "oklch(0.21 0.02 250)"
  sidebar-border: "oklch(0.9 0.012 250)"
  sidebar-ring: "oklch(0.52 0.16 245)"
typography:
  h1:
    fontFamily: "Maple Mono, system-ui, sans-serif"
    fontSize: "2.25rem"
    fontWeight: "700"
    lineHeight: "1.2"
  h2:
    fontFamily: "Maple Mono, system-ui, sans-serif"
    fontSize: "1.875rem"
    fontWeight: "600"
    lineHeight: "1.3"
  h3:
    fontFamily: "Maple Mono, system-ui, sans-serif"
    fontSize: "1.5rem"
    fontWeight: "600"
    lineHeight: "1.4"
  body:
    fontFamily: "Maple Mono, system-ui, sans-serif"
    fontSize: "1rem"
    fontWeight: "400"
    lineHeight: "1.5"
  body-sm:
    fontFamily: "Maple Mono, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: "400"
    lineHeight: "1.5"
  caption:
    fontFamily: "Maple Mono, system-ui, sans-serif"
    fontSize: "0.75rem"
    fontWeight: "400"
    lineHeight: "1.5"
rounded:
  sm: "4px"
  md: "6px"
  lg: "8px"
  xl: "12px"
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
---

## Overview

Nomic is an ADE (AI Development Environment) with a ratatui full-screen TUI. The design system follows shadcn/ui conventions with a clean, minimal aesthetic using oklch color space for precise color control.

## Colors

The palette pairs a restrained cool-neutral skeleton with a single chromatic accent and a categorical palette, using semantic color tokens for different states and contexts.

- **Primary**: Light blue (sky) accent (`oklch(0.52 0.16 245)`) for interactive elements — links, selected states, focus rings, running-status indicators, and the user chat bubble
- **Secondary / Accent**: Light cool gray for backgrounds and hover states
- **Destructive**: Red for errors and destructive actions
- **Success**: Green for success states and confirmations
- **Warning**: Amber for intermediate caution states (e.g. context usage between healthy and critical)
- **Chart 1–5**: Categorical palette (orange / teal / violet / green / magenta). Besides future data visualization, it colors tool-call categories in the chat: 执行 bash → chart-1，查看 read/grep/find → chart-2，修改 write/edit → chart-3，交互 ask/todo → chart-4，代理 agent 系列 → chart-5
- **Background/Foreground**: Base layer colors for the application surface. Neutrals keep hue 250 (blue-leaning gray) with slightly raised chroma (0.01–0.03) so grays read as intentional cool tones rather than pure gray

### Dark Mode

The design system includes full dark mode support with appropriate contrast ratios. Dark mode colors are defined using the same token structure with adjusted values for dark backgrounds:

| Token | Value |
| --- | --- |
| `background` / `popover` / `card` | `oklch(0.147 0.008 250)` / `oklch(0.21 0.012 250)` / `oklch(0.21 0.012 250)` |
| `foreground` / `popover-foreground` / `card-foreground` | `oklch(0.985 0 0)` |
| `primary` / `secondary` / `accent` | `oklch(0.72 0.13 235)` / `oklch(0.274 0.012 250)` / `oklch(0.274 0.012 250)` |
| `primary-foreground` / `secondary-foreground` / `accent-foreground` | `oklch(0.21 0.02 250)` / `oklch(0.985 0 0)` / `oklch(0.985 0 0)` |
| `muted` / `muted-foreground` | `oklch(0.274 0.012 250)` / `oklch(0.705 0.03 250)` |
| `destructive` / `destructive-foreground` | `oklch(0.704 0.191 22.216)` / `oklch(0.985 0 0)` |
| `border` / `input` | `oklch(1 0 0 / 10%)` / `oklch(1 0 0 / 15%)` |
| `ring` / `success` | `oklch(0.72 0.13 235)` / `oklch(0.696 0.17 162.48)` |
| `warning` / `warning-foreground` | `oklch(0.75 0.15 75)` / `oklch(0.21 0.02 250)` |
| `chart-1` … `chart-5` | `oklch(0.75 0.16 45)` / `oklch(0.75 0.11 195)` / `oklch(0.72 0.15 300)` / `oklch(0.75 0.14 150)` / `oklch(0.72 0.15 330)` |
| `sidebar` / `sidebar-foreground` | `oklch(0.21 0.012 250)` / `oklch(0.985 0 0)` |
| `sidebar-primary` / `sidebar-primary-foreground` | `oklch(0.72 0.13 235)` / `oklch(0.21 0.02 250)` |
| `sidebar-accent` / `sidebar-accent-foreground` | `oklch(0.274 0.012 250)` / `oklch(0.985 0 0)` |
| `sidebar-border` / `sidebar-ring` | `oklch(1 0 0 / 10%)` / `oklch(0.72 0.13 235)` |

These values are kept in sync with the `.dark` block in `web/src/index.css`.

## Typography

Uses **Maple Mono** as the default font family (locally bundled via `@fontsource/maple-mono`, so no runtime network fetch is needed), with system-ui fallbacks for glyphs it doesn't cover (e.g. CJK). Code and UI share the same default; code blocks may also use the `font-mono` stack which resolves to Maple Mono first. Font sizes follow a modular scale from caption (0.75rem) to h1 (2.25rem): h1 = 2.25rem/700, h2 = 1.875rem/600, h3 = 1.5rem/600, body = 1rem/400, body-sm = 0.875rem/400, caption = 0.75rem/400.

## Components

Components follow shadcn/ui patterns with consistent padding, rounded corners, and color token references. Interactive hover/active states use the accent tokens; all interactive elements use the ring token (light blue) for focus states. Form fields use the `input` token for borders and placeholders use `muted-foreground`. Chat status badges reuse `success` / `destructive` with translucent backgrounds, and the sidebar hover/active states use `sidebar-accent` / `sidebar-primary`. The user chat bubble is a solid `primary` (light blue) block with `primary-foreground` text. Tool-call cards tint their leading icon by category using the `chart-1` … `chart-5` categorical palette (icon-only, decorative; the tool name stays `muted-foreground` for AA contrast). The context-usage ring escalates `muted-foreground` → `primary` → `warning` → `destructive` as usage crosses 50% / 65% / 80%.

## Accessibility

- All color combinations meet WCAG AA contrast requirements
- Focus states use the ring color token for visibility
- Interactive elements have clear hover and active states
