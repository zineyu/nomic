---
name: Nomic
colors:
  primary: "oklch(0.19 0 0)"
  primary-foreground: "oklch(0.98 0 0)"
  secondary: "oklch(0.955 0 0)"
  secondary-foreground: "oklch(0.19 0 0)"
  accent: "oklch(0.955 0 0)"
  accent-foreground: "oklch(0.19 0 0)"
  destructive: "oklch(0.5 0.17 27)"
  destructive-foreground: "oklch(0.98 0 0)"
  background: "oklch(1 0 0)"
  foreground: "oklch(0.19 0 0)"
  card: "oklch(1 0 0)"
  card-foreground: "oklch(0.19 0 0)"
  muted: "oklch(0.955 0 0)"
  muted-foreground: "oklch(0.5 0 0)"
  border: "oklch(0.915 0 0)"
  input: "oklch(0.915 0 0)"
  ring: "oklch(0.19 0 0)"
  success: "oklch(0.52 0.11 155)"
  sidebar: "oklch(0.975 0 0)"
  sidebar-foreground: "oklch(0.19 0 0)"
  sidebar-primary: "oklch(0.19 0 0)"
  sidebar-primary-foreground: "oklch(0.98 0 0)"
  sidebar-accent: "oklch(0.94 0 0)"
  sidebar-accent-foreground: "oklch(0.19 0 0)"
  sidebar-border: "oklch(0.915 0 0)"
  sidebar-ring: "oklch(0.19 0 0)"
typography:
  h1:
    fontFamily: "Noto Sans, Noto Sans SC, system-ui, sans-serif"
    fontSize: "2.25rem"
    fontWeight: "700"
    lineHeight: "1.2"
  h2:
    fontFamily: "Noto Sans, Noto Sans SC, system-ui, sans-serif"
    fontSize: "1.875rem"
    fontWeight: "600"
    lineHeight: "1.3"
  h3:
    fontFamily: "Noto Sans, Noto Sans SC, system-ui, sans-serif"
    fontSize: "1.5rem"
    fontWeight: "600"
    lineHeight: "1.4"
  body:
    fontFamily: "Noto Sans, Noto Sans SC, system-ui, sans-serif"
    fontSize: "1rem"
    fontWeight: "400"
    lineHeight: "1.5"
  body-sm:
    fontFamily: "Noto Sans, Noto Sans SC, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: "400"
    lineHeight: "1.5"
  caption:
    fontFamily: "Noto Sans, Noto Sans SC, system-ui, sans-serif"
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

Nomic is an ADE (AI Development Environment). Its design language is **monochrome
minimalism**: a pure gray skeleton (chroma = 0) with a single achromatic accent —
*ink* (near-black in light mode, near-white in dark mode). Emphasis is expressed
through contrast inversion, typographic weight, and whitespace — never through hue.

Design principles, in order:

1. **Content first.** Establish information hierarchy before decoration. Any border,
   badge, icon, or color block that serves no functional or semantic purpose is removed.
2. **Whitespace over separators.** Spacing expresses hierarchy; hairline borders are
   added only where spacing alone cannot.
3. **One focal point per screen.** A single primary action per view; everything else
   degrades to secondary or ghost treatments.
4. **Restraint with color.** The interface is achromatic by default. The only chromatic
   tokens are `destructive` (errors, destructive actions) and `success` (transient
   confirmations) — each used in exactly one role, never decoratively.
5. **Copy as interface.** Typographic hierarchy (weight / size / gray level) carries
   state instead of badges and icons.

## Colors

The palette is a pure neutral gray ramp (oklch chroma 0, no hue tint) plus two
functional semantic colors.

- **Primary = ink** (`oklch(0.19 0 0)`): the single accent. Used for the primary
  button, the user chat bubble, focus rings, selected states, and running-status
  indicators. On dark mode it inverts to near-white. Inversion — not hue — is the
  emphasis mechanism.
- **Secondary / Muted / Accent**: light grays for hover states and sunken surfaces.
- **Muted-foreground** (`oklch(0.5 0 0)`): secondary text; tertiary text uses opacity
  steps of foreground (`foreground/70`, `/50`, …) instead of extra tokens.
- **Border / Input** (`oklch(0.915 0 0)`): hairline only, 1px.
- **Destructive** (`oklch(0.5 0.17 27)`): errors and destructive actions — the only
  hue allowed to appear persistently, because errors must be findable.
- **Success** (`oklch(0.52 0.11 155)`): transient confirmations only (e.g. the
  copy-button checkmark flash). Never used for persistent decoration.

There is deliberately **no categorical/chart palette**. Where the old design tinted
tool-call icons by category with chromatic colors, categories are now expressed by an
**opacity ladder of foreground** (100 / 75 / 60 / 45 / 35 percent): stronger ink means
more consequential action (execute > inspect > modify > interact > agent).

### Dark Mode

Dark mode inverts the ramp: background near-black, ink (primary) near-white. Token
structure is identical; only values change:

| Token | Value |
| --- | --- |
| `background` | `oklch(0.16 0 0)` |
| `foreground` / `popover-foreground` / `card-foreground` | `oklch(0.95 0 0)` |
| `card` / `popover` | `oklch(0.19 0 0)` |
| `primary` / `ring` / `sidebar-ring` | `oklch(0.95 0 0)` |
| `primary-foreground` | `oklch(0.19 0 0)` |
| `secondary` / `muted` / `accent` | `oklch(0.23 0 0)` |
| `secondary-foreground` / `accent-foreground` | `oklch(0.95 0 0)` |
| `muted-foreground` | `oklch(0.65 0 0)` |
| `destructive` / `destructive-foreground` | `oklch(0.62 0.17 25)` / `oklch(0.98 0 0)` |
| `success` | `oklch(0.68 0.12 155)` |
| `border` / `input` / `sidebar-border` | `oklch(1 0 0 / 10%)` / `oklch(1 0 0 / 14%)` / `oklch(1 0 0 / 10%)` |
| `sidebar` | `oklch(0.18 0 0)` |
| `sidebar-foreground` / `sidebar-accent-foreground` | `oklch(0.95 0 0)` |
| `sidebar-primary` | `oklch(0.95 0 0)` |
| `sidebar-primary-foreground` | `oklch(0.19 0 0)` |
| `sidebar-accent` | `oklch(0.25 0 0)` |

These values are kept in sync with the `.dark` block in `web/src/index.css`.

## Typography

A single **Noto Sans** voice for all UI and reading content (`--font-sans`),
locally bundled via `@fontsource`: sidebar, buttons, inputs, dialogs, user bubbles,
and assistant markdown messages (including their h1–h3 headings). **Maple Mono**
(`--font-mono`) covers code blocks, inline code, and tool/terminal surfaces
(`font-mono` call sites).

CJK is covered by **Noto Sans SC** (also `@fontsource`-bundled, unicode-range
sliced so the browser only downloads needed glyph chunks) before system fallbacks
in both stacks. Hierarchy comes from
weight and the modular scale only: h1 = 2.25rem/700, h2 = 1.875rem/600,
h3 = 1.5rem/600, body = 1rem/400, body-sm = 0.875rem/400, caption = 0.75rem/400.
Line heights: headings 1.2–1.4, body and UI text 1.5.

## Proportion and Rhythm

- **Column width**: page and message flow share `max-w-page` (920px, defined in
  `index.css` `@theme`); no other column widths.
- **Spacing**: only the spacing tokens (8 / 16 / 24 / 32). Card padding 24, section
  gaps 16–24, control gaps 8. Every `p-*` / `gap-*` value maps to a token step.
- **Radius**: sm 4 / md 6 / lg 8 / xl 12 / full. Cards and bubbles lg–xl, controls md,
  badges full. Radii are hierarchy tools — start with none and add only when needed.
- **Shadow**: shadows are reserved for overlays (dropdowns, dialogs, `shadow-md` and
  up). Cards, bubbles, and inputs use hairline borders instead of shadows.
- **Border**: uniform 1px `border`; focus state is always `ring` + `ring/50`.

## Components

Components follow shadcn/ui patterns, restrained to the monochrome ramp:

- **Primary button / user chat bubble**: solid ink block with inverted text — the one
  high-contrast element on screen.
- **Selected / active states**: neutral `accent` fill plus medium weight; a checkmark
  or 1.5px ink dot marks "current" instead of a colored pill.
- **Status in the message flow**: tool calls render as quiet text rows
  (icon + name + args in muted gray). Completion is neutral (a check in muted ink);
  only failures turn red. Tool icons differentiate category by the foreground opacity
  ladder described above — no chromatic category colors.
- **Context-usage ring**: grayscale escalation — `muted-foreground` below 75%,
  `foreground` from 75–90%, `destructive` above 90%.
- **Links**: `foreground` with a `foreground/30` underline that solidifies on hover,
  instead of a colored link.
- **Overlays** (dropdown / dialog / tooltip / popover): the only elements with shadow;
  everything in-flow is flat.

## Accessibility

- All text/background combinations meet WCAG AA contrast (foreground 0.19 vs
  background 1.0 exceeds 12:1 in light mode)
- Focus states use the ink ring token for visibility
- State is never carried by color alone: errors pair red with an icon + label,
  selection pairs the neutral fill with a checkmark or weight change
