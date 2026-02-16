# Unified TUI Layout Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Merge the 3 tab-cycled TUI views (Active, History, Skills) into a single unified view where selecting a project updates all panels.

**Architecture:** Remove the `View` enum. Replace `draw_active`/`draw_history`/`draw_skills` with a single `draw()`. Move skills to a floating panel. Merge completed tasks into the task queue. Add project stats panel to the left column.

**Tech Stack:** Rust, ratatui, crossterm

---

### Task 1: Remove the `View` enum and simplify the event loop

**Files:**
- Modify: `src/tui/app.rs:20-24` (View enum)
- Modify: `src/tui/app.rs:272` (view field initialization)
- Modify: `src/tui/app.rs:727-733` (event dispatch)
- Modify: `src/tui/app.rs:784-793` (Tab key handler)
- Modify: `src/tui/app.rs:1487-1495` (palette ToggleView action)
- Modify: `src/tui/ui.rs:38-43` (draw dispatch)

This task removes the `View` enum and all branching on it. After this task, the app has no concept of multiple views.

**Step 1: Remove the `View` enum from `app.rs`**

Delete the `View` enum (lines 20-24) and the `pub view: View` field from the `App` struct (line 89). Remove `view: View::Active` from `App::new()` (line 272).

**Step 2: Remove Tab key view cycling from `handle_normal_key()`**

In `handle_normal_key()` (line 784-793), remove the `(KeyCode::Tab, _)` arm entirely. Tab is now a no-op in normal mode (or can be repurposed later).

**Step 3: Remove view branching from the event loop**

In `run()` (lines 727-733), change:
```rust
InputMode::Normal => {
    if self.view == View::Skills {
        self.handle_skills_key(key.code, key.modifiers)?;
    } else {
        self.handle_normal_key(key.code, key.modifiers)?;
    }
}
```
to:
```rust
InputMode::Normal => {
    self.handle_normal_key(key.code, key.modifiers)?;
}
```

**Step 4: Remove `PaletteAction::ToggleView` and update palette items**

In `App::new()`, remove the "Toggle View" palette item (line 235-238). Remove the `PaletteAction::ToggleView` variant from the enum (line 71) and its handler in `execute_palette_action()` (lines 1487-1496).

**Step 5: Simplify `draw()` in `ui.rs`**

Change `draw()` to call a single draw function instead of matching on `app.view`:
```rust
pub fn draw(frame: &mut Frame, app: &App) {
    draw_main(frame, app);

    // Floating panel overlays
    match app.input_mode {
        InputMode::CommandPalette => draw_command_palette(frame, app),
        InputMode::NewTask => draw_task_form_panel(frame, app, " New Task "),
        InputMode::EditTask => draw_task_form_panel(frame, app, " Edit Task "),
        InputMode::NewProject => draw_new_project_panel(frame, app),
        InputMode::HelpOverlay => draw_help_overlay(frame, app),
        InputMode::SubtaskPanel => draw_subtask_panel(frame, app),
        _ => {}
    }
}
```

**Step 6: Remove `visible_tasks()` view branching**

In `visible_tasks()` (line 709), remove the `self.view == View::Active &&` check — done tasks will be handled differently in Task 2.

**Step 7: Fix all remaining `View` references**

Remove `View` from the `use` import in `ui.rs` (line 11). Fix the help overlay which branches on `app.view` (line 1589) — use a single help section for now. Remove all `View::*` references throughout `app.rs`.

**Step 8: Run tests, fix compilation errors**

Run: `cargo build 2>&1 | head -50`

Many tests reference `View` — they'll fail to compile. That's expected. Fix compilation by:
- Delete `view_cycling_with_tab` test
- Delete `snapshot_history_view` test
- Delete `snapshot_skills_view` test
- Delete `skills_view_tab_returns_to_active` test
- Delete `skills_view_ctrl_p_opens_palette` test
- Delete `skills_view_help` test
- Remove any `app.view = View::*` assignments in remaining tests
- Remove `View` from test imports

**Step 9: Run tests to verify**

Run: `cargo test 2>&1 | tail -20`
Expected: all tests pass (some snapshot tests may need content updates — fix those in later tasks)

**Step 10: Run clippy**

Run: `cargo clippy 2>&1 | tail -20`
Expected: clean (fix any warnings)

**Step 11: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "refactor: remove View enum, single unified TUI layout"
```

---

### Task 2: Merge completed tasks into the task queue

**Files:**
- Modify: `src/tui/app.rs` — `visible_tasks()` method
- Modify: `src/tui/ui.rs` — `draw_task_queue()` function

This task makes `visible_tasks()` return all tasks (not just active ones) and renders done tasks with dimmed styling.

**Step 1: Update `visible_tasks()` to include done tasks**

The filter currently skips Done tasks. Remove that filter entirely. The method becomes:
```rust
pub fn visible_tasks(&self) -> Vec<&Task> {
    let filter_lower = self.task_filter.to_lowercase();
    self.tasks
        .iter()
        .filter(|t| {
            if !filter_lower.is_empty() && !t.title.to_lowercase().contains(&filter_lower) {
                return false;
            }
            true
        })
        .collect()
}
```

The SQL query already sorts by `sort_order, created_at`. Done tasks naturally sort after active ones because they were created earlier and their sort_order reflects their original position. This is acceptable — the ordering is chronological which makes sense for a unified list.

**Step 2: Dim done tasks in `draw_task_queue()`**

In `draw_task_queue()` (ui.rs line 531-577), update the task item rendering. When `task.status == TaskStatus::Done`, dim the entire row:
```rust
let is_done = task.status == TaskStatus::Done;
// ... existing spans logic ...
// After building spans, if done, override the title style:
if is_done {
    spans.push(Span::styled(&task.title, Style::default().fg(Color::DarkGray)));
} else {
    spans.push(Span::styled(&task.title, Style::default().fg(Color::White)));
}
```

Also dim the status text for done tasks. The status symbol already uses `Color::Blue` for Done which is fine — but the title and extra text should be `Color::DarkGray`.

**Step 3: Guard actionable keys against done tasks**

In `handle_normal_key()`, the `l` (launch), `e` (edit) keys already check for `TaskStatus::Pending`. The `r` (review) key checks for `InReview`/`InProgress`. So done tasks already can't be launched/edited/reviewed. No changes needed here.

Verify: `o` (open PR) should still work on done tasks if they have a PR URL — this is correct and desirable.

**Step 4: Run tests**

Run: `cargo test 2>&1 | tail -20`
Expected: pass. The `navigate_tasks_jk` test creates 3 tasks (all pending) so the count doesn't change. Snapshot tests may show Done tasks now — update assertions if needed.

**Step 5: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat: show completed tasks in unified task queue with dimmed style"
```

---

### Task 3: Add project stats panel to left column

**Files:**
- Modify: `src/tui/ui.rs` — new `draw_main()` function, repurpose `draw_project_stats()`
- Modify: `src/tui/app.rs` — add `project_stats` field to `App`

This task creates the new unified layout with the project stats panel below the projects list.

**Step 1: Add cached project stats to `App`**

In the `App` struct, add:
```rust
pub project_stats: Option<crate::store::ProjectStats>,
```

Initialize it in `App::new()`:
```rust
let project_stats = projects.first().map(|p| store.project_stats(&p.id).ok()).flatten();
```

In `refresh_data()`, update it:
```rust
self.project_stats = self.selected_project()
    .map(|p| self.store.project_stats(&p.id).ok())
    .flatten();
```

This avoids calling `store.project_stats()` during every render (it was doing this in the old `draw_project_stats` via `app.store.project_stats()`).

**Step 2: Write the new `draw_main()` function**

Replace `draw_active` with `draw_main`. The layout structure:

```rust
fn draw_main(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Bottom status line height (same logic as old draw_active)
    let needs_attention = app.tasks.iter()
        .filter(|t| t.status == TaskStatus::InReview).count();
    let has_status_line = app.toast_message.is_some()
        || (needs_attention > 0
            && app.input_mode != InputMode::ConfirmDelete
            && app.input_mode != InputMode::TaskFilter);
    let bottom_height: u16 = if has_status_line { 2 } else { 1 };

    // Outer: title bar | main area | bottom
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),   // title bar
            Constraint::Min(0),      // main area
            Constraint::Length(bottom_height), // hints/status
        ])
        .split(size);

    // Title bar (updated — no more "Tab:cycle")
    let title = Line::from(vec![
        Span::styled(" claustre ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::raw("                                        "),
        Span::styled("a:project  n:task  l:launch  i:skills  q:quit",
            Style::default().fg(Color::DarkGray)),
    ]);
    frame.render_widget(Paragraph::new(title), outer[0]);

    // Main area: left column (30%) | right column (70%)
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(outer[1]);

    // Left column: projects (top) | stats (bottom)
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(main[0]);

    draw_projects(frame, app, left[0]);
    draw_project_stats(frame, app, left[1]);

    // Right column: tasks (top, flexible) | session detail (mid) | usage (bottom)
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Percentage(35),
            Constraint::Length(if app.rate_limit_state.is_rate_limited { 6 } else { 4 }),
        ])
        .split(main[1]);

    draw_task_queue(frame, app, right[0]);
    draw_session_detail(frame, app, right[1]);
    draw_usage_bars(frame, app, right[2]);

    // Bottom bar (reuse same logic from old draw_active)
    // ... existing bottom bar rendering (confirm delete, task filter, status+hints)
}
```

**Step 3: Update `draw_project_stats()` to use cached stats**

Change `draw_project_stats()` to read from `app.project_stats` instead of calling `app.store.project_stats()`:
```rust
fn draw_project_stats(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Stats ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    if let Some(ref stats) = app.project_stats {
        // ... same rendering as before, using stats directly ...
    } else {
        let msg = Paragraph::new("  No project selected")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
    }
}
```

**Step 4: Delete dead draw functions**

Remove: `draw_active`, `draw_history`, `draw_history_projects`, `draw_completed_tasks`, `draw_skills`, `draw_installed_skills`, `draw_skill_search`, `draw_skill_detail`.

Keep: `draw_projects`, `draw_task_queue`, `draw_session_detail`, `draw_usage_bars`, `draw_project_stats`, `draw_command_palette`, `draw_task_form_panel`, `draw_new_project_panel`, `draw_help_overlay`, `draw_subtask_panel`.

**Step 5: Update the bottom bar / hints**

The hints line should reflect the unified view. Update the hints for both focus states:
```rust
Focus::Projects => " a:add  d:delete  n:task  i:skills  j/k:nav  ?:help",
Focus::Tasks => " n:new  e:edit  s:subtasks  l:launch  r:review  o:PR  d:del  /:filter  J/K:reorder  ?:help",
```

**Step 6: Update help overlay**

Replace the view-branching help content with a single unified help section:
```rust
let lines: Vec<Line<'_>> = vec![
    help_section("Navigation"),
    help_line("  1/2", "Focus projects/tasks"),
    help_line("  j/k", "Navigate up/down"),
    help_line("  Ctrl+P", "Command palette"),
    help_line("  q", "Quit"),
    Line::from(""),
    help_section("Projects"),
    help_line("  a", "Add project"),
    help_line("  d", "Delete project"),
    Line::from(""),
    help_section("Tasks"),
    help_line("  n", "New task"),
    help_line("  e", "Edit task (pending only)"),
    help_line("  s", "Subtasks panel"),
    help_line("  l", "Launch task"),
    help_line("  r", "Review (mark done)"),
    help_line("  o", "Open PR in browser"),
    help_line("  d", "Delete task"),
    help_line("  /", "Search/filter tasks"),
    help_line("  Shift+J/K", "Reorder tasks"),
    Line::from(""),
    help_section("Skills"),
    help_line("  i", "Open skills panel"),
];
```

**Step 7: Run tests and fix snapshot assertions**

Run: `cargo test 2>&1 | tail -30`

Update snapshot test assertions:
- `snapshot_active_view_empty`: should now contain "Stats" panel
- `snapshot_active_view_with_data`: should contain "Stats" panel
- `snapshot_active_view_session_detail`: should contain "Stats" panel
- `snapshot_help_overlay`: update assertions (no more "Tab" in help, add "skills")

**Step 8: Run clippy**

Run: `cargo clippy 2>&1 | tail -20`

**Step 9: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat: unified TUI layout with project stats and single view"
```

---

### Task 4: Add skills floating panel

**Files:**
- Modify: `src/tui/app.rs` — add `InputMode::SkillPanel`, add `i` key handler, move skill key handling
- Modify: `src/tui/ui.rs` — add `draw_skill_panel()` floating overlay

This task adds the skills floating panel accessible via `i` key.

**Step 1: Add `InputMode::SkillPanel` variant**

In the `InputMode` enum, add `SkillPanel`. This represents the state where the skills floating panel is open.

**Step 2: Add `i` key handler in `handle_normal_key()`**

```rust
(KeyCode::Char('i'), _) => {
    self.refresh_skills();
    self.skill_index = 0;
    self.input_mode = InputMode::SkillPanel;
}
```

**Step 3: Add skill panel key handler**

Move the logic from `handle_skills_key()` into a new `handle_skill_panel_key()` method. The key difference: `Esc` closes the panel (sets `input_mode = Normal`), and there's no Tab/quit handling (those are for the top-level view).

```rust
fn handle_skill_panel_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Esc => {
            self.input_mode = InputMode::Normal;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if !self.installed_skills.is_empty() {
                self.skill_index = (self.skill_index + 1).min(self.installed_skills.len() - 1);
                self.refresh_skill_detail();
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            self.skill_index = self.skill_index.saturating_sub(1);
            self.refresh_skill_detail();
        }
        KeyCode::Char('f') => {
            self.input_mode = InputMode::SkillSearch;
            self.input_buffer.clear();
            self.search_results.clear();
            self.skill_index = 0;
        }
        KeyCode::Char('a') => {
            self.input_mode = InputMode::SkillAdd;
            self.input_buffer.clear();
        }
        KeyCode::Char('x') => {
            // same remove logic as handle_skills_key
        }
        KeyCode::Char('u') => {
            // same update logic
        }
        KeyCode::Char('g') => {
            self.skill_scope_global = !self.skill_scope_global;
            self.refresh_skills();
        }
        _ => {}
    }
    Ok(())
}
```

**Step 4: Wire up in the event loop**

In `run()`, add the new input mode to the match:
```rust
InputMode::SkillPanel => self.handle_skill_panel_key(key.code)?,
```

Update `SkillSearch` and `SkillAdd` handlers so that `Esc` returns to `InputMode::SkillPanel` instead of `InputMode::Normal`.

**Step 5: Write `draw_skill_panel()` floating overlay**

Create a centered overlay similar to `draw_subtask_panel()`. It shows installed skills on the left half and skill detail on the right half:

```rust
fn draw_skill_panel(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 80u16.min(area.width.saturating_sub(4));
    let height = 20u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Skills ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    // Split inner into left (skill list) and right (detail)
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(inner);

    // Render skill list in left half (reuse installed_skills rendering logic)
    // Render skill detail in right half (reuse skill_detail_content rendering logic)
    // Render hints at bottom: "f:find  a:add  x:remove  u:update  g:scope  Esc:close"
}
```

**Step 6: Register the floating panel in `draw()`**

Add to the overlay match in `draw()`:
```rust
InputMode::SkillPanel => draw_skill_panel(frame, app),
```

Also ensure `SkillSearch` and `SkillAdd` draw the skill panel underneath (so the search/add input appears within the panel context).

**Step 7: Remove `handle_skills_key()` method**

Delete the now-unused `handle_skills_key()` method from `app.rs`.

**Step 8: Update palette actions**

Change `PaletteAction::FindSkills` to open the skill panel with search mode:
```rust
PaletteAction::FindSkills => {
    self.refresh_skills();
    self.input_mode = InputMode::SkillSearch;
    self.input_buffer.clear();
    self.search_results.clear();
}
```

Change `PaletteAction::UpdateSkills` to work without switching views.

**Step 9: Run tests**

Run: `cargo test 2>&1 | tail -20`

Add a test:
```rust
#[test]
fn skill_panel_opens_with_i() {
    let mut app = test_app();
    press(&mut app, KeyCode::Char('i'));
    assert_eq!(app.input_mode, InputMode::SkillPanel);
}

#[test]
fn skill_panel_closes_with_esc() {
    let mut app = test_app();
    press(&mut app, KeyCode::Char('i'));
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.input_mode, InputMode::Normal);
}
```

**Step 10: Run clippy**

Run: `cargo clippy 2>&1 | tail -20`

**Step 11: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat: skills floating panel accessible via 'i' key"
```

---

### Task 5: Final cleanup and test updates

**Files:**
- Modify: `src/tui/app.rs` — clean up dead code, update remaining tests
- Modify: `src/tui/ui.rs` — remove unused functions

**Step 1: Remove any remaining dead code**

Search for any remaining references to `View`, `draw_history`, `draw_skills`, `handle_skills_key` and remove them. Run `cargo build` to verify.

**Step 2: Update all snapshot tests**

Run each snapshot test individually and verify the output makes sense:
```bash
cargo test snapshot_ -- --nocapture 2>&1
```

Update assertions to match the new layout:
- All snapshots should show "Stats" panel
- No snapshot should reference "history" or "Installed Skills" as top-level views
- The help overlay should show "i" for skills

**Step 3: Run full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: all tests pass

**Step 4: Run clippy**

Run: `cargo clippy 2>&1 | tail -20`
Expected: clean

**Step 5: Run fmt check**

Run: `cargo fmt --check`
Expected: clean

**Step 6: Commit**

```bash
git add -A
git commit -m "chore: cleanup dead code and update tests for unified TUI"
```
