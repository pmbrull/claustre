# Task-Centric Session Detail Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make the session detail pane show the session linked to the currently selected task, and remove independent session navigation entirely.

**Architecture:** Remove `Focus::Sessions` variant, drop `session_index`/`selected_session()`/`NewSession` input mode, and rewire `draw_session_detail()` to resolve the session from the selected task's `session_id`. Two files change: `src/tui/app.rs` (state + key handlers + tests) and `src/tui/ui.rs` (rendering).

**Tech Stack:** Rust, ratatui, crossterm

---

### Task 1: Remove `Focus::Sessions` from the enum and fix all match arms

**Files:**
- Modify: `src/tui/app.rs:27-31` (Focus enum)
- Modify: `src/tui/app.rs` (all `match self.focus` and `Focus::Sessions` references)

**Step 1: Remove `Sessions` from the `Focus` enum**

Change `src/tui/app.rs:27-31`:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Tasks,
}
```

**Step 2: Update focus-switching key handlers**

In `handle_normal_key()` at `src/tui/app.rs:568-570`, change:
```rust
(KeyCode::Char('1'), _) => self.focus = Focus::Projects,
(KeyCode::Char('2'), _) => self.focus = Focus::Tasks,
```
Remove the `Char('3')` line entirely.

**Step 3: Update `move_down()` at `src/tui/app.rs:1221-1243`**

Remove the `Focus::Sessions` arm:
```rust
fn move_down(&mut self) {
    match self.focus {
        Focus::Projects => {
            if !self.projects.is_empty() {
                self.project_index = (self.project_index + 1).min(self.projects.len() - 1);
                let _ = self.refresh_data();
                self.session_index = 0;
                self.task_index = 0;
            }
        }
        Focus::Tasks => {
            let visible_count = self.visible_tasks().len();
            if visible_count > 0 {
                self.task_index = (self.task_index + 1).min(visible_count - 1);
            }
        }
    }
}
```

**Step 4: Update `move_up()` at `src/tui/app.rs:1246-1261`**

Remove the `Focus::Sessions` arm:
```rust
fn move_up(&mut self) {
    match self.focus {
        Focus::Projects => {
            self.project_index = self.project_index.saturating_sub(1);
            let _ = self.refresh_data();
            self.session_index = 0;
            self.task_index = 0;
        }
        Focus::Tasks => {
            self.task_index = self.task_index.saturating_sub(1);
        }
    }
}
```

**Step 5: Update `Enter` key handler at `src/tui/app.rs:614-635`**

Remove `Focus::Sessions` arm (keep Projects and Tasks):
```rust
(KeyCode::Enter, _) => {
    match self.focus {
        Focus::Projects => {
            self.refresh_data()?;
            self.session_index = 0;
            self.task_index = 0;
        }
        Focus::Tasks => {}
    }
}
```

**Step 6: Update `d` (delete) key handler at `src/tui/app.rs:756-790`**

Remove `Focus::Sessions` arm entirely. Keep `Focus::Projects` and `Focus::Tasks`.

**Step 7: Update palette actions at `src/tui/app.rs:1359-1361`**

Change:
```rust
PaletteAction::FocusProjects => self.focus = Focus::Projects,
PaletteAction::FocusTasks => self.focus = Focus::Tasks,
```

**Step 8: Build and fix any remaining compilation errors**

Run: `cargo build 2>&1`
Expected: May have additional `Focus::Sessions` references in match arms that need removal. Fix them iteratively.

**Step 9: Commit**

```bash
git add src/tui/app.rs
git commit -m "refactor: remove Focus::Sessions enum variant and all match arms"
```

---

### Task 2: Remove `NewSession` input mode and related code

**Files:**
- Modify: `src/tui/app.rs:44-53` (InputMode enum)
- Modify: `src/tui/app.rs:70-82` (PaletteAction enum)
- Modify: `src/tui/app.rs:189-220` (palette_items construction)
- Modify: `src/tui/app.rs:513` (run loop NewSession dispatch)
- Modify: `src/tui/app.rs:637-653` (`s` key handler)
- Modify: `src/tui/app.rs:899-925` (`handle_session_input_key` method)
- Modify: `src/tui/app.rs:1324-1327` (palette action handler)
- Modify: `src/tui/ui.rs:51` (draw overlay for NewSession)
- Modify: `src/tui/ui.rs:1279-1323` (`draw_new_session_panel` function)

**Step 1: Remove `NewSession` from `InputMode` enum**

In `src/tui/app.rs:44-53`, remove the `NewSession` variant.

**Step 2: Remove `NewSession` and `FocusSessions` from `PaletteAction` enum**

In `src/tui/app.rs:69-82`, remove `NewSession` and `FocusSessions` variants.

**Step 3: Remove their entries from `palette_items` construction**

In `src/tui/app.rs:189-220`, remove the `PaletteItem` entries for "New Session" and "Focus Sessions".

**Step 4: Remove `NewSession` dispatch from the run loop**

In `src/tui/app.rs:513`, remove:
```rust
InputMode::NewSession => self.handle_session_input_key(key.code)?,
```

**Step 5: Simplify the `s` key handler**

In `src/tui/app.rs:637-653`, change to only open the subtask panel (remove the else branch that opened NewSession):
```rust
(KeyCode::Char('s'), _) => {
    if self.focus == Focus::Tasks && !self.visible_tasks().is_empty() {
        if let Some(task) = self.visible_tasks().get(self.task_index) {
            self.subtasks = self
                .store
                .list_subtasks_for_task(&task.id)
                .unwrap_or_default();
        }
        self.subtask_index = 0;
        self.input_buffer.clear();
        self.input_mode = InputMode::SubtaskPanel;
    }
}
```

**Step 6: Remove `handle_session_input_key` method entirely**

Delete `src/tui/app.rs:899-925`.

**Step 7: Remove palette action handlers for `NewSession` and `FocusSessions`**

In the `execute_palette_action` method, remove the arms for `PaletteAction::NewSession` and `PaletteAction::FocusSessions`.

**Step 8: Remove `draw_new_session_panel` from ui.rs**

Delete `src/tui/ui.rs:1279-1323` and remove `InputMode::NewSession` from the overlay match at `src/tui/ui.rs:51`.

**Step 9: Build and fix any remaining references**

Run: `cargo build 2>&1`

**Step 10: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "refactor: remove NewSession input mode and session-related palette actions"
```

---

### Task 3: Remove `session_index` and `selected_session()` from App state

**Files:**
- Modify: `src/tui/app.rs:108` (session_index field)
- Modify: `src/tui/app.rs:473-475` (selected_session method)
- Modify: `src/tui/app.rs:293-365` (refresh_data — session_index clamping)

**Step 1: Remove `session_index` field from App struct**

Remove line `src/tui/app.rs:108`. Also remove it from `App::new()` initialization.

**Step 2: Remove `selected_session()` method**

Delete `src/tui/app.rs:473-475`.

**Step 3: Add a `session_for_selected_task()` helper method**

Add to `impl App`:
```rust
/// Returns the session linked to the currently selected task, if any.
pub fn session_for_selected_task(&self) -> Option<&Session> {
    let task = self.visible_tasks().into_iter().nth(self.task_index)?;
    let sid = task.session_id.as_deref()?;
    self.sessions.iter().find(|s| s.id == sid)
}
```

**Step 4: Remove `session_index` clamping from refresh_data()**

In `refresh_data()`, remove the lines:
```rust
if self.session_index >= self.sessions.len() && !self.sessions.is_empty() {
    self.session_index = self.sessions.len() - 1;
}
```

Also remove the `self.session_index = 0;` lines in `move_down`/`move_up` for Projects focus and in the `Enter` key handler — these are harmless to remove since the field no longer exists.

**Step 5: Build and fix any remaining references to session_index or selected_session**

Run: `cargo build 2>&1`

**Step 6: Commit**

```bash
git add src/tui/app.rs
git commit -m "refactor: replace session_index/selected_session with session_for_selected_task"
```

---

### Task 4: Rewire `draw_session_detail()` to use selected task's session

**Files:**
- Modify: `src/tui/ui.rs:323-422` (draw_session_detail function)
- Modify: `src/tui/ui.rs:215-221` (status bar hints)
- Modify: `src/tui/ui.rs:134-147` (title bar hints)

**Step 1: Rewrite `draw_session_detail()` to resolve session from task**

Replace `src/tui/ui.rs:323-422`:
```rust
fn draw_session_detail(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = Style::default().fg(Color::DarkGray);

    let block = Block::default()
        .title(" Session Detail ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let visible = app.visible_tasks();
    if visible.is_empty() {
        let msg = Paragraph::new("  No tasks")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let Some(session) = app.session_for_selected_task() else {
        let msg = Paragraph::new("  No session — press l to launch")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    };

    let status_color = match session.claude_status {
        ClaudeStatus::Working => Color::Green,
        ClaudeStatus::WaitingForInput => Color::Yellow,
        ClaudeStatus::Error => Color::Red,
        ClaudeStatus::Done => Color::Blue,
        ClaudeStatus::Idle => Color::DarkGray,
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Branch: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.branch_name, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                session.claude_status.symbol(),
                Style::default().fg(status_color),
            ),
            Span::raw(" "),
            Span::styled(
                session.claude_status.as_str(),
                Style::default().fg(status_color),
            ),
        ]),
    ];

    if !session.status_message.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  Message: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("\"{}\"", &session.status_message),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("  Files: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{} changed (+{} -{})",
                session.files_changed, session.lines_added, session.lines_removed
            ),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::styled("  Last activity: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&session.last_activity_at, Style::default().fg(Color::White)),
    ]));

    // Show task info (PR URL, mode)
    if let Some(task) = app.visible_tasks().into_iter().nth(app.task_index) {
        if let Some(ref url) = task.pr_url {
            lines.push(Line::from(vec![
                Span::styled("  PR: ", Style::default().fg(Color::DarkGray)),
                Span::styled(url, Style::default().fg(Color::Magenta)),
            ]));
        }
    }

    let p = Paragraph::new(lines).block(block).wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}
```

**Step 2: Update status bar hints**

In `src/tui/ui.rs:215-221`, change to only have Projects and Tasks:
```rust
let hints = match app.focus {
    Focus::Projects => " a:add  d:delete  n:task  j/k:nav  ?:help",
    Focus::Tasks => {
        " n:new  e:edit  s:subtasks  l:launch  r:review  o:PR  d:del  /:filter  J/K:reorder  ?:help"
    }
};
```

**Step 3: Update title bar hints**

In `src/tui/ui.rs:134-147`, change the hint text:
```rust
"Tab:cycle  a:project  n:task  l:launch  q:quit",
```
(Remove `s:session` from the hint string.)

**Step 4: Build and verify**

Run: `cargo build 2>&1`
Expected: Clean build.

**Step 5: Commit**

```bash
git add src/tui/ui.rs
git commit -m "feat: session detail pane now shows session linked to selected task"
```

---

### Task 5: Update and fix tests

**Files:**
- Modify: `src/tui/app.rs` (test module, ~lines 1980-3176)

**Step 1: Fix `focus_switching_with_numbers` test**

Change `src/tui/app.rs:2013-2026`:
```rust
#[test]
fn focus_switching_with_numbers() {
    let mut app = test_app();
    assert_eq!(app.focus, Focus::Projects);

    press(&mut app, KeyCode::Char('2'));
    assert_eq!(app.focus, Focus::Tasks);

    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.focus, Focus::Projects);
}
```

**Step 2: Remove session-related tests**

Delete these tests entirely:
- `new_session_opens_form` (~line 2511)
- `new_session_requires_project` (~line 2519)
- `new_session_cancel` (~line 2526)
- `new_session_typing_and_backspace` (~line 2536)
- `snapshot_new_session_panel` (~line 2970)

**Step 3: Fix `subtask_panel_requires_tasks_focus` test**

Change `src/tui/app.rs:3135-3143`:
```rust
#[test]
fn subtask_panel_requires_tasks_focus() {
    let mut app = test_app_with_tasks();
    // Focus is Projects by default
    assert_eq!(app.focus, Focus::Projects);
    press(&mut app, KeyCode::Char('s'));
    // Should be no-op since focus is not Tasks
    assert_eq!(app.input_mode, InputMode::Normal);
}
```

**Step 4: Fix any remaining test compilation errors**

Run: `cargo test 2>&1`
Fix any test that references `Focus::Sessions`, `InputMode::NewSession`, `session_index`, or `selected_session()`.

**Step 5: Run full test suite**

Run: `cargo test 2>&1`
Expected: All tests pass.

**Step 6: Run clippy**

Run: `cargo clippy 2>&1`
Expected: No warnings.

**Step 7: Commit**

```bash
git add src/tui/app.rs
git commit -m "test: update tests for task-centric session detail redesign"
```
