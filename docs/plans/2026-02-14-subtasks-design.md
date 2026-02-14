# Subtasks Design

**Date:** 2026-02-14

## Problem

The current model ties tasks to sessions loosely. We want:
- Each task = one session (1:1) = one branch/PR = one review unit
- Ability to break a task into subtasks that run sequentially in the same session
- Small, organized units of work that build on each other

## Design

### Data Model

**New table: `subtasks`**

```sql
CREATE TABLE subtasks (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id),
    title TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'pending',
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    started_at TEXT,
    completed_at TEXT
);
```

Subtask statuses: `pending`, `in_progress`, `done`, `error` (same as tasks, minus `in_review`).

### Task Lifecycle (unchanged)

```
pending → in_progress → in_review → done
                     \→ error
```

Keep `in_review` for tasks — it makes sense at the task level (= session/branch review). When all subtasks are done, the task goes to `in_review` automatically via `claustre_task_done`.

### Subtask Lifecycle

```
pending → in_progress → done
                     \→ error
```

### Behavior

**Task without subtasks:** Works exactly like today. One session, one unit of work.

**Task with subtasks:**
1. When the task is launched (autonomous or supervised), the first pending subtask is fed as the prompt
2. When Claude calls `claustre_task_done`, the current subtask is marked `done` and the next pending subtask is fed
3. When all subtasks are done, the task transitions to `in_review` (existing behavior)
4. The user reviews the whole branch/PR and marks the task as `done`

### MCP Changes

`claustre_task_done` behavior:
- If the task has subtasks: mark current subtask `done`, feed next subtask. If no more subtasks, mark task `in_review`.
- If the task has no subtasks: mark task `in_review` (current behavior).

### TUI Changes

- Tasks panel: show subtask count/progress (e.g., "3/5" next to task title)
- New key: `s` when focused on a task opens subtask management (add/view/reorder subtasks)
- Subtask panel: similar to tasks panel but simpler (just title + status)

### Files Changed

| File | Change |
|------|--------|
| `src/store/mod.rs` | Migration v5: create subtasks table |
| `src/store/models.rs` | Add `Subtask` struct |
| `src/store/queries.rs` | Subtask CRUD, update `claustre_task_done` logic |
| `src/mcp/mod.rs` | Update task_done to handle subtasks |
| `src/tui/app.rs` | Subtask management UI, subtask form |
| `src/tui/ui.rs` | Render subtask indicators and panel |
