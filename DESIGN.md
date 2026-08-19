---
name: Nomic
colors:
  primary: "oklch(0.21 0.006 285.885)"
  primary-foreground: "oklch(0.985 0 0)"
  secondary: "oklch(0.97 0.001 286.375)"
  secondary-foreground: "oklch(0.21 0.006 285.885)"
  accent: "oklch(0.97 0.001 286.375)"
  accent-foreground: "oklch(0.21 0.006 285.885)"
  destructive: "oklch(0.577 0.245 27.325)"
  destructive-foreground: "oklch(0.985 0 0)"
  background: "oklch(1 0 0)"
  foreground: "oklch(0.21 0.006 285.885)"
  card: "oklch(1 0 0)"
  card-foreground: "oklch(0.21 0.006 285.885)"
  muted: "oklch(0.97 0.001 286.375)"
  muted-foreground: "oklch(0.552 0.016 285.938)"
  border: "oklch(0.9 0.005 285.823)"
  input: "oklch(0.9 0.005 285.823)"
  ring: "oklch(0.552 0.016 285.938)"
  success: "oklch(0.596 0.145 163.225)"
  chart-1: "oklch(0.646 0.222 41.116)"
  chart-2: "oklch(0.6 0.118 184.704)"
  chart-3: "oklch(0.398 0.07 227.392)"
  chart-4: "oklch(0.828 0.189 84.429)"
  chart-5: "oklch(0.769 0.188 70.08)"
  sidebar: "oklch(0.985 0 0)"
  sidebar-foreground: "oklch(0.21 0.006 285.885)"
  sidebar-primary: "oklch(0.21 0.006 285.885)"
  sidebar-primary-foreground: "oklch(0.985 0 0)"
  sidebar-accent: "oklch(0.967 0.001 286.375)"
  sidebar-accent-foreground: "oklch(0.21 0.006 285.885)"
  sidebar-border: "oklch(0.9 0.005 285.823)"
  sidebar-ring: "oklch(0.552 0.016 285.938)"
typography:
  h1:
    fontFamily: "Inter Variable, Inter, system-ui, sans-serif"
    fontSize: "2.25rem"
    fontWeight: "700"
    lineHeight: "1.2"
  h2:
    fontFamily: "Inter Variable, Inter, system-ui, sans-serif"
    fontSize: "1.875rem"
    fontWeight: "600"
    lineHeight: "1.3"
  h3:
    fontFamily: "Inter Variable, Inter, system-ui, sans-serif"
    fontSize: "1.5rem"
    fontWeight: "600"
    lineHeight: "1.4"
  body:
    fontFamily: "Inter Variable, Inter, system-ui, sans-serif"
    fontSize: "1rem"
    fontWeight: "400"
    lineHeight: "1.5"
  body-sm:
    fontFamily: "Inter Variable, Inter, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: "400"
    lineHeight: "1.5"
  caption:
    fontFamily: "Inter Variable, Inter, system-ui, sans-serif"
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

The palette uses high-contrast neutrals with semantic color tokens for different states and contexts.

- **Primary**: Deep ink for headlines and core text
- **Secondary**: Light gray for backgrounds and subtle elements
- **Destructive**: Red for errors and destructive actions
- **Success**: Green for success states and confirmations
- **Background/Foreground**: Base layer colors for the application surface

### Dark Mode

The design system includes full dark mode support with appropriate contrast ratios. Dark mode colors are defined using the same token structure with adjusted values for dark backgrounds:

| Token | Value |
| --- | --- |
| `background` / `popover` / `card` | `oklch(0.147 0.004 285.823)` / `oklch(0.21 0.006 285.885)` / `oklch(0.21 0.006 285.885)` |
| `foreground` / `popover-foreground` / `card-foreground` | `oklch(0.985 0 0)` |
| `primary` / `secondary` / `accent` | `oklch(0.985 0 0)` / `oklch(0.274 0.006 286.033)` / `oklch(0.274 0.006 286.033)` |
| `primary-foreground` / `secondary-foreground` / `accent-foreground` | `oklch(0.21 0.006 285.885)` / `oklch(0.985 0 0)` / `oklch(0.985 0 0)` |
| `muted` / `muted-foreground` | `oklch(0.274 0.006 286.033)` / `oklch(0.705 0.015 286.067)` |
| `destructive` / `destructive-foreground` | `oklch(0.704 0.191 22.216)` / `oklch(0.985 0 0)` |
| `border` / `input` | `oklch(1 0 0 / 10%)` / `oklch(1 0 0 / 15%)` |
| `ring` / `success` | `oklch(0.705 0.015 286.067)` / `oklch(0.696 0.17 162.48)` |
| `chart-1` … `chart-5` | `oklch(0.488 0.243 264.376)` / `oklch(0.696 0.17 162.48)` / `oklch(0.769 0.188 70.08)` / `oklch(0.627 0.265 303.9)` / `oklch(0.645 0.246 16.439)` |
| `sidebar` / `sidebar-foreground` | `oklch(0.21 0.006 285.885)` / `oklch(0.985 0 0)` |
| `sidebar-primary` / `sidebar-primary-foreground` | `oklch(0.985 0 0)` / `oklch(0.21 0.006 285.885)` |
| `sidebar-accent` / `sidebar-accent-foreground` | `oklch(0.274 0.006 286.033)` / `oklch(0.985 0 0)` |
| `sidebar-border` / `sidebar-ring` | `oklch(1 0 0 / 10%)` / `oklch(0.705 0.015 286.067)` |

These values are kept in sync with the `.dark` block in `web/src/index.css`.

## Typography

Uses Inter (delivered as the locally bundled **Inter Variable** font via `@fontsource-variable/inter`, so no runtime network fetch is needed) as the primary font family with system-ui fallbacks. Font sizes follow a modular scale from caption (0.75rem) to h1 (2.25rem): h1 = 2.25rem/700, h2 = 1.875rem/600, h3 = 1.5rem/600, body = 1rem/400, body-sm = 0.875rem/400, caption = 0.75rem/400.

## Components

Components follow shadcn/ui patterns with consistent padding, rounded corners, and color token references. Interactive hover/active states use the accent tokens; all interactive elements use the ring token for focus states. Form fields use the `input` token for borders and placeholders use `muted-foreground`. Chat status badges reuse `success` / `destructive` with translucent backgrounds, and the sidebar hover/active states use `sidebar-accent` / `sidebar-primary`. The `chart-1` … `chart-5` tokens are reserved for future data visualization.

## Accessibility

- All color combinations meet WCAG AA contrast requirements
- Focus states use the ring color token for visibility
- Interactive elements have clear hover and active states
