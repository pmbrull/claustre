# TUI CRUD Operations Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add project, remove project, and enhanced task creation (description + mode) directly from the TUI, removing the need to drop to CLI for these operations.

**Architecture:** Three new `InputMode` variants handle multi-field input forms. Each form uses a field-index pattern to Tab between fields, reusing the existing `input_buffer` for the active field and storing committed fields in dedicated `App` state. A confirmation dialog reuses the popup pattern from `draw_command_palette`.

**Tech Stack:** ratatui, crossterm, existing `Store` CRUD methods (no new DB queries needed)

---

### Task 1: Add `NewProject` input mode and state fields

**Files:**
- Modify: `src/tui/app.rs:29-36` (InputMode enum)
- Modify: `src/tui/app.rs:64-99` (App struct)
- Modify: `src/tui/app.rs:155-178` (App::new)

**Step 1: Add InputMode variant and App fields**

In `src/tui/app.rs`, add `NewProject` to the `InputMode` enum:

```rust
pub enum InputMode {
    Normal,
    NewTask,
    NewSession,
    NewProject,       // <-- new
    ConfirmDelete,    // <-- new (for Task 3)
    CommandPalette,
    SkillSearch,
    SkillAdd,
}
```

Add fields to `App` struct for the multi-field project form:

```rust
// Add Project form state
pub new_project_field: u8,      // 0 = name, 1 = path
pub new_project_name: String,
pub new_project_path: String,

// Confirm delete state
pub confirm_target: String,     // display label for "Delete <name>?"
pub confirm_project_id: String, // project ID to delete on confirm
```

Initialize them in `App::new()`:

```rust
new_project_field: 0,
new_project_name: String::new(),
new_project_path: String::new(),
confirm_target: String::new(),
confirm_project_id: String::new(),
```

**Step 2: Add key handler routing in `App::run()`**

In `src/tui/app.rs:240-253`, add the two new modes to the match:

```rust
InputMode::NewProject => self.handle_new_project_key(key.code)?,
InputMode::ConfirmDelete => self.handle_confirm_delete_key(key.code)?,
```

**Step 3: Add `handle_new_project_key` method**

```rust
fn handle_new_project_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => {
            if self.new_project_field == 0 && !self.input_buffer.is_empty() {
                // Commit name, move to path field
                self.new_project_name = std::mem::take(&mut self.input_buffer);
                self.new_project_field = 1;
                // Pre-fill path with "." as default
                self.input_buffer = String::from(".");
            } else if self.new_project_field == 1 && !self.input_buffer.is_empty() {
                // Commit path, create project
                self.new_project_path = std::mem::take(&mut self.input_buffer);
                match std::fs::canonicalize(&self.new_project_path) {
                    Ok(abs_path) => {
                        if let Some(abs_str) = abs_path.to_str() {
                            self.store.create_project(&self.new_project_name, abs_str)?;
                        }
                    }
                    Err(_) => {
                        // Path invalid — silently ignore, user can retry
                    }
                }
                self.new_project_name.clear();
                self.new_project_path.clear();
                self.new_project_field = 0;
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
        }
        KeyCode::Tab | KeyCode::BackTab => {
            // Toggle between fields
            if self.new_project_field == 0 {
                self.new_project_name = std::mem::take(&mut self.input_buffer);
                self.input_buffer = std::mem::take(&mut self.new_project_path);
                self.new_project_field = 1;
            } else {
                self.new_project_path = std::mem::take(&mut self.input_buffer);
                self.input_buffer = std::mem::take(&mut self.new_project_name);
                self.new_project_field = 0;
            }
        }
        KeyCode::Esc => {
            self.input_buffer.clear();
            self.new_project_name.clear();
            self.new_project_path.clear();
            self.new_project_field = 0;
            self.input_mode = InputMode::Normal;
        }
        KeyCode::Char(c) => {
            self.input_buffer.push(c);
        }
        KeyCode::Backspace => {
            self.input_buffer.pop();
        }
        _ => {}
    }
    Ok(())
}
```

**Step 4: Wire `a` key to open AddProject form from Normal mode**

In `handle_normal_key`, add before the catch-all `_ => {}`:

```rust
(KeyCode::Char('a'), _) => {
    self.input_mode = InputMode::NewProject;
    self.input_buffer.clear();
    self.new_project_name.clear();
    self.new_project_path.clear();
    self.new_project_field = 0;
}
```

**Step 5: Verify it compiles**

Run: `cargo build 2>&1 | head -30`
Expected: Compiles (UI rendering not wired yet, but no errors since unused fields are allowed during development)

**Step 6: Commit**

```
feat(tui): add NewProject input mode and key handler
```

---

### Task 2: Render the Add Project form in the status bar

**Files:**
- Modify: `src/tui/ui.rs:136-176` (status bar in `draw_active`)

**Step 1: Add NewProject rendering to the status bar**

In `draw_active()`, the status bar section (lines 136-176) already has `if/else if` chains for `NewTask` and `NewSession`. Add `NewProject` before the `else`:

```rust
} else if app.input_mode == InputMode::NewProject {
    let (label, hint) = if app.new_project_field == 0 {
        ("Project name: ", "(Enter: next field, Esc: cancel)")
    } else {
        ("Repo path: ", "(Enter: create, Tab: back, Esc: cancel)")
    };
    Line::from(vec![
        Span::styled(format!(" {label}"), Style::default().fg(Color::Magenta)),
        Span::raw(&app.input_buffer),
        Span::styled("\u{2588}", Style::default().fg(Color::Magenta)),
        Span::styled(
            format!("  {hint}"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
} else if app.input_mode == InputMode::ConfirmDelete {
```

Also add a stub for `ConfirmDelete` (will be filled in Task 3):

```rust
} else if app.input_mode == InputMode::ConfirmDelete {
    Line::from(vec![
        Span::styled(
            format!(" Delete '{}'? ", app.confirm_target),
            Style::default().fg(Color::Red),
        ),
        Span::styled(
            "(y: confirm, Esc: cancel)",
            Style::default().fg(Color::DarkGray),
        ),
    ])
} else {
```

**Step 2: Add `a` to the help hint in the title bar**

In `draw_active()` (line 111), update the hints string to include `a:project`:

```rust
"Tab:cycle view  a:project  n:task  s:session  q:quit",
```

**Step 3: Update the empty-projects message**

In `draw_projects()` (line 193), change the hint:

```rust
let msg = Paragraph::new("  No projects yet.\n  Press 'a' to add one.")
```

**Step 4: Add `NewProject` to the `InputMode` import in ui.rs**

The existing import at line 11 is:
```rust
use super::app::{App, Focus, InputMode, View};
```
This already imports `InputMode`, so no change needed (all variants are accessible).

**Step 5: Add palette entry for Add Project**

In `src/tui/app.rs`, add `AddProject` to `PaletteAction` enum:

```rust
pub enum PaletteAction {
    NewTask,
    NewSession,
    AddProject,       // <-- new
    RemoveProject,    // <-- new (for Task 3)
    ToggleView,
    // ... rest
}
```

Add corresponding `PaletteItem` entries in `App::new()` palette_items:

```rust
PaletteItem {
    label: "Add Project".into(),
    action: PaletteAction::AddProject,
},
PaletteItem {
    label: "Remove Project".into(),
    action: PaletteAction::RemoveProject,
},
```

Handle in `execute_palette_action`:

```rust
PaletteAction::AddProject => {
    self.input_mode = InputMode::NewProject;
    self.input_buffer.clear();
    self.new_project_name.clear();
    self.new_project_path.clear();
    self.new_project_field = 0;
}
PaletteAction::RemoveProject => {
    if let Some(project) = self.selected_project() {
        self.confirm_target = project.name.clone();
        self.confirm_project_id = project.id.clone();
        self.input_mode = InputMode::ConfirmDelete;
    }
}
```

**Step 6: Verify it compiles and renders**

Run: `cargo build 2>&1 | head -30`
Expected: Clean compilation

**Step 7: Commit**

```
feat(tui): render add-project form in status bar and command palette
```

---

### Task 3: Add Remove Project with confirmation

**Files:**
- Modify: `src/tui/app.rs` (handle_confirm_delete_key, `x` keybinding)

**Step 1: Add `handle_confirm_delete_key` method**

```rust
fn handle_confirm_delete_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char('y') => {
            if !self.confirm_project_id.is_empty() {
                self.store.delete_project(&self.confirm_project_id)?;
                self.confirm_project_id.clear();
                self.confirm_target.clear();
                self.input_mode = InputMode::Normal;
                self.project_index = 0;
                self.refresh_data()?;
            }
        }
        KeyCode::Esc | KeyCode::Char('n') => {
            self.confirm_project_id.clear();
            self.confirm_target.clear();
            self.input_mode = InputMode::Normal;
        }
        _ => {}
    }
    Ok(())
}
```

**Step 2: Wire `x` key in `handle_normal_key` (projects focused)**

Add before the catch-all:

```rust
(KeyCode::Char('x'), _) => {
    if self.focus == Focus::Projects
        && let Some(project) = self.selected_project()
    {
        self.confirm_target = project.name.clone();
        self.confirm_project_id = project.id.clone();
        self.input_mode = InputMode::ConfirmDelete;
    }
}
```

**Step 3: Verify and commit**

Run: `cargo build 2>&1 | head -30`

```
feat(tui): add remove-project with y/n confirmation
```

---

### Task 4: Enhance task creation with description and mode

**Files:**
- Modify: `src/tui/app.rs` (App struct, `handle_input_key`)
- Modify: `src/tui/ui.rs` (status bar rendering for NewTask)

**Step 1: Add task form state to App**

```rust
// Enhanced task form state
pub new_task_field: u8,           // 0 = title, 1 = description, 2 = mode
pub new_task_title: String,
pub new_task_description: String,
pub new_task_mode: TaskMode,
```

Initialize in `App::new()`:

```rust
new_task_field: 0,
new_task_title: String::new(),
new_task_description: String::new(),
new_task_mode: crate::store::TaskMode::Supervised,
```

**Step 2: Rewrite `handle_input_key` for multi-field flow**

The flow: title (Enter) -> description (Enter) -> mode (Tab to toggle, Enter to create)

```rust
fn handle_input_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => match self.new_task_field {
            0 => {
                if !self.input_buffer.is_empty() {
                    self.new_task_title = std::mem::take(&mut self.input_buffer);
                    self.new_task_field = 1;
                }
            }
            1 => {
                // Description can be empty
                self.new_task_description = std::mem::take(&mut self.input_buffer);
                self.new_task_field = 2;
            }
            _ => {
                // Create the task
                if let Some(project_id) = self.selected_project().map(|p| p.id.clone()) {
                    self.store.create_task(
                        &project_id,
                        &self.new_task_title,
                        &self.new_task_description,
                        self.new_task_mode,
                    )?;
                }
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
        },
        KeyCode::Tab | KeyCode::BackTab if self.new_task_field == 2 => {
            // Toggle mode on Tab when in mode field
            self.new_task_mode = match self.new_task_mode {
                crate::store::TaskMode::Supervised => crate::store::TaskMode::Autonomous,
                crate::store::TaskMode::Autonomous => crate::store::TaskMode::Supervised,
            };
        }
        KeyCode::Esc => {
            self.reset_task_form();
            self.input_mode = InputMode::Normal;
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

fn reset_task_form(&mut self) {
    self.input_buffer.clear();
    self.new_task_title.clear();
    self.new_task_description.clear();
    self.new_task_mode = crate::store::TaskMode::Supervised;
    self.new_task_field = 0;
}
```

Also update the `n` key handler and `PaletteAction::NewTask` handler to reset the form:

```rust
// In handle_normal_key, the 'n' key:
(KeyCode::Char('n'), _) => {
    if self.selected_project().is_some() {
        self.reset_task_form();
        self.input_mode = InputMode::NewTask;
    }
}

// In execute_palette_action:
PaletteAction::NewTask => {
    if self.selected_project().is_some() {
        self.reset_task_form();
        self.input_mode = InputMode::NewTask;
    }
}
```

**Step 3: Update status bar rendering for NewTask**

Replace the existing NewTask status bar rendering in `ui.rs`:

```rust
if app.input_mode == InputMode::NewTask {
    match app.new_task_field {
        0 => Line::from(vec![
            Span::styled(" Task title: ", Style::default().fg(Color::Yellow)),
            Span::raw(&app.input_buffer),
            Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
            Span::styled(
                "  (Enter: next, Esc: cancel)",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        1 => Line::from(vec![
            Span::styled(" Description: ", Style::default().fg(Color::Yellow)),
            Span::raw(&app.input_buffer),
            Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
            Span::styled(
                "  (Enter: next, Esc: cancel)",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        _ => Line::from(vec![
            Span::styled(" Mode: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                app.new_task_mode.as_str(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  (Tab: toggle, Enter: create, Esc: cancel)",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    }
}
```

**Step 4: Verify and commit**

Run: `cargo build 2>&1 | head -30`

```
feat(tui): multi-field task creation with description and mode
```

---

### Task 5: Update help hints and final polish

**Files:**
- Modify: `src/tui/ui.rs` (title bars, status bar hints)

**Step 1: Update Active view title bar hints**

Already done in Task 2, but verify the full hint reads:
```
Tab:cycle view  a:project  n:task  s:session  q:quit
```

**Step 2: Update Normal mode status bar hints**

In the status bar `else` branch (line 170-174), update to include `x` and `a`:

```rust
Line::from(Span::styled(
    " 1:projects  2:sessions  3:tasks  a:add project  x:remove  j/k:navigate",
    Style::default().fg(Color::DarkGray),
))
```

**Step 3: Run full checks**

Run: `cargo clippy 2>&1 | head -40`
Run: `cargo test 2>&1 | tail -20`
Run: `cargo fmt --check`

Fix any warnings.

**Step 4: Commit**

```
feat(tui): update help hints for new project/task operations
```
