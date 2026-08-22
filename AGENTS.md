# AGENTS.md

> A ADE (ai development environment): ratatui full-screen TUI plus a
> unified streaming provider abstraction, event-driven agent loop,
> SQLite-backed persistent sessions, and a skills system. Architecture decisions live in `docs/adr/`.

## Build & Check

Use the devenv environment for everything (never install or run project dependencies outside it):

- Enter environment: `devenv shell`
- Full check (CI-equivalent, must pass before every commit): `check`
- Build: `cargo build --workspace --all-features --locked`
- Format: `cargo fmt --all`
- Lint: `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`
- Test: `cargo nextest run --workspace --all-features --locked`
- Doc tests: `cargo test --workspace --doc --locked`

`check` additionally runs: `cargo doc` (`-D warnings`), `cargo deny`, `cargo audit`,
`cargo-machete`, `taplo fmt --check`, `typos`, and `scripts/check-file-size.sh` (the file-size
ratchet gate).

## Code Style

- Formatter: `rustfmt` (config in `rustfmt.toml`: `edition = "2024"`, `max_width = 100`)
- Linter: `clippy` (config in `clippy.toml`: `msrv = "1.97"`)
- Strict workspace lints (`[workspace.lints]` in `Cargo.toml`):
  - `unsafe_code = "forbid"`
  - `clippy::all` / `pedantic` / `nursery` / `cargo` all `deny`
- TOML: `taplo fmt` (config in `taplo.toml`)
- Spelling: `typos` (allowlist in `_typos.toml`; add project-specific words when falsely flagged)
- Per-file cap of 800 lines (`scripts/check-file-size.sh`, a ratchet gate; exemptions are recorded
  in `scripts/file-size-baseline.txt` and may only shrink, never grow)

## Testing

- Framework: `cargo-nextest` (unit + integration)
- Location: `#[cfg(test)]` modules inside each crate and `crates/runtime/*/tests/` / `crates/app/*/tests/`
- Coverage threshold: none

## Security & Safety

- Never commit secrets, tokens, or real `.env` files; see `config.example.toml` for the template
- `unsafe_code` is forbidden across the whole workspace
- Before adding a dependency, confirm license compatibility (`cargo deny`), no vulnerabilities
  (`cargo audit`), and no unused deps (`cargo-machete`); prefer existing dependencies and isolate
  core third-party calls behind thin adapters

## Design System

The project uses [DESIGN.md](https://github.com/google-labs-code/design.md) to describe the visual identity to coding agents.

- **File**: `DESIGN.md` in project root
- **Format**: YAML front matter (tokens) + Markdown body (rationale)
- **CLI**: `npx @google/design.md` (no local install needed)

### Commands

```bash
# Validate DESIGN.md
npx @google/design.md lint DESIGN.md

# Export to Tailwind v4 CSS theme
npx @google/design.md export --format css-tailwind DESIGN.md > web/src/theme.css

# Export to Tailwind v3 JSON config
npx @google/design.md export --format json-tailwind DESIGN.md > tailwind.theme.json

# Export to W3C Design Token Format
npx @google/design.md export --format dtcg DESIGN.md > tokens.json
```

### NPM Scripts (in `web/`)

```bash
npm run design:lint
npm run design:export-tailwind
npm run design:export-css
```

### Guidelines

- Keep `DESIGN.md` tokens in sync with `web/src/index.css` CSS variables
- Run `design:lint` before committing design changes
- Use token references (`{colors.primary}`) in component definitions
- See [spec](https://github.com/google-labs-code/design.md/blob/main/docs/spec.md) for full format reference

## UI Rules

Visual tokens have a single source of truth: `DESIGN.md` + the `@theme` block in `web/src/index.css`.
New visual properties must be added as tokens first, then consumed by components.

### 0. Design style: Minimalism

- Content first: establish information hierarchy before decoration; remove any element (divider,
  border, icon, color block) that serves no functional or semantic purpose
- Whitespace over separators: prefer spacing to express hierarchy; add a line or background only
  when spacing alone is not enough
- One focal point per screen: a single primary action; everything else degrades to secondary/ghost
- Restraint with color and effects: color, shadow, and radius are hierarchy tools — start with none
  and add only when needed
- Copy as interface: prefer clear typographic hierarchy to express state instead of adding badges or
  icons

### 1. Color ratio 70-20-10

- **~70% dominant**: the neutral skeleton (`background` / `foreground` / `card`) sets the base tone
- **~20% secondary**: surface-level grays (`muted` / `secondary` / `sidebar-accent`, hover states,
  separators)
- **≤10% accent**: the `primary` ink (near-black in light mode, near-white in dark mode —
  inversion, not hue, is the emphasis mechanism) plus the functional `success` / `destructive`
  semantic colors — only for interactive elements (links, buttons, selected states, focus rings)
  and status indicators
- Never flood large areas (whole cards, whole sidebar) with accent color; the interface is
  achromatic by default and chromatic tokens (`destructive`, `success`) appear only in their
  single designated role, never decoratively

### 2. Proportion and rhythm

- **Column width**: page and message flow share `max-w-page` (920px, defined in `index.css`
  `@theme`); do not introduce new column widths
- **Line height**: headings 1.2–1.4 (`h1` / `h2` / `h3` tokens), body and UI text 1.5 (`body` /
  `body-sm` / `caption`)
- **Spacing**: only spacing tokens (8/16/24/32): card padding 24, card/section gaps 16–24, control
  gaps 8; every `p-*` / `gap-*` / `px-*` value must map to a token step, no magic numbers

### 3. One unified set of base tokens

- **Radius**: only the rounded tokens (4/6/8/12/full); cards and bubbles lg(8)/xl(12), controls
  md(6), badges full
- **Shadow**: shadows are reserved for overlays (`shadow-md`/`shadow-lg` on dropdowns/dialogs);
  in-flow surfaces (cards, bubbles, inputs) are flat and use hairline borders instead; no custom
  `box-shadow` values
- **Border**: uniform 1px `border` token; use `separator` for dividers; focus state is always `ring`
  + `ring-ring/50`, never ad-hoc outline colors
- **Button height**: only the button size steps xs 24 / sm 32 / default 36 / lg 40 (icon buttons
  24/32/36/40); no custom heights
- **Icons**: lucide-react only; sizes limited to 12 (`size-3`, auxiliary) / 14 (`size-3.5`, inline
  default) / 16 (`size-4`, standard); icons are achromatic — categories are expressed by a foreground
  opacity ladder (e.g. ToolCard tints its icon via `text-foreground/<opacity>`) and only errors use
  `destructive`; no other icon libraries or inline SVGs

## After Coding

Every commit must pass the `check` command defined in `devenv.nix`.

## Commit & Release

- Write concise commit subjects, optionally with Conventional Commit prefixes (`feat:`, `fix:`,
  `docs:`, etc.)
- Each commit does one thing and can be reverted/cherry-picked independently; keep formatting or
  refactoring changes in their own commit
- Version control uses Jujutsu (`jj`); reorganize into atomic commits before pushing for review
- Release process: see `docs/releasing.md` (`release <semver>` creates a release branch; publishing
  is automated after the PR merges)

## References

- `docs/adr/` — architecture decision records (starting at 0001; read relevant ADRs before changing
  core design)
- `docs/releasing.md` — release process (read before releasing)
- `README.md` — user-facing guide, TUI keybindings, and slash commands
