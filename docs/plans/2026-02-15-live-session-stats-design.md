# Live Session Stats Design

## Goal

Show live file changes and token usage in the session detail panel, updated by polling.

## 1. Live File Stats (git diff polling)

The TUI spawns a background thread every ~5 seconds that runs `git diff --stat` in each active session's worktree. Results return via `mpsc` channel (same pattern as PR merge polling). On receipt, `update_session_git_stats()` persists to SQLite. The existing 250ms `refresh_data()` picks it up; `draw_session_detail` already renders `files_changed/lines_added/lines_removed`.

### Changes

- `app.rs`: Add `maybe_poll_git_stats()` + `poll_git_stats_results()` using `AtomicBool` + `mpsc` + `Instant` pattern
- `app.rs`: Call both in the `Tick` handler
- No UI changes needed — data already rendered

## 2. Token Usage (stop hook + session-update)

The stop hook already runs after each Claude turn. We add JSONL parsing to extract cumulative token usage and pass it to `claustre session-update` via new CLI flags. The handler calls the existing `update_task_usage()` store method. The session detail panel gets a new line showing tokens/cost from the selected task.

### Changes

- `session/mod.rs` (stop hook script): Add JSONL parsing for token extraction
- `main.rs` (`SessionUpdate`): Add `--input-tokens`, `--output-tokens`, `--cost` CLI args; call `update_task_usage()`
- `store/queries.rs`: Remove `#[allow(dead_code)]` from `update_task_usage`
- `ui.rs` (`draw_session_detail`): Add tokens + cost line from selected task
