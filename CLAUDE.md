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

Single-binary Rust application. Ten modules, one responsibility each:

| Module           | Purpose                                            |
|------------------|----------------------------------------------------|
| `main.rs`        | CLI entry point (clap). Dispatches to TUI or subcommands |
| `config/`        | Config loading (`config.toml`), `CLAUDE.md` merge, path helpers |
| `store/`         | SQLite database: schema, models, CRUD queries      |
| `tui/`           | ratatui terminal UI: app state, event loop, rendering |
| `session/`       | Git worktree lifecycle + session setup              |
| `pty/`           | Native PTY embedding via `portable-pty` + `vt100`  |
| `skills/`        | skills.sh CLI wrapper, ANSI parser                 |
| `scanner/`       | Passive scanner for external Claude Code sessions   |
| `session_host.rs`| Detached PTY owner + Unix socket server for session IPC |
| `update.rs`      | Auto-update: GitHub release check, download, rollback |

## Entity Model

### Relationships

```
Project 1──* Task 1──* Subtask
Project 1──* Session
Task *──0..1 Session (assigned via session_id FK)
```

- **Project** — a git repository registered in claustre. Has a `name` and `repo_path`.
- **Task** — a unit of work belonging to a project. Has a `title`, `description`, `status`, `mode` (autonomous/supervised/exploration), and an optional `session_id` linking it to the session executing it. Tracks token usage (`input_tokens`, `output_tokens`) and timing (`started_at`, `completed_at`). Has a `push_mode` (pr/push) controlling delivery method, optional `ci_status` (running/passed/failed), optional `branch` name, and a `review_loop` flag. Tasks within a project are ordered by `sort_order`.
- **Subtask** — an optional breakdown of a task into steps. When a task has subtasks, they are all included in the prompt as an ordered list (Claude works through them sequentially).
- **Session** — a running Claude Code instance tied to a project. Maps 1:1 to a git worktree + embedded terminal tab. Tracks `claude_status` (idle/working/done/error), `status_message`, and git diff stats (`files_changed`, `lines_added`, `lines_removed`). A session is "active" while `closed_at IS NULL`.
- **RateLimitState** — singleton row tracking usage percentages and rate limit windows. Updated by the TUI's OAuth API polling.
- **ExternalSession** — a Claude Code session discovered by the scanner module (not managed by claustre). Tracks project path, model, branch, token usage, and JSONL file path. Used to surface non-claustre Claude activity in the TUI.

### Task modes

- **Autonomous** — claustre launches `claustre feed-next --session-id X` in an embedded PTY. `feed-next` runs Claude as a blocking subprocess, then loops to chain the next pending autonomous task. The Stop hook fires after each Claude turn to update session/task state.
- **Supervised** — claustre launches `claude '<prompt>'` directly in an embedded PTY. The user drives the interaction. The Stop hook still fires to update session state.
- **Exploration** — same as supervised but intended for open-ended research/investigation tasks without a specific deliverable.

### Task status lifecycle

Full set of statuses: `draft`, `pending`, `working`, `interrupted`, `in_review`, `conflict`, `ci_failed`, `done`, `error`.

```
draft ──[user edits]──> pending ──[launch]──> working ──[Stop hook detects PR]──> in_review ──[PR merged or user 'r']──> done
                                                 ↑↓                                   │         \──[error]──> error
                                            interrupted                               │
                                                 ↑                                    │
                                                 └───[UserPromptSubmit: --resumed]────┘
```

| Transition | Trigger | Where |
|---|---|---|
| `pending → working` | User presses `l` (launch) in TUI, or `feed-next` picks up next task | `session::create_session()`, `main::run_feed_next()` |
| `working → in_review` | Stop hook detects a PR via `gh pr view` and calls `claustre session-update --pr-url` | `main.rs` `SessionUpdate` handler |
| `working → interrupted` | TUI detects session is no longer active (claustre restarted while Claude was running) | `tui/app.rs` |
| `interrupted → working` | Stop hook fires (proves Claude is still alive) or `--resumed` | `main.rs` `SessionUpdate` handler |
| `in_review → working` | `UserPromptSubmit` hook detects user activity and calls `session-update --resumed` | `main.rs` `SessionUpdate` handler |
| `in_review → done` | PR merge poller detects merge (auto), or user presses `r` (manual). Both tear down the session. | `tui/app.rs` `poll_pr_merge_results()`, key handler |
| `working → error` | External/manual (no automatic trigger yet) | — |

### Subtask handling

When a task has subtasks, `feed-next` builds a prompt that includes all subtasks as an ordered list. Claude works through them sequentially in a single session. Subtask DB model is retained for organizational/display purposes in the TUI.

### Session status (`ClaudeStatus`)

Tracks what Claude is doing right now, updated by the Stop hook:

| Status | Meaning | Set by |
|---|---|---|
| `idle` | No working task assigned | DB default, Stop hook (only when no task is active) |
| `working` | Claude is actively processing a task | `create_session()` on launch, `feed-next` on task start |
| `interrupted` | Session was active but claustre restarted (session-host may still be running) | TUI on restart detection |
| `paused` | Claude is waiting for user permission (tool approval) | TUI-only (detected from PTY screen, not persisted to DB) |
| `waiting` | Claude asked a question and awaits user answer (`AskUserQuestion`) | TUI-only (detected from PTY screen, not persisted to DB) |
| `done` | Claude finished the task (PR detected) | Stop hook (when PR detected via `session-update`) |
| `error` | Something went wrong | Manual |

**Paused detection:** The TUI scans each session's Claude PTY screen on every tick for Claude Code's permission prompt pattern ("Allow \<ToolName\>" + yes/no options). When detected and the session has a working task, the TUI overrides the displayed status to `paused` (⏸, yellow). This is purely in-memory — the DB still shows `working`. The override clears automatically when the screen no longer shows a permission prompt (e.g., user approved the action).

**Waiting detection:** Same mechanism as paused, but detects Claude Code's `AskUserQuestion` interactive selector pattern (❯ selection cursor + "Other" option). When detected, the TUI shows `waiting` (⏳, cyan). Clears when the user answers the question and the screen no longer shows the selector.

## Communication Architecture

### Hooks + CLI (no MCP)

Claustre uses Claude Code's Stop hook and CLI subcommands instead of an MCP server:

```
┌─────────┐  Stop hook  ┌──────────────────┐  writes   ┌──────────┐  reads    ┌─────────┐
│ Claude   │ ──fires──>  │ claustre         │ ────────> │  SQLite  │ <──poll── │   TUI   │
│ Session  │             │ session-update   │           │   (WAL)  │           │  (1s)   │
│ (embedded│             │ (sets idle,      │           │          │           │         │
│  PTY tab)│             │  detects PR)     │           │          │           │         │
│          │             └──────────────────┘           └──────────┘           └─────────┘
└─────────┘
```

**Supervised tasks:**
1. `create_session()` spawns `claude '<prompt>'` in an embedded PTY
2. Stop hook fires after each Claude turn → `claustre session-update` → SQLite
3. TUI polls SQLite every 1s

**Autonomous task chains:**
1. `create_session()` spawns `claustre feed-next --session-id X` in an embedded PTY
2. `feed-next` runs Claude as a blocking subprocess, loops for next task
3. Stop hook fires after each Claude turn → `claustre session-update` → SQLite
4. TUI polls SQLite every 1s

**Rate limits / usage:**
- TUI polls the Anthropic OAuth API via a background thread (`fetch_and_cache_usage`)
- Reads `~/.claude/statusline-cache.json` (shared with statusline script)
- `feed-next` checks the same cache before starting each task

**PR merge auto-completion:**
- Every 15 seconds, the TUI spawns a background thread that checks all `in_review` tasks with a `pr_url`
- For each task, runs `gh pr view <url> --json state --jq .state` and checks if the state is `MERGED`
- When a merge is detected, the result is sent back via `mpsc` channel
- The main tick loop picks it up: tears down the session (worktree + PTY tab), marks the task `done`, and shows a toast
- Uses an `AtomicBool` flag to prevent overlapping polls (same pattern as usage fetch)

### Hooks

Each worktree gets three hooks registered in `.claude/settings.local.json` (not `.claude/settings.json` — see gotcha below). The `TaskCompleted` and `Stop` hooks source a shared `_claustre-common.sh` helper.

**`TaskCompleted` hook** (progress sync) — fires each time Claude marks an internal task as completed:
1. Reads Claude's internal task progress from `~/.claude/tasks/<session_id>/` and writes `progress.json` to `~/.claustre/tmp/<session_id>/`
2. Calls `claustre session-update --session-id <ID>` (no token extraction — deferred to Stop hook)

**`Stop` hook** (final validation + usage) — fires when Claude finishes responding:
1. Reads task progress and writes `progress.json` (catch-all for anything `TaskCompleted` missed)
2. Extracts cumulative token usage from Claude's JSONL conversation log
3. Checks for an open PR on the current branch via `gh pr view`
4. Calls `claustre session-update --session-id <ID> [--pr-url <URL>] [--input-tokens N --output-tokens N]`

**`UserPromptSubmit` hook** (resume signal) — fires when the user sends a prompt:
1. Reads session ID from `.claustre_session_id`
2. Calls `claustre session-update --session-id <ID> --resumed`
3. If the session has an `in_review` task, transitions it back to `working` and sets session to `Working`

The `TaskCompleted` hook handles incremental progress sync so the TUI reflects task status changes immediately. Token extraction is deferred to the Stop hook to avoid redundant JSONL scans. The `Stop` hook acts as a final sweep — it's the only one that extracts token usage and detects PRs. The `UserPromptSubmit` hook provides instant resume detection when the user continues chatting on an `in_review` task.

All claustre sessions set `CLAUSTRE_SESSION=1` in the environment (via `settings.local.json` and process env). Global hooks can check this to skip token-wasting work in managed sessions.

### CLI Subcommands

| Command | Purpose |
|---|---|
| `claustre` / `claustre dashboard` | Launch the TUI (default) |
| `claustre init` | Initialize `~/.claustre/` directory |
| `claustre add-project <name> [path]` | Register a git repository |
| `claustre add-task <project> <title>` | Create a task (`-d` description, `-m` mode) |
| `claustre list-projects` | List all projects with session/task counts |
| `claustre list-tasks <project>` | List tasks for a project |
| `claustre stats <project>` | Show time/token usage stats |
| `claustre remove-project <project>` | Delete a project |
| `claustre export <project>` | Export tasks to JSON (`-o` output path) |
| `claustre skills [find\|add\|remove\|update]` | Manage skills (skills.sh integration) |
| `claustre feed-next --session-id <ID>` | Autonomous task chain runner (blocking loop) |
| `claustre session-update --session-id <ID>` | Called by hooks: sets session idle, transitions task state |
| `claustre session-host --session-id <ID>` | Detached PTY owner + Unix socket server |
| `claustre review-loop --session-id <ID>` | Monitor PR comments and implement feedback |
| `claustre health-check` | Verify binary is functional (used by auto-update) |
| `claustre rollback` | Revert to previous binary version after bad update |

### TUI User Actions (User → Claustre)

Key actions in normal mode (Active view):

| Key | Action | Effect |
|---|---|---|
| `l` | Launch task | Creates session (worktree + embedded PTY + hooks), assigns task, launches Claude or feed-next |
| `r` | Review/mark done | Tears down session (worktree + PTY tab), marks task `done` |
| `n` | New task | Opens task creation form |
| `e` | Edit task | Opens edit form (pending/draft tasks only) |
| `s` | Subtasks | Opens subtask panel for the selected task |
| `k` | Kill session | Tears down session, resets task to pending for re-launch |
| `a` | Add project | Opens project creation form |
| `i` | Skills | Opens skills management panel |
| `/` | Filter tasks | Opens task filter input |
| `?` | Help | Shows help overlay |
| `d` | Delete | Confirmation dialog for project/session/task deletion |
| `Enter` | Go to session | Switches to session's embedded terminal tab |
| `o` | Open PR | Opens task's `pr_url` in browser |
| `J`/`K` | Reorder tasks | Swaps `sort_order` of adjacent tasks |
| `Ctrl+P` | Command palette | Opens command search palette |

### Session Creation Flow

When user presses `l` on a pending task:

1. `create_worktree()` — `git worktree add` from the project repo
2. `write_merged_config()` — merges global + project CLAUDE.md, copies hooks
3. `store.create_session()` — inserts session row in DB
4. Write `.claustre_session_id` + Stop hook into the worktree
5. `pre_trust_worktree()` — seeds `~/.claude.json` to skip trust dialog
6. Return `SessionSetup` to TUI — contains session, claude_cmd, worktree_path, tab_label
7. TUI spawns `SessionTerminals` (shell + Claude PTYs) and adds a session tab

### Review Loop

When a task has `review_loop` enabled and transitions to `InReview`, the TUI spawns a `claustre review-loop --session-id <ID>` process in a new pane. This process:

1. Polls for the task's PR comments at a configurable interval (default 120s, set via `[review_loop] poll_interval_secs` in `config.toml`)
2. Runs Claude with the review prompt (built-in or custom via `[review_loop] prompt` in `config.toml`)
3. Claude evaluates comments adversarially, implements accepted ones, commits, and pushes
4. Loops until the task is done or rate limits are hit

Configuration in `~/.claustre/config.toml`:
```toml
[review_loop]
poll_interval_secs = 60       # default: 120
# prompt = "Custom prompt"    # default: built-in adversarial review prompt
```

### Notification Flow

When a task transitions to `in_review` (via `session-update` detecting a PR), the handler calls `NotificationConfig::notify()` which fires a shell command (default: `say "completed {task}"` on macOS). The command is fire-and-forget — spawned without waiting.

## Key Patterns

### State refresh via polling

The TUI runs a 1s tick. On each tick, `refresh_data()` re-queries the database to pick up any changes from the Stop hook / `session-update` / `feed-next`. This is simpler than cross-thread channels and good enough for dashboard latency.

### Pre-fetched sidebar summaries

`build_project_summaries()` queries session/task data for all projects up front and stores it in a `HashMap<String, ProjectSummary>`. This avoids N+1 queries during rendering.

### Config inheritance

Worktree config is assembled at session creation time in `session::write_merged_config()`:
- CLAUDE.md: global + project + repo merged in order
- Hooks: global copied first, project hooks override by filename

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

Schema uses versioned migrations via `MIGRATIONS` array in `mod.rs`. A `schema_version` table tracks the current version. Currently five migrations (v1–v5): v1 defines the core schema, v2 adds `external_sessions`, v3 adds `tasks.branch`, v4 adds `tasks.ci_status`, v5 adds `tasks.review_loop`. To add a new migration, append a `Migration` to the `MIGRATIONS` array with the next version number.

### tui/

- `mod.rs` -- `run()` initializes terminal and starts app loop
- `app.rs` -- `App` state struct, all key handlers, data refresh logic
- `event.rs` -- crossterm event polling with tick-rate support
- `ui.rs` -- all rendering functions (`draw_active`, `draw_history`, `draw_skills`, etc.)
- `form.rs` -- task/project form rendering and field layout
- `keymap.rs` -- declarative keybinding registry (`Action` enum, `KeyMap`, help entry generation)
- `theme.rs` -- color palette and styling constants

The `App` struct holds all mutable state. `Tab` (Dashboard/Session), `Focus` (Projects/Tasks), and `InputMode` (Normal/NewTask/EditTask/NewProject/ConfirmDelete/CommandPalette/SkillPanel/SkillSearch/SkillAdd/HelpOverlay/TaskFilter/SubtaskPanel) form the state machine. When `active_tab > 0`, keys are forwarded to the session's PTY instead of the dashboard key handlers. `Ctrl+K`/`Ctrl+J` navigate between tabs.

### session/

Manages the full lifecycle:
1. `create_session()` -- worktree + config + hooks + DB row → returns `SessionSetup`
2. `teardown_session()` -- capture git stats + remove worktree + close in DB

Shell commands are run via `std::process::Command`. The TUI handles tab removal before calling teardown.

### pty/

Native terminal embedding using `portable-pty` + `vt100`:
- `mod.rs` -- `EmbeddedTerminal` (PTY + vt100 parser + reader thread), `SessionTerminals` (tree-based pane layout), `LayoutNode`, `SplitDirection`, `PaneId`
- `widget.rs` -- `TerminalWidget` ratatui widget for rendering vt100 screens with proper color/attribute mapping
- `protocol.rs` -- Unix socket protocol (`HostMessage`/`ClientMessage`) for session-host IPC

Each session starts with at least two PTYs: a shell and a Claude process, arranged in a configurable tree layout (`LayoutNode`). Users can dynamically split any pane (right or down) to add more shells. Keystrokes are forwarded to the focused PTY via `send_bytes()`. A background reader thread drains PTY output into a channel, consumed on each tick by `process_output()`.

**Pane layout tree:** `SessionTerminals` uses a `HashMap<PaneId, PaneInfo>` for terminal storage and a `LayoutNode` tree (binary tree of `Split` and `Pane` nodes) for spatial arrangement. Splits can be horizontal (side-by-side) or vertical (stacked), each with a configurable ratio.

**Session keybindings:**

| Key | Action |
|---|---|
| `Ctrl+H` | Focus previous pane |
| `Ctrl+L` | Focus next pane |
| `Ctrl+R` | Split right (new shell beside focused) |
| `Ctrl+B` | Split down (new shell below focused) |
| `Ctrl+W` | Close focused pane (cannot close Claude pane or last pane) |
| `Ctrl+D` | Return to dashboard |
| `Ctrl+G` | Scroll to bottom (live screen) |
| `Shift+PgUp/PgDn` | Scroll page up/down |
| `Ctrl+J`/`Ctrl+K` | Switch tabs (also works in session) |

**Layout config:** The `[layout]` section in `~/.claustre/config.toml` defines the starting pane arrangement. Each leaf is `"shell"` or `"claude"` (exactly one `"claude"` required). When absent, defaults to horizontal 50/50 shell-left / claude-right.

### skills/

Wraps `npx skills` CLI commands. Parses ANSI-colored output using a static `LazyLock<Regex>`. All parsing functions have unit tests.

### scanner/

Passive scanner for external (non-claustre) Claude Code sessions. Discovers sessions by scanning `~/.claude/projects/` JSONL files, extracts token usage, model, timestamps, and project metadata. Only tracks active sessions (JSONL modified within 5 minutes). Skips claustre-managed sessions and unchanged files for efficiency. Results are upserted into the `external_sessions` table.

### session_host.rs

Detached PTY process host. Runs as a separate process (`claustre session-host`) that owns the actual PTY child and serves it over a Unix socket. Survives TUI restarts — the TUI reconnects to existing session-hosts on startup. Handles `HostMessage` (keystrokes, resize) from the TUI client and sends `ClientMessage` (PTY output frames) back.

### update.rs

Auto-update support. Checks GitHub releases for newer versions, downloads the appropriate binary, runs a smoke test (`health-check`), backs up the current binary to `~/.claustre/bin/claustre.prev`, and replaces it. Provides `claustre rollback` as manual escape hatch. Controlled by `auto_update = true` in `config.toml`.

## Gotchas

1. **claustre must be in PATH** -- the `feed-next` subcommand and Stop hook both invoke `claustre` by name. If claustre isn't in PATH, autonomous chains and session updates won't work.

2. **Stop hook requires `gh` CLI** -- PR detection uses `gh pr view`. If `gh` is not installed or not authenticated, the Stop hook can't detect PRs and tasks won't auto-transition to `in_review`.

3. **SQLite WAL mode** -- the connection uses `PRAGMA journal_mode=WAL`. This allows concurrent reads and writes but means you'll see `.db-wal` and `.db-shm` files alongside the database. Don't delete them while claustre is running.

4. **Versioned migrations** -- the schema uses a `schema_version` table and a `MIGRATIONS` array. New migrations append to the array with the next version number.

5. **Worktree cleanup** -- `teardown_session()` force-removes worktrees (`git worktree remove --force`). If a worktree has uncommitted changes, they will be lost.

6. **skills.sh dependency** -- the skills module shells out to `npx skills`. This requires Node.js and a network connection for `find`/`add`/`update`. The TUI won't crash if npx is missing, but skills operations will fail.

7. **Task index uses `visible_tasks()`** -- `visible_tasks()` returns all tasks including `Done` (shown dimmed at the bottom). All navigation, selection, and rendering use this method so `task_index` always refers to the visible list.

8. **Notification fire-and-forget** -- `NotificationConfig::notify()` spawns the command and doesn't wait. If the command fails, it logs a warning but doesn't surface it to the user.

9. **`feed-next` is fully synchronous** -- it runs Claude as a blocking subprocess. No tokio or async runtime. The Stop hook writes to SQLite from a separate process, so there's no lock contention.

10. **Hook settings must use `settings.local.json`** -- Claude Code has three settings files for hooks: `~/.claude/settings.json` (global), `.claude/settings.json` (project, shareable), and `.claude/settings.local.json` (project, local-only). In practice, hooks defined in `.claude/settings.json` do **not** get executed — only `~/.claude/settings.json` and `.claude/settings.local.json` work. The `write_stop_hook()` function must write to `.claude/settings.local.json`, not `.claude/settings.json`. Additionally, always include `"matcher": ""` in the hook group to match the format used by working global hooks. Claude Code snapshots hooks at session startup, so changes to settings files after launch won't take effect until the next Claude Code session.

## Documentation Maintenance

When changing features, adding/removing CLI subcommands, modifying keybindings, adding task statuses, or altering the architecture:

1. **Update this CLAUDE.md** — keep the module table, entity model, status lifecycle, CLI subcommands table, TUI key actions table, session keybindings, and gotchas in sync with the code.
2. **Update README.md** — keep the keybindings tables, review loop configuration, and quick start instructions current.
3. **Update migration count** — when adding a new schema migration, update the store/ section to reflect the new count and purpose.

Documentation must reflect the actual code. Outdated docs are worse than no docs.

## Debugging Stop Hook Failures

When a task doesn't transition to `in_review` after a PR is opened, follow these steps:

### 1. Check current state

```bash
# Task status and PR URL
sqlite3 ~/.claustre/claustre.db 'SELECT id, title, status, pr_url FROM tasks WHERE status NOT IN ("done");'

# Session status (should be "idle" after hook fires, "working" means hook never ran session-update)
sqlite3 ~/.claustre/claustre.db 'SELECT id, claude_status, status_message FROM sessions WHERE closed_at IS NULL;'
```

### 2. Check the hook debug log

The stop hook writes to `~/.claustre/hook-debug.log`. If the file is missing or has no entries for the session, the hook never executed. If it has entries, check whether `claustre session-update` was called and its exit code.

### 3. Check Claude Code's JSONL for hook execution

```bash
# Find the project dir (PWD hash)
ls ~/.claude/projects/ | grep <project-slug>

# Count stop hook events
grep "stop_hook_summary" ~/.claude/projects/<dir>/<session>.jsonl

# Check hook errors and timing
grep "stop_hook_summary" <jsonl-file> | python3 -c "import sys,json; [print(json.dumps({k:v for k,v in json.loads(l).items() if k in ('timestamp','hookCount','hookErrors','hasOutput')}, indent=2)) for l in sys.stdin]"
```

Key things to look for:
- **`hookErrors` not empty** — a hook failed
- **`hookCount` missing the stop hook** — `settings.local.json` wasn't loaded (check it exists at the worktree root's `.claude/` dir)
- **Only one `stop_hook_summary`** — the Stop hook fires once at the end of Claude's conversation, not per tool call
- **Time between `hook_progress` and `stop_hook_summary` matches timeout** — the hook group may have been killed

### 4. Manually run the hook to recover

```bash
cd <worktree-path> && bash -x .claude/hooks/stop-hook.sh
```

This will call `claustre session-update` and fix the DB state. The `-x` flag traces execution so you can see exactly where it fails.

### 5. Check session tab in the TUI

Switch to the session's terminal tab (press Enter on the session) to see if Claude is still running (mid-turn) or finished (at the `❯` prompt).
