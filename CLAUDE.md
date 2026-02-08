# Claustre Development Guide

## Build & Test

```bash
cargo build              # Debug build
cargo build --release    # Release build
cargo test               # Run all tests (skills, store, config modules)
cargo clippy             # Lint (strict: clippy::all denied, pedantic warned)
cargo fmt --check        # Check formatting
```

The project must compile cleanly with zero clippy warnings before committing.

## Architecture Overview

Single-binary Rust application. Six modules, one responsibility each:

| Module       | Purpose                                            |
|--------------|----------------------------------------------------|
| `main.rs`    | CLI entry point (clap). Dispatches to TUI or subcommands |
| `config/`    | Config loading (`config.toml`), `CLAUDE.md` merge, path helpers |
| `store/`     | SQLite database: schema, models, CRUD queries      |
| `tui/`       | ratatui terminal UI: app state, event loop, rendering |
| `session/`   | Git worktree + Zellij tab lifecycle                |
| `mcp/`       | Async MCP server (tokio, Unix socket, JSON-RPC 2.0) |
| `skills/`    | skills.sh CLI wrapper, ANSI parser                 |

## Key Patterns

### Two SQLite connections

The TUI and MCP server each have their own `Store` (SQLite `Connection`). The MCP server's store is wrapped in `Arc<Mutex<Store>>` and accessed via `store.lock().await`. This avoids the TUI blocking on MCP writes. **Never share a single connection across threads.**

### State refresh via polling

The TUI runs a 250ms tick. On each tick, `refresh_data()` re-queries the database to pick up any changes from the MCP server. This is simpler than cross-thread channels and good enough for dashboard latency.

### Pre-fetched sidebar summaries

`build_project_summaries()` queries session/task data for all projects up front and stores it in a `HashMap<String, ProjectSummary>`. This avoids N+1 queries during rendering.

### Config inheritance

Worktree config is assembled at session creation time in `session::write_merged_config()`:
- CLAUDE.md: global + project + repo merged in order
- Hooks: global copied first, project hooks override by filename

### Task lifecycle

```
pending -> in_progress -> in_review -> done
                       \-> error
```

- `in_progress` is set when a session starts working on the task
- `in_review` is set by the MCP `claustre_task_done` tool
- `done` is set manually by the user pressing `r` in the TUI
- Autonomous tasks auto-queue: when one finishes, `feed_next_task()` sends the next prompt to Zellij

### MCP transport

Content-Length framed JSON-RPC 2.0 over a Unix socket. The socket lives at `~/.claustre/mcp.sock`. Each worktree's `.mcp.json` uses `claustre mcp-bridge` to bridge stdio to the socket. Session ID is passed via `CLAUSTRE_SESSION_ID` env var.

## Rust Edition & Style

- **Edition 2024** -- uses let-chains (`if let Some(x) = ... && condition`) and other 2024 features
- **Clippy**: `all` denied, `pedantic` warned, with selected pedantic lints allowed (see `Cargo.toml` `[lints.clippy]`)
- **Error handling**: `anyhow::Result` everywhere. Use `.context()` for actionable error messages
- **No `unwrap()`** in production paths. `unwrap()` is acceptable only in tests or when a regex/constant is known-valid (use `expect()` with a reason)
- `#[expect(dead_code, reason = "...")]` instead of `#[allow(dead_code)]` for intentional dead code

## Module Details

### store/

- `mod.rs` -- `Store` struct (wraps `rusqlite::Connection`), `open()`, `migrate()`
- `models.rs` -- `Project`, `Task`, `Session`, enums (`TaskStatus`, `TaskMode`, `ClaudeStatus`), `ProjectStats`
- `queries.rs` -- all CRUD operations as `impl Store` methods

Schema uses versioned migrations via `MIGRATIONS` array in `mod.rs`. A `schema_version` table tracks the current version. Legacy databases (pre-migration system) are detected by checking for existing tables and auto-stamped as v1. To add a new migration, append a `Migration` to the `MIGRATIONS` array with the next version number.

### tui/

- `mod.rs` -- `run()` initializes terminal and starts app loop
- `app.rs` -- `App` state struct, all key handlers, data refresh logic
- `event.rs` -- crossterm event polling with tick-rate support
- `ui.rs` -- all rendering functions (`draw_active`, `draw_history`, `draw_skills`, etc.)

The `App` struct holds all mutable state. `View` (Active/History/Skills), `Focus` (Projects/Sessions/Tasks), and `InputMode` (Normal/NewTask/NewSession/CommandPalette/SkillSearch/SkillAdd) form the state machine.

### session/

Manages the full lifecycle:
1. `create_session()` -- worktree + config + Zellij tab + MCP config + optional Claude launch
2. `teardown_session()` -- capture git stats + close tab + remove worktree + close in DB
3. `goto_session()` -- switch to Zellij tab
4. `feed_next_task()` -- auto-queue next autonomous task

Shell commands are run via `std::process::Command`. The `shell_escape()` helper handles single-quote escaping for prompts sent to Zellij.

### mcp/

Async server using `tokio::net::UnixListener`. Each connection is spawned as a task. The protocol is MCP (JSON-RPC 2.0) with Content-Length framing.

Four tools: `claustre_status`, `claustre_task_done`, `claustre_usage`, `claustre_log`.

### skills/

Wraps `npx skills` CLI commands. Parses ANSI-colored output using a static `LazyLock<Regex>`. All parsing functions have unit tests.

## Gotchas

1. **Must run inside Zellij** -- session creation calls `zellij action new-tab`. If you're not in a Zellij session, this fails silently or errors out.

2. **claustre must be in PATH** -- the MCP bridge uses `claustre mcp-bridge` (invoked by Claude Code via `.mcp.json`). If claustre isn't in PATH, Claude sessions can't report back.

3. **Stale socket** -- the MCP server cleans up `~/.claustre/mcp.sock` on start, but if the process crashes, the stale socket may prevent restart. Delete it manually if needed.

4. **SQLite WAL mode** -- both connections use `PRAGMA journal_mode=WAL`. This allows concurrent reads and writes but means you'll see `.db-wal` and `.db-shm` files alongside the database. Don't delete them while claustre is running.

5. **Versioned migrations** -- the schema uses a `schema_version` table and a `MIGRATIONS` array. Legacy databases are auto-detected and stamped as v1. New migrations append to the array. Always test with both fresh and existing databases.

6. **Worktree cleanup** -- `teardown_session()` force-removes worktrees (`git worktree remove --force`). If a worktree has uncommitted changes, they will be lost.

7. **skills.sh dependency** -- the skills module shells out to `npx skills`. This requires Node.js and a network connection for `find`/`add`/`update`. The TUI won't crash if npx is missing, but skills operations will fail.

8. **Task index uses `visible_tasks()`** -- in the Active view, `visible_tasks()` filters out `Done` tasks. All navigation, selection, and rendering use this method so `task_index` always refers to the visible list.

9. **Notification fire-and-forget** -- `NotificationConfig::notify()` spawns the command and doesn't wait. If the command fails, it logs a warning but doesn't surface it to the user.

10. **MCP lock contention** -- the `SharedStore` uses `tokio::sync::Mutex`. Each MCP tool call holds the lock for the duration of its DB operations. Keep tool handlers fast.
