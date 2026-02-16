# Unified TUI Layout Design

## Problem

The current TUI has 3 tab-cycled views (Active, History, Skills). Cycling with Tab is confusing — changing the selected project doesn't update stats, completed tasks, or skills unless you're on the right tab. Users want all project-related info visible at once.

## Design

### Layout

```
┌───────────┬──────────────────────────────────────────────┐
│           │ Task Queue                                   │
│ Projects  │ (active → done, dimmed)                      │
│  [1]      │                                    [2]       │
├───────────┼──────────────────────────────────────────────┤
│ Project   │ Session Detail                               │
│ Stats     │                                              │
│           ├──────────────────────────────────────────────┤
│           │ Usage bars                                   │
├───────────┴──────────────────────────────────────────────┤
│ hints / status bar                                       │
└──────────────────────────────────────────────────────────┘
```

- Left column: ~30% width. Projects (top ~60%), Stats (bottom ~40%).
- Right column: ~70% width. Tasks (top, flexible), Session detail (mid ~35%), Usage (bottom, 4-6 lines).
- `1`/`2` keys switch focus between Projects and Tasks (unchanged).

### Changes

| What | Change |
|------|--------|
| `View` enum | Remove entirely (Active/History/Skills gone) |
| `draw()` | Single function, new layout |
| `draw_active` | Replaced by new unified `draw()` |
| `draw_history`, `draw_history_projects`, `draw_project_stats`, `draw_completed_tasks` | Removed. Stats inlined into left column. Completed tasks merged into task list. |
| `draw_skills`, `draw_installed_skills`, `draw_skill_search`, `draw_skill_detail` | Moved into a floating panel (`InputMode::SkillPanel`) |
| Tab key | No longer cycles views — freed up |
| `i` key | Opens skills floating panel |
| `visible_tasks()` | Returns all tasks (active first, done at bottom, dimmed) |
| `handle_skills_key()` | Removed. Skills keys handled inside floating panel input mode |
| `handle_normal_key()` | Simplified — no view-dependent branching |
| Snapshot tests | Updated for the new single layout |

### Task list behavior

- `visible_tasks()` returns all tasks sorted by status: pending/in-progress/in-review first (by `sort_order`), then done tasks at the end.
- Done tasks rendered with dimmed style.
- Navigation (`j`/`k`) moves through the full list.
- Done tasks are not actionable (no `l` launch, no `e` edit) — keys silently no-op.

### Skills floating panel

- Triggered by `i` key → `InputMode::SkillPanel`.
- Reuses existing skill rendering in a centered overlay (like the subtask panel).
- Keys inside panel: `f` find, `a` add, `x` remove, `u` update, `g` scope toggle, `j`/`k` navigate, `Esc` close.
- `skill_scope_global` still tied to the selected project context.

### Focus model

Unchanged: `Focus::Projects` and `Focus::Tasks`. Stats and session detail are informational-only (no focus needed).
