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
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "2.25rem"
    fontWeight: "700"
    lineHeight: "1.2"
  h2:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "1.875rem"
    fontWeight: "600"
    lineHeight: "1.3"
  h3:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "1.5rem"
    fontWeight: "600"
    lineHeight: "1.4"
  body:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "1rem"
    fontWeight: "400"
    lineHeight: "1.5"
  body-sm:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: "400"
    lineHeight: "1.5"
  caption:
    fontFamily: "Inter, system-ui, sans-serif"
    fontSize: "0.75rem"
    fontWeight: "400"
    lineHeight: "1.5"
rounded:
  sm: "4px"
  md: "6px"
  lg: "8px"
  xl: "12px"
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

The design system includes full dark mode support with appropriate contrast ratios. Dark mode colors are defined using the same token structure with adjusted values for dark backgrounds.

## Typography

Uses Inter as the primary font family with system-ui fallbacks. Font sizes follow a modular scale from caption (0.75rem) to h1 (2.25rem).

## Components

Components follow shadcn/ui patterns with consistent padding, rounded corners, and color token references. All interactive elements use the ring token for focus states.

## Accessibility

- All color combinations meet WCAG AA contrast requirements
- Focus states use the ring color token for visibility
- Interactive elements have clear hover and active states
