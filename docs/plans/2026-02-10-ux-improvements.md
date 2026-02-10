# UX Improvements Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add 7 UX improvements to the Claustre TUI: error toasts, help screen, dynamic status hints, task edit/delete, task reorder, and task search/filter.

**Architecture:** All changes are in the TUI layer (`src/tui/app.rs`, `src/tui/ui.rs`) plus a DB migration for `sort_order` and store queries for task update/delete/reorder. No changes to MCP, session, or config modules.

**Tech Stack:** Rust, ratatui, crossterm, rusqlite

---

### Task 1: Error Toasts — App State

Add a toast notification system to `App` that shows transient messages in the status bar.

**Files:**
- Modify: `src/tui/app.rs`

**Step 1: Add toast fields to App struct**

In `src/tui/app.rs`, add after the `rate_limit_state` field (line ~128):

```rust
// Toast notification
pub toast_message: Option<String>,
pub toast_style: ToastStyle,
pub toast_expires: Option<std::time::Instant>,
```

Add the enum before `App`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastStyle {
    Info,
    Success,
    Error,
}
```

Initialize in `App::new()`:
```rust
toast_message: None,
toast_style: ToastStyle::Info,
toast_expires: None,
```

**Step 2: Add toast helper methods**

Add to `impl App`:

```rust
pub fn show_toast(&mut self, message: impl Into<String>, style: ToastStyle) {
    self.toast_message = Some(message.into());
    self.toast_style = style;
    self.toast_expires = Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
}

/// Clear expired toast on each tick.
fn tick_toast(&mut self) {
    if let Some(expires) = self.toast_expires {
        if std::time::Instant::now() > expires {
            self.toast_message = None;
            self.toast_expires = None;
        }
    }
}
```

Call `self.tick_toast()` at the start of the `AppEvent::Tick` handler in `run()`.

**Step 3: Wire toasts into existing error paths**

Replace silent failures with toast calls. Examples:

In `handle_normal_key` for session goto (Focus::Sessions Enter):
```rust
Focus::Sessions => {
    if let Some(session) = self.selected_session() {
        if let Err(e) = crate::session::goto_session(session) {
            self.show_toast(format!("Failed to switch tab: {e}"), ToastStyle::Error);
        }
    }
}
```

In `handle_normal_key` for launch task ('l'):
```rust
// After create_session, add error handling:
match crate::session::create_session(...) {
    Ok(()) => self.show_toast("Session launched", ToastStyle::Success),
    Err(e) => self.show_toast(format!("Launch failed: {e}"), ToastStyle::Error),
}
```

In `handle_normal_key` for teardown session ('d'):
```rust
match crate::session::teardown_session(&self.store, &session_id) {
    Ok(()) => self.show_toast("Session closed", ToastStyle::Success),
    Err(e) => self.show_toast(format!("Teardown failed: {e}"), ToastStyle::Error),
}
```

**Step 4: Run tests and verify it compiles**

Run: `cargo build 2>&1 | head -30`
Expected: compiles successfully

**Step 5: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): add toast notification system"
```

---

### Task 2: Error Toasts — Rendering

Render the toast in the status bar, overriding the default hints when a toast is active.

**Files:**
- Modify: `src/tui/ui.rs`

**Step 1: Add toast rendering to draw_active**

In `draw_active()`, replace the status bar rendering section (the `let status = if ...` block, around line 150-181). The toast takes priority over everything:

```rust
let status = if let Some(ref msg) = app.toast_message {
    let color = match app.toast_style {
        super::app::ToastStyle::Info => Color::Cyan,
        super::app::ToastStyle::Success => Color::Green,
        super::app::ToastStyle::Error => Color::Red,
    };
    Line::from(Span::styled(
        format!(" {msg} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ))
} else if app.input_mode == InputMode::ConfirmDelete {
    // existing confirm delete rendering...
} else {
    // existing needs_attention / default hints...
};
```

Also add toast rendering to `draw_history` and `draw_skills` status bars (same pattern — check toast first, then existing logic).

**Step 2: Run tests and verify it compiles**

Run: `cargo build 2>&1 | head -30`
Expected: compiles successfully

**Step 3: Commit**

```bash
git add src/tui/ui.rs
git commit -m "feat(tui): render toast notifications in status bar"
```

---

### Task 3: Help Screen — Overlay

Add a `?` keybinding that shows a floating help overlay with all keybindings for the current view.

**Files:**
- Modify: `src/tui/app.rs`
- Modify: `src/tui/ui.rs`

**Step 1: Add HelpOverlay input mode**

In `InputMode` enum in `app.rs`, add `HelpOverlay` variant.

In `handle_normal_key`, add:
```rust
(KeyCode::Char('?'), _) => {
    self.input_mode = InputMode::HelpOverlay;
}
```

In `run()`, add `InputMode::HelpOverlay` to the key match:
```rust
InputMode::HelpOverlay => {
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
        self.input_mode = InputMode::Normal;
    }
}
```

Also add `?` handler in `handle_skills_key` (same pattern).

**Step 2: Render help overlay**

In `ui.rs`, add to the `draw()` function's overlay match:
```rust
InputMode::HelpOverlay => draw_help_overlay(frame, app),
```

Implement `draw_help_overlay`:
```rust
fn draw_help_overlay(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = 22u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Help — press ? or Esc to close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    let lines = match app.view {
        View::Active => vec![
            help_line("Global", ""),
            help_line("  Tab", "Cycle views"),
            help_line("  Ctrl+P", "Command palette"),
            help_line("  1/2/3", "Focus projects/sessions/tasks"),
            help_line("  j/k", "Navigate up/down"),
            help_line("  q", "Quit"),
            help_line("", ""),
            help_line("Projects", ""),
            help_line("  a", "Add project"),
            help_line("  x", "Remove project"),
            help_line("", ""),
            help_line("Sessions", ""),
            help_line("  s", "New session"),
            help_line("  Enter", "Go to Zellij tab"),
            help_line("  d", "Teardown session"),
            help_line("", ""),
            help_line("Tasks", ""),
            help_line("  n", "New task"),
            help_line("  e", "Edit task"),
            help_line("  l", "Launch task"),
            help_line("  r", "Review (mark done)"),
            help_line("  x", "Delete task"),
            help_line("  /", "Search/filter tasks"),
            help_line("  Shift+J/K", "Reorder tasks"),
        ],
        View::History => vec![
            help_line("  j/k", "Navigate projects"),
            help_line("  Tab", "Cycle views"),
            help_line("  q", "Quit"),
        ],
        View::Skills => vec![
            help_line("  j/k", "Navigate skills"),
            help_line("  f", "Find skills"),
            help_line("  a", "Add skill"),
            help_line("  x", "Remove skill"),
            help_line("  u", "Update skills"),
            help_line("  g", "Toggle scope"),
            help_line("  Tab", "Cycle views"),
            help_line("  q", "Quit"),
        ],
    };

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    if desc.is_empty() {
        Line::from(Span::styled(
            format!("  {key}"),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(vec![
            Span::styled(format!("  {key:<14}"), Style::default().fg(Color::Cyan)),
            Span::styled(desc, Style::default().fg(Color::White)),
        ])
    }
}
```

**Step 3: Run tests and verify it compiles**

Run: `cargo build 2>&1 | head -30`
Expected: compiles successfully

**Step 4: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat(tui): add help screen overlay with ? keybinding"
```

---

### Task 4: Dynamic Status Bar Hints

Make the status bar show context-specific keybindings based on current focus.

**Files:**
- Modify: `src/tui/ui.rs`

**Step 1: Replace static hints with dynamic hints**

In `draw_active()`, change the default status bar (the else branch that currently shows a static string) to be focus-dependent:

```rust
} else {
    let hints = match app.focus {
        Focus::Projects => " a:add  x:remove  n:task  s:session  j/k:nav  ?:help",
        Focus::Sessions => " Enter:goto  d:teardown  s:new  j/k:nav  ?:help",
        Focus::Tasks => " n:new  e:edit  l:launch  r:review  x:delete  /:search  J/K:reorder  ?:help",
    };
    Line::from(Span::styled(hints, Style::default().fg(Color::DarkGray)))
};
```

**Step 2: Run tests and verify it compiles**

Run: `cargo build 2>&1 | head -30`
Expected: compiles successfully

**Step 3: Commit**

```bash
git add src/tui/ui.rs
git commit -m "feat(tui): dynamic status bar hints based on focus"
```

---

### Task 5: Task Edit and Delete — Store Layer

Add `update_task` and `delete_task` queries.

**Files:**
- Modify: `src/store/queries.rs`

**Step 1: Add update_task query**

```rust
pub fn update_task(
    &self,
    id: &str,
    title: &str,
    description: &str,
    mode: TaskMode,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    self.conn.execute(
        "UPDATE tasks SET title = ?1, description = ?2, mode = ?3, updated_at = ?4 WHERE id = ?5",
        params![title, description, mode.as_str(), now, id],
    )?;
    Ok(())
}
```

**Step 2: Add delete_task query**

```rust
pub fn delete_task(&self, id: &str) -> Result<()> {
    self.conn
        .execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
    Ok(())
}
```

**Step 3: Add tests**

```rust
#[test]
fn test_update_task() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();
    let task = store
        .create_task(&project.id, "old title", "old desc", TaskMode::Supervised)
        .unwrap();

    store
        .update_task(&task.id, "new title", "new desc", TaskMode::Autonomous)
        .unwrap();

    let t = store.get_task(&task.id).unwrap();
    assert_eq!(t.title, "new title");
    assert_eq!(t.description, "new desc");
    assert_eq!(t.mode, TaskMode::Autonomous);
}

#[test]
fn test_delete_task() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();
    let task = store
        .create_task(&project.id, "doomed", "", TaskMode::Supervised)
        .unwrap();

    store.delete_task(&task.id).unwrap();
    let tasks = store.list_tasks_for_project(&project.id).unwrap();
    assert!(tasks.is_empty());
}
```

**Step 4: Run tests**

Run: `cargo test -- test_update_task test_delete_task`
Expected: both pass

**Step 5: Commit**

```bash
git add src/store/queries.rs
git commit -m "feat(store): add update_task and delete_task queries"
```

---

### Task 6: Task Edit and Delete — TUI Handlers

Wire `e` (edit) and `x` (delete) keybindings for tasks in the Active view.

**Files:**
- Modify: `src/tui/app.rs`

**Step 1: Add EditTask input mode**

Add `EditTask` to `InputMode` enum.

Add fields to `App`:
```rust
// Editing task state
pub editing_task_id: Option<String>,
```

Initialize in `App::new()`:
```rust
editing_task_id: None,
```

**Step 2: Add 'e' handler to handle_normal_key**

```rust
// Edit task
(KeyCode::Char('e'), _) => {
    if self.focus == Focus::Tasks {
        if let Some(task) = self.visible_tasks().get(self.task_index).copied() {
            if task.status == crate::store::TaskStatus::Pending {
                self.editing_task_id = Some(task.id.clone());
                self.new_task_title.clone_from(&task.title);
                self.new_task_description.clone_from(&task.description);
                self.new_task_mode = task.mode;
                self.new_task_field = 0;
                self.input_buffer.clone_from(&task.title);
                self.input_mode = InputMode::EditTask;
            }
        }
    }
}
```

**Step 3: Add 'x' handler for task delete (when focused on Tasks)**

Modify the existing 'x' handler to also handle task deletion when focus is Tasks:

```rust
(KeyCode::Char('x'), _) => {
    match self.focus {
        Focus::Projects => {
            // existing project delete logic...
        }
        Focus::Tasks => {
            if let Some(task) = self.visible_tasks().get(self.task_index).copied() {
                if task.status == crate::store::TaskStatus::Pending {
                    self.confirm_target = task.title.clone();
                    self.confirm_project_id = task.id.clone(); // reuse field for task id
                    self.input_mode = InputMode::ConfirmDelete;
                }
            }
        }
        _ => {}
    }
}
```

Update `handle_confirm_delete_key` to detect whether we're deleting a project or task based on focus. Better approach: add a `confirm_delete_kind` field:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTarget {
    Project,
    Task,
}
```

Add to App:
```rust
pub confirm_delete_kind: DeleteTarget,
```

Set `confirm_delete_kind = DeleteTarget::Project` in the project delete path and `DeleteTarget::Task` in the task delete path.

In `handle_confirm_delete_key`:
```rust
KeyCode::Char('y') => {
    match self.confirm_delete_kind {
        DeleteTarget::Project => {
            if !self.confirm_project_id.is_empty() {
                self.store.delete_project(&self.confirm_project_id)?;
                self.project_index = 0;
            }
        }
        DeleteTarget::Task => {
            if !self.confirm_project_id.is_empty() {
                self.store.delete_task(&self.confirm_project_id)?;
            }
        }
    }
    self.confirm_project_id.clear();
    self.confirm_target.clear();
    self.input_mode = InputMode::Normal;
    self.refresh_data()?;
}
```

**Step 4: Add EditTask key handler**

Add to `run()` match:
```rust
InputMode::EditTask => self.handle_edit_task_key(key.code)?,
```

Implement `handle_edit_task_key` — identical to `handle_input_key` except on Enter it calls `update_task` instead of `create_task`:

```rust
fn handle_edit_task_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => {
            self.save_current_task_field();
            if !self.new_task_title.is_empty() {
                if let Some(ref task_id) = self.editing_task_id {
                    self.store.update_task(
                        task_id,
                        &self.new_task_title,
                        &self.new_task_description,
                        self.new_task_mode,
                    )?;
                }
                self.editing_task_id = None;
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
        }
        KeyCode::Esc => {
            self.editing_task_id = None;
            self.reset_task_form();
            self.input_mode = InputMode::Normal;
        }
        // All other keys: delegate to same field logic as new task
        other => self.handle_input_key(other)?,
    }
    Ok(())
}
```

Wait — this won't work since `handle_input_key` has its own Enter/Esc handlers. Instead, extract the shared field editing into a helper and call it from both. Better: just duplicate the field editing keys:

```rust
fn handle_edit_task_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => {
            self.save_current_task_field();
            if !self.new_task_title.is_empty() {
                if let Some(ref task_id) = self.editing_task_id.clone() {
                    self.store.update_task(
                        task_id,
                        &self.new_task_title,
                        &self.new_task_description,
                        self.new_task_mode,
                    )?;
                    self.show_toast("Task updated", ToastStyle::Success);
                }
                self.editing_task_id = None;
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
        }
        KeyCode::Esc => {
            self.editing_task_id = None;
            self.reset_task_form();
            self.input_mode = InputMode::Normal;
        }
        KeyCode::Tab => {
            self.save_current_task_field();
            self.new_task_field = (self.new_task_field + 1) % 3;
            self.load_current_task_field();
        }
        KeyCode::BackTab => {
            self.save_current_task_field();
            self.new_task_field = if self.new_task_field == 0 { 2 } else { self.new_task_field - 1 };
            self.load_current_task_field();
        }
        KeyCode::Left | KeyCode::Right if self.new_task_field == 2 => {
            self.new_task_mode = match self.new_task_mode {
                crate::store::TaskMode::Supervised => crate::store::TaskMode::Autonomous,
                crate::store::TaskMode::Autonomous => crate::store::TaskMode::Supervised,
            };
        }
        KeyCode::Char(c) if self.new_task_field < 2 => {
            self.input_buffer.push(c);
        }
        KeyCode::Backspace if self.new_task_field < 2 => {
            self.input_buffer.pop();
        }
        _ => {}
    }
    Ok(())
}
```

**Step 5: Render "Edit Task" panel**

In `ui.rs`, add to the overlay match in `draw()`:
```rust
InputMode::EditTask => draw_edit_task_panel(frame, app),
```

`draw_edit_task_panel` is identical to `draw_new_task_panel` except the title says " Edit Task ". Extract a shared helper:

```rust
fn draw_task_form_panel(frame: &mut Frame, app: &App, title: &str) {
    // ... same as current draw_new_task_panel but with configurable title
}

fn draw_new_task_panel(frame: &mut Frame, app: &App) {
    draw_task_form_panel(frame, app, " New Task ");
}

fn draw_edit_task_panel(frame: &mut Frame, app: &App) {
    draw_task_form_panel(frame, app, " Edit Task ");
}
```

**Step 6: Run tests and verify it compiles**

Run: `cargo build 2>&1 | head -30`
Expected: compiles successfully

**Step 7: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat(tui): add task edit (e) and delete (x) keybindings"
```

---

### Task 7: Task Reorder — Migration & Store

Add a `sort_order` column to tasks and update queries to support reordering.

**Files:**
- Modify: `src/store/mod.rs`
- Modify: `src/store/models.rs`
- Modify: `src/store/queries.rs`

**Step 1: Add migration v3**

In `src/store/mod.rs`, append to `MIGRATIONS`:

```rust
Migration {
    version: 3,
    sql: "
        ALTER TABLE tasks ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
        UPDATE tasks SET sort_order = CAST((julianday(created_at) - 2460000) * 86400 AS INTEGER);
    ",
},
```

**Step 2: Add sort_order to Task model**

In `src/store/models.rs`, add to `Task`:
```rust
pub sort_order: i64,
```

**Step 3: Update all task queries to include sort_order**

In `queries.rs`:
- `get_task`: add `sort_order` to SELECT (column index 14), add to `Task` construction
- `list_tasks_for_project`: same + change `ORDER BY created_at` to `ORDER BY sort_order, created_at`
- `create_task`: set sort_order to current max+1 for the project
- `next_pending_task_for_session`: add `sort_order` to SELECT, change ORDER to `sort_order, created_at`

For `create_task`, before the INSERT:
```rust
let max_order: i64 = self.conn.query_row(
    "SELECT COALESCE(MAX(sort_order), 0) FROM tasks WHERE project_id = ?1",
    params![project_id],
    |row| row.get(0),
)?;
```
Then use `max_order + 1` in the INSERT.

**Step 4: Add swap_task_order query**

```rust
pub fn swap_task_order(&self, task_a_id: &str, task_b_id: &str) -> Result<()> {
    let order_a: i64 = self.conn.query_row(
        "SELECT sort_order FROM tasks WHERE id = ?1",
        params![task_a_id],
        |row| row.get(0),
    )?;
    let order_b: i64 = self.conn.query_row(
        "SELECT sort_order FROM tasks WHERE id = ?1",
        params![task_b_id],
        |row| row.get(0),
    )?;
    self.conn.execute(
        "UPDATE tasks SET sort_order = ?1 WHERE id = ?2",
        params![order_b, task_a_id],
    )?;
    self.conn.execute(
        "UPDATE tasks SET sort_order = ?1 WHERE id = ?2",
        params![order_a, task_b_id],
    )?;
    Ok(())
}
```

**Step 5: Add tests**

```rust
#[test]
fn test_task_sort_order() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();

    let t1 = store.create_task(&project.id, "first", "", TaskMode::Supervised).unwrap();
    let t2 = store.create_task(&project.id, "second", "", TaskMode::Supervised).unwrap();
    let t3 = store.create_task(&project.id, "third", "", TaskMode::Supervised).unwrap();

    // Default order
    let tasks = store.list_tasks_for_project(&project.id).unwrap();
    assert_eq!(tasks[0].title, "first");
    assert_eq!(tasks[1].title, "second");
    assert_eq!(tasks[2].title, "third");

    // Swap first and third
    store.swap_task_order(&t1.id, &t3.id).unwrap();
    let tasks = store.list_tasks_for_project(&project.id).unwrap();
    assert_eq!(tasks[0].title, "third");
    assert_eq!(tasks[2].title, "first");
}
```

**Step 6: Run tests**

Run: `cargo test`
Expected: all pass

**Step 7: Commit**

```bash
git add src/store/mod.rs src/store/models.rs src/store/queries.rs
git commit -m "feat(store): add sort_order column and task reordering"
```

---

### Task 8: Task Reorder — TUI Handlers

Wire `Shift+J` and `Shift+K` to reorder tasks.

**Files:**
- Modify: `src/tui/app.rs`

**Step 1: Add keybindings**

In `handle_normal_key`, add:

```rust
(KeyCode::Char('J'), _) => {
    // Shift+J: move task down in sort order
    if self.focus == Focus::Tasks {
        let visible = self.visible_tasks();
        if self.task_index + 1 < visible.len() {
            let current_id = visible[self.task_index].id.clone();
            let next_id = visible[self.task_index + 1].id.clone();
            if self.store.swap_task_order(&current_id, &next_id).is_ok() {
                self.task_index += 1;
                let _ = self.refresh_data();
            }
        }
    }
}

(KeyCode::Char('K'), _) => {
    // Shift+K: move task up in sort order
    if self.focus == Focus::Tasks && self.task_index > 0 {
        let visible = self.visible_tasks();
        let current_id = visible[self.task_index].id.clone();
        let prev_id = visible[self.task_index - 1].id.clone();
        if self.store.swap_task_order(&current_id, &prev_id).is_ok() {
            self.task_index -= 1;
            let _ = self.refresh_data();
        }
    }
}
```

Note: `KeyCode::Char('J')` (uppercase) is how crossterm represents Shift+J.

**Step 2: Run tests and verify it compiles**

Run: `cargo build 2>&1 | head -30`
Expected: compiles successfully

**Step 3: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): add Shift+J/K task reordering"
```

---

### Task 9: Task Search/Filter

Add `/` keybinding to filter tasks by title.

**Files:**
- Modify: `src/tui/app.rs`
- Modify: `src/tui/ui.rs`

**Step 1: Add TaskFilter input mode and state**

In `InputMode`, add `TaskFilter`.

Add to `App`:
```rust
pub task_filter: String,
```

Initialize in `App::new()`:
```rust
task_filter: String::new(),
```

**Step 2: Update visible_tasks to respect filter**

```rust
pub fn visible_tasks(&self) -> Vec<&Task> {
    let base: Box<dyn Iterator<Item = &Task>> = match self.view {
        View::Active => Box::new(self.tasks.iter().filter(|t| t.status != crate::store::TaskStatus::Done)),
        View::History | View::Skills => Box::new(self.tasks.iter()),
    };
    if self.task_filter.is_empty() {
        base.collect()
    } else {
        let filter_lower = self.task_filter.to_lowercase();
        base.filter(|t| t.title.to_lowercase().contains(&filter_lower)).collect()
    }
}
```

**Step 3: Add '/' handler to handle_normal_key**

```rust
(KeyCode::Char('/'), _) => {
    if self.focus == Focus::Tasks || self.view == View::Active {
        self.task_filter.clear();
        self.input_mode = InputMode::TaskFilter;
        self.focus = Focus::Tasks;
    }
}
```

**Step 4: Add TaskFilter key handler**

Add to `run()` match:
```rust
InputMode::TaskFilter => self.handle_task_filter_key(key.code)?,
```

```rust
fn handle_task_filter_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter | KeyCode::Esc => {
            if code == KeyCode::Esc {
                self.task_filter.clear();
            }
            self.input_mode = InputMode::Normal;
            self.task_index = 0;
        }
        KeyCode::Char(c) => {
            self.task_filter.push(c);
            self.task_index = 0;
        }
        KeyCode::Backspace => {
            self.task_filter.pop();
            self.task_index = 0;
        }
        _ => {}
    }
    Ok(())
}
```

**Step 5: Render filter in status bar and task queue title**

In `ui.rs`, update `draw_task_queue` to show the filter in the block title when active:
```rust
let title = if !app.task_filter.is_empty() {
    format!(" Task Queue [filter: {}] ", app.task_filter)
} else {
    " Task Queue ".to_string()
};
```

In `draw_active`, when `InputMode::TaskFilter`, show the filter input in the status bar:
```rust
// Add before the existing status bar logic:
} else if app.input_mode == InputMode::TaskFilter {
    Line::from(vec![
        Span::styled(" /", Style::default().fg(Color::Yellow)),
        Span::raw(&app.task_filter),
        Span::styled("█", Style::default().fg(Color::Yellow)),
        Span::styled(
            "  Enter:apply  Esc:clear",
            Style::default().fg(Color::DarkGray),
        ),
    ])
}
```

**Step 6: Run tests and verify it compiles**

Run: `cargo build 2>&1 | head -30`
Expected: compiles successfully

**Step 7: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat(tui): add task search/filter with / keybinding"
```

---

### Task 10: Final Integration — Clippy & Test

Run full quality checks.

**Files:** none (verification only)

**Step 1: Run clippy**

Run: `cargo clippy 2>&1`
Expected: no warnings

**Step 2: Run all tests**

Run: `cargo test 2>&1`
Expected: all pass

**Step 3: Fix any issues found in steps 1-2**

**Step 4: Final commit if any fixes needed**

```bash
git add -A
git commit -m "fix: address clippy and test issues from UX improvements"
```
