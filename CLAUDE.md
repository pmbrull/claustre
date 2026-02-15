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

## Entity Model

### Relationships

```
Project 1──* Task 1──* Subtask
Project 1──* Session
Task *──0..1 Session (assigned via session_id FK)
```

- **Project** — a git repository registered in claustre. Has a `name` and `repo_path`.
- **Task** — a unit of work belonging to a project. Has a `title`, `description`, `status`, `mode` (autonomous/supervised), and an optional `session_id` linking it to the session executing it. Tracks token usage (`input_tokens`, `output_tokens`, `cost`) and timing (`started_at`, `completed_at`). Tasks within a project are ordered by `sort_order`.
- **Subtask** — an optional breakdown of a task into sequential steps. Each subtask has its own `status` lifecycle. When a task has subtasks, they are fed to Claude one at a time in `sort_order`.
- **Session** — a running Claude Code instance tied to a project. Maps 1:1 to a git worktree + Zellij tab. Tracks `claude_status` (idle/working/waiting_for_input/done/error), `status_message`, and git diff stats (`files_changed`, `lines_added`, `lines_removed`). A session is "active" while `closed_at IS NULL`.
- **RateLimitState** — singleton row tracking whether Claude is rate-limited, which window (5h/7d), usage percentages, and reset time. Used to pause autonomous task feeding.

### Task modes

- **Autonomous** — claustre launches Claude with the prompt and appends `AUTONOMOUS_SUFFIX` telling Claude to work without user input. When the task completes, `feed_next_task()` auto-starts the next pending autonomous task on the same session.
- **Supervised** — claustre launches Claude with the prompt but the user drives the interaction. No auto-queuing.

### Task status lifecycle

```
pending ──[launch]──> in_progress ──[claustre_task_done]──> in_review ──[user 'r']──> done
                                   \──[error]──> error
```

| Transition | Trigger | Where |
|---|---|---|
| `pending → in_progress` | User presses `l` (launch) in TUI, or `feed_next_task()` auto-queues | `session::create_session()`, `session::feed_next_task()` |
| `in_progress → in_review` | Claude calls `claustre_task_done` MCP tool (or `claustre_status` with `done`) | `mcp/mod.rs` handler |
| `in_review → done` | User presses `r` in TUI (also tears down session) | `tui/app.rs` key handler |
| `in_progress → error` | External/manual (no automatic trigger yet) | — |

### Subtask lifecycle

When a task has subtasks, the first pending subtask is launched instead of the task description. On each `claustre_task_done` call:
1. The current in-progress subtask is marked `done`
2. If another subtask is pending, it's marked `in_progress` and fed to the session (parent task stays `in_progress`)
3. If no more subtasks remain, the parent task transitions to `in_review`

### Session status (`ClaudeStatus`)

Tracks what Claude is doing right now, reported by Claude via MCP:

| Status | Meaning | Set by |
|---|---|---|
| `idle` | Default, session just created | DB default |
| `working` | Claude is actively processing | `claustre_status` MCP tool, or `create_session()`/`feed_next_task()` on launch |
| `waiting_for_input` | Claude needs user input | `claustre_status` MCP tool |
| `done` | Claude finished the task | `claustre_task_done` or `claustre_status` with `done` |
| `error` | Something went wrong | `claustre_status` MCP tool |

## Communication Architecture

### TUI ↔ MCP Server (via SQLite)

The TUI and MCP server communicate **indirectly through the SQLite database**. There are no channels or direct messages between them.

```
┌─────────┐  writes   ┌──────────┐  reads    ┌─────────┐
│ Claude   │ ───MCP──> │  SQLite  │ <──poll── │   TUI   │
│ Session  │           │   (WAL)  │           │  (250ms │
│ (worktree│           │          │ ──writes─>│   tick)  │
│  + Zellij│           │          │           │         │
│  tab)    │           │          │           │         │
└─────────┘           └──────────┘           └─────────┘
```

- **MCP → DB**: Claude calls MCP tools (`claustre_status`, `claustre_task_done`, `claustre_usage`, etc.) which write session/task state to SQLite via the MCP server's own `Store` connection.
- **DB → TUI**: Every 250ms the TUI calls `refresh_data()` which re-queries all projects, sessions, and tasks from SQLite, picking up any MCP-written changes.
- **TUI → DB**: User actions (launch task, mark done, delete, create) write directly to SQLite via the TUI's own `Store` connection.

### MCP Bridge (stdio ↔ Unix socket)

Claude Code in each worktree connects to claustre's MCP server via a stdio bridge:

```
Claude Code ──stdio──> claustre mcp-bridge ──Unix socket──> MCP server
                       (injects session_id)
```

1. Each worktree has a `.mcp.json` pointing to `claustre mcp-bridge`
2. The bridge reads `CLAUSTRE_SESSION_ID` from its environment (set in `.mcp.json`)
3. On every `tools/call` request, the bridge injects `session_id` into the arguments — Claude never needs to know its own session ID
4. Responses flow back unmodified from MCP server through the bridge to Claude

### MCP Tools (Claude → Claustre)

Six tools available to Claude sessions:

| Tool | Purpose | DB Effect |
|---|---|---|
| `claustre_status` | Report current state + message | Updates `sessions.claude_status` + `status_message` |
| `claustre_task_done` | Signal task complete (must commit + PR first) | Marks subtask/task `in_review`, triggers `feed_next_task()` for autonomous chains, fires notification |
| `claustre_usage` | Report token usage + cost | Increments `tasks.input_tokens`, `output_tokens`, `cost` |
| `claustre_log` | Send structured log message | Writes to tracing (info/warn/error) |
| `claustre_rate_limited` | Report rate limit hit | Sets `rate_limit_state`, pauses autonomous feeding |
| `claustre_usage_windows` | Report usage window percentages | Updates `rate_limit_state.usage_5h_pct` / `usage_7d_pct` |

### TUI User Actions (User → Claustre)

Key actions in normal mode (Active view):

| Key | Action | Effect |
|---|---|---|
| `l` | Launch task | Creates session (worktree + Zellij tab + MCP config), assigns task, launches Claude |
| `r` | Review/mark done | Tears down session (worktree + tab), marks task `done` |
| `n` | New task | Opens task creation form |
| `e` | Edit task | Opens edit form (pending tasks only) |
| `s` | Subtasks / New session | Opens subtask panel (tasks focus) or session creation (otherwise) |
| `d` | Delete | Confirmation dialog for project/session/task deletion |
| `Enter` | Go to session | Switches to session's Zellij tab |
| `o` | Open PR | Opens task's `pr_url` in browser |
| `J`/`K` | Reorder tasks | Swaps `sort_order` of adjacent tasks |

### Session Creation Flow

When user presses `l` on a pending task:

1. `create_worktree()` — `git worktree add` from the project repo
2. `write_merged_config()` — merges global + project CLAUDE.md, copies hooks
3. `create_zellij_tab()` — opens new Zellij tab with cwd set to worktree
4. `store.create_session()` — inserts session row in DB
5. `write_mcp_config()` — writes `.mcp.json` with `CLAUSTRE_SESSION_ID`
6. `pre_trust_worktree()` — seeds `~/.claude.json` to skip trust dialog
7. `launch_claude_in_zellij()` — types `claude '<prompt>'` into the Zellij pane
8. `return_to_claustre()` — switches Zellij focus back to the TUI tab

### Notification Flow

When a task transitions to `in_review` (via `claustre_task_done`), the MCP handler calls the `NotifyFn` callback. This is wired to `NotificationConfig::notify()` which fires a shell command (default: `say "completed {task}"` on macOS). The command is fire-and-forget — spawned without waiting.

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

Six tools: `claustre_status`, `claustre_task_done`, `claustre_usage`, `claustre_log`, `claustre_rate_limited`, `claustre_usage_windows`.

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
