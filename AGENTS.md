# AGENTS.md

> A ADE (ai development environment): TUI and WebUI plus a
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

- Never commit secrets, tokens, or real `.env` files; settings live in SQLite (see ADR-0039, `nomic config --help`)
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

# Export to W3C Design Token Format
npx @google/design.md export --format dtcg DESIGN.md > tokens.json
```

### Guidelines

- Keep `DESIGN.md` tokens in sync with the Flutter theme (`app/lib/theme.dart`)
- Run `design:lint` before committing design changes
- Use token references (`{colors.primary}`) in component definitions
- See [spec](https://github.com/google-labs-code/design.md/blob/main/docs/spec.md) for full format reference

## UI Rules

Visual tokens have a single source of truth: `DESIGN.md` (DeepSeek Harness design
system: static scales → semantic aliases → component bindings) and its Dart
expression `app/lib/theme.dart` (`NomicTokens` light/dark pairs, plus `AppText`,
`Radii`, `AppShadows`, `AppMotion`; Flutter GUI, ADR-0046). Feature components
consume semantic tokens only — no literal colors, and no values absent from the
token sheets (the nearest semantic token wins; additions land in `DESIGN.md` and
`theme.dart` in the same change).

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

### 1. Color: achromatic skeleton, blue as a point

- **Dominant (~70%)**: the neutral-bluish skeleton (`background` / `foreground` /
  `card`) sets the base tone; light mode keeps all layers white and separates with
  hairline borders, dark mode lifts surfaces (#151517 → #232324 → #2C2C2E →
  #353638)
- **Secondary (~20%)**: surface grays (`primaryDimmed` hover fills, `tip` strips,
  `codeSurface`, `sidebar` / `sidebarHover` / `sidebarActive`) and the muted text
  ladder (`secondary` / `tertiary` / `caption`)
- **Accent (≤10%)**: the `primary` ink (near-black in light mode, near-white in
  dark mode — inversion, not hue, is the emphasis mechanism) plus the functional
  `error` / `success` / `warning` colors — only for status indicators and
  destructive actions
- `business` (DeepSeek blue) is a point, never a fill: links, info buttons
  (send), focus rings, and live-state markers (running spinner, unread dot,
  active-session bar); never flood large areas (whole cards, whole sidebar) with
  accent color; the interface is achromatic by default and chromatic tokens appear
  only in their single designated role, never decoratively

### 2. Proportion and rhythm

- **Column width**: page and message flow share `maxPageWidth` (760px, defined in
  `app/lib/theme.dart`); do not introduce new column widths
- **Typography**: two ladders from DESIGN.md — markdown content (`h1` 21/30 …
  `small` 12/20, at the 14px default content size) and UI (`xl` 24/32 … `xxxs`
  11/14, each with a strong variant); every font size pairs with its line height —
  never set one without the other (`AppText` encodes both)
- **Spacing**: 8/16/24/32 (`Spacing` sm/md/lg/xl): card padding 24, card/section
  gaps 16–24, control gaps 8; every `padding` / `SizedBox` / `gap` value must map
  to a step, no magic numbers. DESIGN.md deliberately omits a spacing scale —
  this rhythm and `maxPageWidth` are component-owned conventions

### 3. One unified set of base tokens

- **Radius**: only the rounded tokens (4/6/8/10/12/16/pill via `Radii`
  sm/md/lg/xl/xxl/xxxl/full); cards xxl(12), user bubbles xxl(12), controls
  md(6), floating composer xxxl(16); pill only for circular controls
- **Shadow**: shadows are reserved for overlays (dropdowns/dialogs) and the
  floating composer, always via `AppShadows` (0.5px stroke + panel/prominent/soft
  layers); in-flow surfaces (cards, bubbles, inputs) are flat with hairline
  borders; never pair a neutral border with an elevation shadow on the same
  surface — the stroke is the border
- **Border**: hairline strokes climb `border` (l1, dividers) → `sidebarBorder`
  (l2, sidebar boundary) → `borderStrong` (l3, inputs / tables / outlined
  controls); focus state is always a `business`-blue ring, never ad-hoc outline
  colors
- **Motion**: transitions ride `AppMotion.curve` (cubic-bezier(0.4, 0, 0.2, 1))
  at the 100/200/300ms steps
- **Button height**: only the button size steps xs 24 / sm 32 / default 36 / lg 40 (icon buttons
  24/32/36/40); no custom heights
- **Icons**: lucide (`lucide_icons` package) only; sizes limited to 12 (auxiliary) / 14 (inline
  default) / 16 (standard); icons are achromatic — categories are expressed by a foreground
  opacity ladder (e.g. step rows tint their icon via foreground opacity) and only errors use
  `error`; no other icon libraries or inline SVGs
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
