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
