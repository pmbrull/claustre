# Task-Driven Session Creation

**Date:** 2026-02-14

## Problem

Currently, autonomous tasks always create a new session (worktree + Zellij tab) immediately. There's no way to queue multiple autonomous tasks into one session for sequential execution. The per-task `in_review` state doesn't make sense when tasks share a worktree — changes are cumulative and the review unit should be the session (branch), not individual tasks.

## Design

### Core Changes

1. **Tasks get a `needs_new_session` flag** — when creating a task, the user picks:
   - **New session** (`true`): task gets its own worktree/tab → runs in parallel
   - **Default session** (`false`): task queues into the project's default session → runs sequentially

2. **Remove `InReview` from `TaskStatus`** — tasks go `pending → in_progress → done` (+ `error`). Review happens at the session level, not per-task.

3. **Session-level review** — when all tasks in a session are `done` and no more are pending, `ClaudeStatus::Done` signals "needs review." The user reviews the whole branch/PR, then tears down the session.

### Default Session

Each project can have one "default session" — a long-running session that tasks queue into.

- Identified by convention: `branch_name = 'default'` and `closed_at IS NULL`
- Created lazily when the first `needs_new_session = false` task is submitted
- If the default session was closed, a fresh one is created

### Behavior Matrix

| Mode | needs_new_session | On task creation |
|------|-------------------|------------------|
| Autonomous | true | Create new session + launch Claude immediately (current behavior) |
| Autonomous | false | Find/create default session, assign task. If session idle → `feed_next_task` launches it. If busy → queues behind current. |
| Supervised | true | Just create the task (current behavior) |
| Supervised | false | Assign to default session (create if needed). User navigates to it. |

### Task Lifecycle (simplified)

```
pending → in_progress → done
                     \→ error
```

- `in_progress`: set when a session starts working on the task
- `done`: set by `claustre_task_done` MCP tool (was `in_review`, now goes straight to `done`)
- `error`: set on failure

### Session Review

- `ClaudeStatus::Done` on a session = "all work complete, review this branch"
- The `r` key in TUI operates on sessions: tear down / mark reviewed
- `has_review_tasks()` query becomes `has_review_sessions()` — checks for sessions with `claude_status = 'done'`

### Data Model Changes

**Migration v5:**
```sql
ALTER TABLE tasks ADD COLUMN needs_new_session INTEGER NOT NULL DEFAULT 1;
```

**Remove `InReview` from `TaskStatus` enum.** Existing `in_review` rows in the DB get treated as `done` (add fallback in `FromStr`).

### TUI Form Changes

Task form gets a 3rd field:

- Field 0: Prompt (text input)
- Field 1: Mode (supervised ↔ autonomous)
- Field 2: Session (new ↔ default) — toggle with ←/→

### Files Changed

| File | Change |
|------|--------|
| `src/store/mod.rs` | Migration v5 |
| `src/store/models.rs` | Remove `InReview` from `TaskStatus`, add `needs_new_session` to `Task` |
| `src/store/queries.rs` | Update task CRUD, add `get_default_session()`, remove `has_review_tasks()`, add `has_review_sessions()` |
| `src/session/mod.rs` | Add default session find/create logic |
| `src/tui/app.rs` | 3-field form, session-level review key, launch logic for default session |
| `src/tui/ui.rs` | Render session field in task form, update review indicators |
| `src/mcp/mod.rs` | `claustre_task_done` → mark task `done` instead of `in_review` |
| `src/config/mod.rs` | Update CLAUDE.md instructions (remove `in_review` references) |
