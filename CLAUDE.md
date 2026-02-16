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

Single-binary Rust application. Five modules, one responsibility each:

| Module       | Purpose                                            |
|--------------|----------------------------------------------------|
| `main.rs`    | CLI entry point (clap). Dispatches to TUI or subcommands |
| `config/`    | Config loading (`config.toml`), `CLAUDE.md` merge, path helpers |
| `store/`     | SQLite database: schema, models, CRUD queries      |
| `tui/`       | ratatui terminal UI: app state, event loop, rendering |
| `session/`   | Git worktree + Zellij tab lifecycle                |
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
- **Subtask** — an optional breakdown of a task into steps. When a task has subtasks, they are all included in the prompt as an ordered list (Claude works through them sequentially).
- **Session** — a running Claude Code instance tied to a project. Maps 1:1 to a git worktree + Zellij tab. Tracks `claude_status` (idle/working/done/error), `status_message`, and git diff stats (`files_changed`, `lines_added`, `lines_removed`). A session is "active" while `closed_at IS NULL`.
- **RateLimitState** — singleton row tracking usage percentages and rate limit windows. Updated by the TUI's OAuth API polling.

### Task modes

- **Autonomous** — claustre launches `claustre feed-next --session-id X` in the Zellij tab. `feed-next` runs Claude as a blocking subprocess, then loops to chain the next pending autonomous task. The Stop hook fires after each Claude turn to update session/task state.
- **Supervised** — claustre launches `claude '<prompt>'` directly in the Zellij tab. The user drives the interaction. The Stop hook still fires to update session state.

### Task status lifecycle

```
pending ──[launch]──> in_progress ──[Stop hook detects PR]──> in_review ──[PR merged or user 'r']──> done
                                   \──[error]──> error
```

| Transition | Trigger | Where |
|---|---|---|
| `pending → in_progress` | User presses `l` (launch) in TUI, or `feed-next` picks up next task | `session::create_session()`, `main::run_feed_next()` |
| `in_progress → in_review` | Stop hook detects a PR via `gh pr view` and calls `claustre session-update --pr-url` | `main.rs` `SessionUpdate` handler |
| `in_review → done` | PR merge poller detects merge (auto), or user presses `r` (manual). Both tear down the session. | `tui/app.rs` `poll_pr_merge_results()`, key handler |
| `in_progress → error` | External/manual (no automatic trigger yet) | — |

### Subtask handling

When a task has subtasks, `feed-next` builds a prompt that includes all subtasks as an ordered list. Claude works through them sequentially in a single session. Subtask DB model is retained for organizational/display purposes in the TUI.

### Session status (`ClaudeStatus`)

Tracks what Claude is doing right now, updated by the Stop hook:

| Status | Meaning | Set by |
|---|---|---|
| `idle` | No in-progress task assigned | DB default, Stop hook (only when no task is active) |
| `working` | Claude is actively processing a task | `create_session()` on launch, `feed-next` on task start |
| `done` | Claude finished the task (PR detected) | Stop hook (when PR detected via `session-update`) |
| `error` | Something went wrong | Manual |

## Communication Architecture

### Hooks + CLI (no MCP)

Claustre uses Claude Code's Stop hook and CLI subcommands instead of an MCP server:

```
┌─────────┐  Stop hook  ┌──────────────────┐  writes   ┌──────────┐  reads    ┌─────────┐
│ Claude   │ ──fires──>  │ claustre         │ ────────> │  SQLite  │ <──poll── │   TUI   │
│ Session  │             │ session-update   │           │   (WAL)  │           │  (1s)   │
│ (worktree│             │ (sets idle,      │           │          │           │         │
│  + Zellij│             │  detects PR)     │           │          │           │         │
│  tab)    │             └──────────────────┘           └──────────┘           └─────────┘
└─────────┘
```

**Supervised tasks:**
1. `create_session()` types `claude '<prompt>'` into the Zellij pane
2. Stop hook fires after each Claude turn → `claustre session-update` → SQLite
3. TUI polls SQLite every 1s

**Autonomous task chains:**
1. `create_session()` types `claustre feed-next --session-id X` into the Zellij pane
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
- The main tick loop picks it up: tears down the session (worktree + Zellij tab), marks the task `done`, and shows a toast
- Uses an `AtomicBool` flag to prevent overlapping polls (same pattern as usage fetch)

### Hooks

Each worktree gets two hooks registered in `.claude/settings.local.json` (not `.claude/settings.json` — see gotcha below). Both source a shared `_claustre-common.sh` helper.

**`TaskCompleted` hook** (primary) — fires each time Claude marks an internal task as completed:
1. Reads Claude's internal task progress from `~/.claude/tasks/<session_id>/` and writes `progress.json` to `~/.claustre/tmp/<session_id>/`
2. Extracts cumulative token usage from Claude's JSONL conversation log
3. Calls `claustre session-update --session-id <ID> [--input-tokens N --output-tokens N --cost F]`

**`Stop` hook** (final validation) — fires when Claude finishes responding:
1. Reads task progress and writes `progress.json` (catch-all for anything `TaskCompleted` missed)
2. Extracts cumulative token usage (final update)
3. Checks for an open PR on the current branch via `gh pr view`
4. Calls `claustre session-update --session-id <ID> [--pr-url <URL>] [--input-tokens N --output-tokens N --cost F]`

The `TaskCompleted` hook handles incremental progress sync so the TUI reflects task status changes immediately. The `Stop` hook acts as a final sweep and is the only one that detects PRs (since PR creation happens at the end of Claude's work, not mid-task).

### CLI Subcommands (orchestration)

| Command | Purpose | Effect |
|---|---|---|
| `claustre session-update` | Called by Stop hook | Sets session idle, optionally transitions task to `in_review` if PR URL provided |
| `claustre feed-next` | Autonomous task chain runner | Blocking loop: assigns task → runs Claude → checks result → loops |

### TUI User Actions (User → Claustre)

Key actions in normal mode (Active view):

| Key | Action | Effect |
|---|---|---|
| `l` | Launch task | Creates session (worktree + Zellij tab + hooks), assigns task, launches Claude or feed-next |
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
5. Write `.claustre_session_id` + Stop hook into the worktree
6. `pre_trust_worktree()` — seeds `~/.claude.json` to skip trust dialog
7. Launch Claude: `claude '<prompt>'` (supervised) or `claustre feed-next --session-id X` (autonomous)
8. `return_to_claustre()` — switches Zellij focus back to the TUI tab

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

Schema uses versioned migrations via `MIGRATIONS` array in `mod.rs`. A `schema_version` table tracks the current version. Legacy databases (pre-migration system) are detected by checking for existing tables and auto-stamped as v1. To add a new migration, append a `Migration` to the `MIGRATIONS` array with the next version number.

### tui/

- `mod.rs` -- `run()` initializes terminal and starts app loop
- `app.rs` -- `App` state struct, all key handlers, data refresh logic
- `event.rs` -- crossterm event polling with tick-rate support
- `ui.rs` -- all rendering functions (`draw_active`, `draw_history`, `draw_skills`, etc.)

The `App` struct holds all mutable state. `View` (Active/History/Skills), `Focus` (Projects/Sessions/Tasks), and `InputMode` (Normal/NewTask/NewSession/CommandPalette/SkillSearch/SkillAdd) form the state machine.

### session/

Manages the full lifecycle:
1. `create_session()` -- worktree + config + Zellij tab + hooks + Claude launch
2. `teardown_session()` -- capture git stats + close tab + remove worktree + close in DB
3. `goto_session()` -- switch to Zellij tab

Shell commands are run via `std::process::Command`. The `shell_escape()` helper handles single-quote escaping for prompts sent to Zellij.

### skills/

Wraps `npx skills` CLI commands. Parses ANSI-colored output using a static `LazyLock<Regex>`. All parsing functions have unit tests.

## Gotchas

1. **Must run inside Zellij** -- session creation calls `zellij action new-tab`. If you're not in a Zellij session, this fails silently or errors out.

2. **claustre must be in PATH** -- the `feed-next` subcommand and Stop hook both invoke `claustre` by name. If claustre isn't in PATH, autonomous chains and session updates won't work.

3. **Stop hook requires `gh` CLI** -- PR detection uses `gh pr view`. If `gh` is not installed or not authenticated, the Stop hook can't detect PRs and tasks won't auto-transition to `in_review`.

4. **SQLite WAL mode** -- the connection uses `PRAGMA journal_mode=WAL`. This allows concurrent reads and writes but means you'll see `.db-wal` and `.db-shm` files alongside the database. Don't delete them while claustre is running.

5. **Versioned migrations** -- the schema uses a `schema_version` table and a `MIGRATIONS` array. Legacy databases are auto-detected and stamped as v1. New migrations append to the array. Always test with both fresh and existing databases.

6. **Worktree cleanup** -- `teardown_session()` force-removes worktrees (`git worktree remove --force`). If a worktree has uncommitted changes, they will be lost.

7. **skills.sh dependency** -- the skills module shells out to `npx skills`. This requires Node.js and a network connection for `find`/`add`/`update`. The TUI won't crash if npx is missing, but skills operations will fail.

8. **Task index uses `visible_tasks()`** -- in the Active view, `visible_tasks()` filters out `Done` tasks. All navigation, selection, and rendering use this method so `task_index` always refers to the visible list.

9. **Notification fire-and-forget** -- `NotificationConfig::notify()` spawns the command and doesn't wait. If the command fails, it logs a warning but doesn't surface it to the user.

10. **`feed-next` is fully synchronous** -- it runs Claude as a blocking subprocess. No tokio or async runtime. The Stop hook writes to SQLite from a separate process, so there's no lock contention.

11. **Hook settings must use `settings.local.json`** -- Claude Code has three settings files for hooks: `~/.claude/settings.json` (global), `.claude/settings.json` (project, shareable), and `.claude/settings.local.json` (project, local-only). In practice, hooks defined in `.claude/settings.json` do **not** get executed — only `~/.claude/settings.json` and `.claude/settings.local.json` work. The `write_stop_hook()` function must write to `.claude/settings.local.json`, not `.claude/settings.json`. Additionally, always include `"matcher": ""` in the hook group to match the format used by working global hooks. Claude Code snapshots hooks at session startup, so changes to settings files after launch won't take effect until the next Claude Code session.

12. **Debugging must use the `claustre` Zellij session** -- claustre runs inside its own Zellij session named `claustre`. When debugging session/hook issues, always target the `claustre` Zellij session (e.g. `zellij --session claustre action ...`), not the current development Zellij session.

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

### 5. Dump the session tab to see Claude's state

```bash
zellij --session claustre action dump-screen --full /tmp/session-dump.txt
```

Check if Claude is still running (mid-turn) or finished (at the `❯` prompt).
