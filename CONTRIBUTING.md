# Contributing to Claustre

## Prerequisites

- **Rust** (edition 2024) — install via [rustup](https://rustup.rs/)
- **gh** — GitHub CLI, used by hooks for PR detection (`brew install gh`)
- **cargo-deny** — dependency auditing (`cargo install cargo-deny`)
- **cargo-llvm-cov** (optional) — for coverage reports (`cargo install cargo-llvm-cov && rustup component add llvm-tools-preview`)
- **Node.js** (optional) — only needed for skills management (`npx skills`) and the docs site

## Building

The project is a Cargo workspace with a shared library and two binaries: the CLI/TUI (`claustre`) and a Tauri desktop app (`claustre-app`).

```bash
# Debug build
make build            # or: cargo build

# Release build
make release          # or: cargo build --release

# Install to ~/.cargo/bin
make install          # runs: cargo install --path . && codesign
```

### Important: install location

`make install` puts the binary in `~/.cargo/bin/claustre`. If you also have a copy at `~/.local/bin/claustre` (e.g., from a prior release download or auto-update), your shell may resolve to the stale one depending on `$PATH` order. Check with:

```bash
which -a claustre
```

If you see two entries, remove the one you don't want or ensure `~/.cargo/bin` comes first in your `$PATH`.

### Tauri desktop app

The native macOS app is a separate build step:

```bash
cargo build -p claustre-app          # debug
cd app && cargo tauri build          # release (produces .app bundle)
```

To make `claustre app` find the binary, copy it next to your claustre install:

```bash
cp app/src-tauri/target/release/claustre-app ~/.cargo/bin/
```

## Testing

```bash
make test             # cargo test
make check            # full CI-equivalent: fmt + lint + deny + doc + test + coverage gate
```

### Coverage

Minimum coverage threshold is 60%. The `check` target enforces this.

```bash
make cov              # summary
make cov-report       # per-file breakdown
make cov-html         # HTML report at coverage/html/index.html
```

## Code Standards

- **Clippy is strict**: `clippy::all` is denied, `clippy::pedantic` is warned. The project must compile with zero clippy warnings.
- **No `unwrap()` in production code.** Use `.context()` from `anyhow` for actionable errors. `expect("reason")` is fine for known-valid constants (e.g., compiled regexes).
- **Edition 2024 features** are used throughout (let-chains, etc.).
- **Formatting**: `cargo fmt --check` must pass. Run `cargo fmt` before committing.
- Use `#[expect(dead_code, reason = "...")]` instead of `#[allow(dead_code)]` for intentional dead code.

## Making Changes

1. Create a branch from `main`
2. Make your changes
3. Run `make check` to verify everything passes
4. Update documentation if you changed:
   - CLI subcommands or flags
   - TUI keybindings
   - Task statuses or lifecycle
   - Database schema (append to the `MIGRATIONS` array — never modify existing migrations)
   - Architecture or module responsibilities
5. Open a PR against `main`

### What to update

| Change | Update |
|--------|--------|
| New/changed CLI subcommand | `CLAUDE.md` CLI table, `README.md` |
| New/changed keybinding | `CLAUDE.md` key tables, `README.md` |
| New task status | `CLAUDE.md` lifecycle diagram |
| New DB migration | `CLAUDE.md` store section (migration count + purpose) |
| New module | `CLAUDE.md` module table |

## Project Layout

```
src/
  main.rs              CLI entry point (clap)
  lib.rs               Shared library root
  config/              Config loading, CLAUDE.md merge, paths
  store/               SQLite schema, models, CRUD
  tui/                 ratatui terminal UI
  session/             Git worktree lifecycle + session setup
  pty/                 Embedded PTY (portable-pty + vt100)
  skills/              skills.sh CLI wrapper
  scanner/             External Claude Code session scanner
  session_host.rs      Detached PTY owner + Unix socket server
  update.rs            Auto-update from GitHub releases
app/src-tauri/         Tauri desktop app (shares the library)
docs/                  Astro docs site
```

## Docs Site

```bash
make docs              # dev server at http://localhost:4321
make docs-build        # build to docs/dist/
```

## Releases

Releases are driven by git tags. The GitHub Actions workflow builds binaries and creates the release:

```bash
make publish VERSION=0.1.0
```

This creates a `release/<version>` branch, bumps `Cargo.toml`, tags, and pushes. The CI handles the rest.
