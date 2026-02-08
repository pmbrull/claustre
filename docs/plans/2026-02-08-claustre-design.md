# Claustre — Design Document

## Overview

Claustre is a Rust TUI application for orchestrating multiple Claude Code sessions across projects. It uses git worktrees for isolation, Zellij for terminal management, and an MCP server for real-time status reporting. It provides a centralized dashboard to see what needs attention, a kanban-style task queue, and an inheritance-based config system for CLAUDE.md and hooks.

## Architecture

Four core subsystems, all in a single process:

1. **TUI layer** — ratatui-based dashboard with vim keybindings and a command palette (`ctrl+p`)
2. **MCP server** — async (tokio) JSON-RPC server on a unix socket that Claude sessions report status to
3. **Session manager** — orchestrates worktree creation, config merging, Zellij tab management, and Claude Code launching
4. **Task store** — SQLite database with optional per-project JSON export

## Data Model

### Project
| Field | Type | Description |
|-------|------|-------------|
| id | TEXT (UUID) | Primary key |
| name | TEXT | Display name |
| repo_path | TEXT | Absolute path to main repo |
| created_at | DATETIME | Creation timestamp |

### Task
| Field | Type | Description |
|-------|------|-------------|
| id | TEXT (UUID) | Primary key |
| project_id | TEXT | FK → Project |
| title | TEXT | Short description |
| description | TEXT | Full task description/prompt |
| status | TEXT | pending / in_progress / in_review / done / error |
| mode | TEXT | autonomous / supervised |
| session_id | TEXT (nullable) | FK → Session, assigned when picked up |
| created_at | DATETIME | Creation timestamp |
| updated_at | DATETIME | Last status change |
| started_at | DATETIME (nullable) | When work began |
| completed_at | DATETIME (nullable) | When work finished |
| input_tokens | INTEGER | Token usage (reported via MCP) |
| output_tokens | INTEGER | Token usage (reported via MCP) |
| cost | REAL | Estimated cost in USD |

### Session
| Field | Type | Description |
|-------|------|-------------|
| id | TEXT (UUID) | Primary key |
| project_id | TEXT | FK → Project |
| branch_name | TEXT | Git branch name |
| worktree_path | TEXT | Absolute path to worktree |
| zellij_tab_name | TEXT | For `zellij action go-to-tab-name` |
| claude_status | TEXT | idle / working / waiting_for_input / done / error |
| status_message | TEXT | What Claude is currently doing |
| last_activity_at | DATETIME | Updated by MCP heartbeats |
| files_changed | INTEGER | Git diff stat |
| lines_added | INTEGER | Git diff stat |
| lines_removed | INTEGER | Git diff stat |
| created_at | DATETIME | Creation timestamp |
| closed_at | DATETIME (nullable) | When session was torn down |

## Config Inheritance

Filesystem-based, not in SQLite:

```
~/.claustre/
├── config.toml          # claustre settings (keybindings, defaults, etc.)
├── claude.md            # global CLAUDE.md fragments
├── hooks/               # global hooks (pre-commit, etc.)
└── claustre.db          # SQLite database
```

Per-project overrides (optional):
```
{repo}/.claustre/
├── claude.md            # project-specific CLAUDE.md additions
├── hooks/               # project-specific hooks
└── tasks.json           # exported task snapshot (optional)
```

**Merge strategy**: Global config is the base. Project-level config extends it. For CLAUDE.md, project fragments are appended after global. For hooks, project hooks override global hooks with the same filename; unique hooks from both levels are included.

## MCP Server

**Transport**: Unix domain socket at `~/.claustre/mcp.sock`

**Tools exposed to Claude sessions**:

- `claustre_status(session_id, state, message)` — report current state (working/waiting_for_input/done/error) and a human-readable message
- `claustre_task_done(session_id, summary)` — signal task completion; claustre transitions task to `in_review`; if auto-queue has pending autonomous tasks, feeds the next one
- `claustre_usage(session_id, input_tokens, output_tokens, cost)` — report token usage for stats tracking
- `claustre_log(session_id, level, message)` — structured logging for review

When setting up a session, claustre writes `.mcp.json` into the worktree so Claude Code auto-connects.

## Session Lifecycle

### Setup
1. `git worktree add ~/.claustre/worktrees/{project}/{branch}` from the project repo
2. Merge config: assemble CLAUDE.md (global + project), copy hooks (global + project overrides)
3. Write `.mcp.json` pointing at `~/.claustre/mcp.sock` with the session ID
4. `zellij action new-tab --name "{project}:{branch}" --cwd {worktree_path}`

### Launch (per task mode)
- **Autonomous**: write `claude --prompt "..."` to the Zellij pane via `zellij action write`. Claude works independently.
- **Supervised**: leave the user at the shell. Dashboard shows task as `in_progress`, Claude status as `idle`.

### Auto-queue
When a task completes (`claustre_task_done`), if the session has more pending autonomous tasks, claustre writes the next prompt to the Zellij pane automatically. Supervised tasks pause — session shows as `in_review`.

### Teardown (user-initiated from TUI)
1. Capture final `git diff --stat`
2. Close Zellij tab via `zellij action close-tab`
3. `git worktree remove`
4. Update SQLite: mark session closed, task done

## TUI Layout

### Active View (default)

```
┌─────────────────────────────────────────────────────────┐
│  claustre                              ctrl+p: commands │
├──────────────────────┬──────────────────────────────────┤
│  PROJECTS            │  SESSION DETAIL                  │
│                      │                                  │
│  ▸ openmetadata [3]  │  Branch: fix/auth-bug            │
│    ● working         │  Status: ● working               │
│    ◐ in_review  ←!   │  Message: "refactoring auth..."  │
│    ○ idle            │  Files: 4 changed (+52 -18)      │
│                      │  Last activity: 12s ago          │
│  ▸ claustre [1]      │                                  │
│    ● working         │  Task: Fix auth token refresh    │
│                      │  Mode: autonomous                │
│  ▸ side-project [0]  │                                  │
│                      ├──────────────────────────────────┤
│                      │  TASK QUEUE                      │
│                      │                                  │
│                      │  ☐ Add retry logic        pending│
│                      │  ☐ Update API docs        pending│
│                      │  ☑ Fix auth bug        in_review│
│                      │                                  │
└──────────────────────┴──────────────────────────────────┘
```

### History View (toggle with `h` or `Tab`)

```
┌─────────────────────────────────────────────────────────┐
│  claustre — history                    ctrl+p: commands │
├──────────────────────┬──────────────────────────────────┤
│  PROJECTS            │  PROJECT STATS: openmetadata     │
│                      │                                  │
│  ▸ openmetadata      │  Total tasks:     47             │
│  ▸ claustre          │  Completed:       42             │
│  ▸ side-project      │  Sessions run:    18             │
│                      │  Total time:      14h 32m        │
│                      │  Tokens used:     2.1M           │
│                      │  Avg task time:   20m            │
│                      │                                  │
│                      ├──────────────────────────────────┤
│                      │  COMPLETED TASKS                 │
│                      │                                  │
│                      │  ✓ Fix auth bug         23m  12k│
│                      │  ✓ Add retry logic      45m  28k│
│                      │  ✓ Refactor DB layer  1h02m  54k│
│                      │  ✓ Update API docs      12m   8k│
│                      │  ...                             │
└──────────────────────┴──────────────────────────────────┘
```

### Keybindings
- `j/k` — navigate lists
- `Enter` — expand/collapse project, or jump to session's Zellij tab
- `n` — new task (inline form)
- `s` — new session (assign tasks, pick branch)
- `r` — review: mark `in_review` task as done or send back with feedback
- `h` / `Tab` — toggle active/history view
- `ctrl+p` — command palette
- `q` — quit / back

## Tech Stack

- **ratatui** + **crossterm** — TUI rendering and input
- **tokio** — async runtime for MCP server + background tasks
- **rusqlite** — SQLite access
- **serde** + **serde_json** / **toml** — serialization for config and MCP messages
- **uuid** — ID generation
- **chrono** — timestamps
- **clap** — CLI argument parsing (for non-TUI commands like `claustre add-project`)

## Implementation Phases

### Phase 1: Foundation
- Cargo project setup with workspace structure
- SQLite schema + migrations (rusqlite)
- Config file loading (TOML) and CLAUDE.md merge logic
- Core data types and store layer (CRUD for projects, tasks, sessions)

### Phase 2: TUI — Active View
- ratatui app scaffold with event loop
- Project list panel (left)
- Session detail panel (top-right)
- Task queue panel (bottom-right)
- Vim keybindings (j/k/Enter/q)
- Inline task creation form (`n`)

### Phase 3: Session Manager
- Git worktree create/remove
- CLAUDE.md + hooks merging into worktrees
- Zellij tab creation/navigation/close
- Session creation flow (`s` keybinding)
- Launch modes (autonomous vs supervised)

### Phase 4: MCP Server
- Unix socket server (tokio)
- MCP protocol: tool definitions and request handling
- `claustre_status`, `claustre_task_done`, `claustre_usage`, `claustre_log`
- Auto-write `.mcp.json` into worktrees
- Real-time dashboard updates from MCP events

### Phase 5: Auto-queue & Review Flow
- Auto-feed next task on completion
- `in_review` flow: mark done or send feedback
- Task assignment to sessions

### Phase 6: History View & Stats
- History view toggle
- Aggregated project stats (tasks, sessions, time, tokens, cost)
- Per-task completion details

### Phase 7: Command Palette & Polish
- Fuzzy-finder command palette (ctrl+p)
- Per-project JSON export
- Error handling and edge cases
- CLI subcommands for scripting (claustre add-project, claustre add-task, etc.)
