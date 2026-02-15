# Task-Centric Session Detail

## Problem

The TUI has three focus panels (Projects, Sessions, Tasks) but sessions are not independently useful. Users interact with tasks — sessions are implementation details tied to tasks. The Session Detail pane currently shows whichever session is selected by an independent `session_index`, disconnected from the selected task.

## Design

### Remove `Focus::Sessions`

- `Focus` enum: `Projects | Tasks` (drop `Sessions`)
- Keys: `1` = Projects, `2` = Tasks
- Tab cycles between Projects and Tasks only
- Remove `session_index`, `selected_session()`, `NewSession` input mode
- Remove session-specific key handlers: `s` for new session (keep subtask panel), `d` on sessions, `Enter` on sessions

### Session Detail driven by selected task

`draw_session_detail()` resolves the session from the selected task's `session_id`:

- **Task has session**: show branch, status, message, files changed, last activity, PR URL
- **Task has no session (pending)**: show "No session — press l to launch"
- **No task selected**: show "No tasks"

### Keyboard changes

| Key | Before | After |
|-----|--------|-------|
| `1` | Focus Projects | Focus Projects |
| `2` | Focus Sessions | Focus Tasks |
| `3` | Focus Tasks | Removed |
| `Tab` | Cycle P→S→T | Cycle P→T |
| `Enter` on Sessions | Jump to Zellij tab | N/A (no sessions focus) |
| `Enter` on Tasks | No-op | No-op |
| `s` (non-task focus) | New session form | No-op |
| `d` on Sessions | Delete session | N/A |

### Layout unchanged

- Left: Projects (35%)
- Right: Usage bars + Session Detail (task-driven) + Task Queue (65%)

### Command palette cleanup

Remove `NewSession`, `FocusSessions` actions from the command palette.
