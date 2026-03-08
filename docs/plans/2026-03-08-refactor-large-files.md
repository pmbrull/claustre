# Refactor Large Files Into Smaller Modules

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Break down the 4 largest source files into well-organized submodules for maintainability.

**Architecture:** Each large file becomes a directory module with focused subfiles. All `impl` blocks use the same struct imported from the parent `mod.rs`. Public API stays identical — this is a pure structural refactor with no behavioral changes.

**Tech Stack:** Rust 2024 edition, ratatui, rusqlite, portable-pty, vt100

---

## Target Files

| File | Lines | Split Into |
|------|-------|------------|
| `src/tui/app.rs` | 6440 | `src/tui/app/` (8+ submodules) |
| `src/tui/ui.rs` | 2643 | `src/tui/ui/` (7 submodules) |
| `src/pty/mod.rs` | 2617 | 4 new files in `src/pty/` |
| `src/store/queries.rs` | 2255 | `src/store/queries/` (7 submodules) |

---

### Task 1: Split `store/queries.rs` by entity

Create `src/store/queries/` directory module. Split methods by entity:
- `mod.rs` — helper functions (`optional()`, `in_transaction()`), re-exports
- `projects.rs` — Project CRUD (4 methods)
- `tasks.rs` — Task CRUD + queries (24 methods)
- `sessions.rs` — Session CRUD (9 methods)
- `rate_limits.rs` — RateLimit methods (4 methods)
- `subtasks.rs` — Subtask CRUD (8 methods)
- `stats.rs` — Stats queries + `ProjectStats` impl
- `external_sessions.rs` — ExternalSession CRUD (5 methods)

Tests stay in `mod.rs` as `#[cfg(test)] mod tests`.

### Task 2: Split `pty/mod.rs` by struct

Extract into separate files within `src/pty/`:
- `mod.rs` — constants, `PaneId` type alias, re-exports
- `embedded.rs` — `Backend` enum + `EmbeddedTerminal` struct + impl
- `layout.rs` — `SplitDirection`, `LayoutNode`, layout helper functions
- `session_terminals.rs` — `PaneInfo`, `SessionTerminals` struct + impl
- `selection.rs` — `Selection` struct + impl

Tests stay in `mod.rs`.

### Task 3: Split `tui/ui.rs` by rendering concern

Create `src/tui/ui/` directory module:
- `mod.rs` — `draw()` entry point, helpers (spinner, toast), re-exports
- `tab_bar.rs` — tab layout computation + rendering
- `session.rs` — session terminal pane rendering
- `dashboard.rs` — dashboard layout + 4 panels (projects, stats, tasks, session detail)
- `forms.rs` — task form + project form rendering
- `overlays.rs` — command palette, subtask panel, skill panels, help, task details
- `usage.rs` — usage bars + formatting utilities

### Task 4: Split `tui/app.rs` by concern

Create `src/tui/app/` directory module:
- `mod.rs` — `App` struct definition, type aliases, enums, constants, re-exports
- `initialization.rs` — `App::new()`
- `data_refresh.rs` — `refresh_data()` + project summaries
- `polling.rs` — all background polling (usage, updates, PR, git stats, scanner, title gen)
- `session_lifecycle.rs` — create, launch, teardown, review loop, toast
- `pty_management.rs` — tab management, PTY output, restore, detect paused
- `event_loop.rs` — `App::run()` main loop
- `input.rs` — all input handlers (dashboard, session, forms, modals, encoding)

Tests stay in `mod.rs`.
