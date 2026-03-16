# Enhance GitHub Board: Filtering & Pagination

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Improve the sprint board with assignee filtering (`@me`), pagination, text search, `b` key hint, and default `sprint:@current` selection.

**Architecture:** Five enhancements to the existing sprint board. The `gh` CLI fetches issues filtered to the current user. Client-side text filtering (title/number/labels) is layered on top. Pagination uses smooth scrolling with page-up/page-down. On first open, the current milestone is auto-selected.

**Tech Stack:** Rust, ratatui TUI, `gh` CLI, SQLite (no schema changes)

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/github.rs` | Modify | Add `--assignee @me` to `fetch_issues()`, add `fetch_gh_username()` |
| `src/tui/app/mod.rs` | Modify | Add `board_filter`, `board_filter_cursor`, `board_first_load`, `BoardFilter` input mode |
| `src/tui/app/input.rs` | Modify | Board key handlers: `/` filter, `G`/`gg` jump, page-up/down, auto-select current milestone |
| `src/tui/ui/board.rs` | Modify | `b/Esc:back` hint, `/` filter hint, filter input bar, filter status in header |
| `src/tui/ui/mod.rs` | Modify | Route `BoardFilter` input mode to board rendering |

---

## Chunk 1: All Five Enhancements

### Task 1: Filter issues to `@me` assignee

**Files:**
- Modify: `src/github.rs:53-78` (`fetch_issues` function)

- [ ] **Step 1: Write the test for `--assignee @me` argument**

Add a test that verifies `fetch_issues` passes `--assignee @me` in the args. Since this calls an external process, we'll test the `assign_column` logic separately and verify the `--assignee` flag is documented. Instead, write a unit test for the existing `assign_column` with assignee data to confirm the struct supports it.

Actually, since `fetch_issues` shells out to `gh`, we test this integration-style. The key change is just adding two args. Skip unit test for this trivial change.

- [ ] **Step 2: Add `--assignee @me` to `fetch_issues`**

In `src/github.rs`, in `fetch_issues()`, add `"--assignee"` and `"@me"` to the args vec:

```rust
let mut args = vec![
    "issue", "list", "--json", fields, "--limit", "100", "--state", "all",
    "--assignee", "@me",
];
```

- [ ] **Step 3: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

---

### Task 2: Add `BoardFilter` input mode and state fields

**Files:**
- Modify: `src/tui/app/mod.rs:67-84` (InputMode enum)
- Modify: `src/tui/app/mod.rs:225-235` (board state fields)

- [ ] **Step 1: Add `BoardFilter` variant to `InputMode`**

In `src/tui/app/mod.rs`, add `BoardFilter` after `MilestoneFilter`:

```rust
pub(crate) enum InputMode {
    // ... existing variants ...
    BoardView,
    MilestoneFilter,
    BoardFilter,  // <-- new
}
```

- [ ] **Step 2: Add board filter state fields**

In the `App` struct, after `board_error`, add:

```rust
// Board text filter state
pub board_filter: String,
pub board_filter_cursor: usize,
pub board_first_load: bool,
```

- [ ] **Step 3: Initialize new fields in `initialization.rs`**

In `src/tui/app/initialization.rs`, in the `App` struct literal (around line 187), add after `board_error: None,`:

```rust
board_filter: String::new(),
board_filter_cursor: 0,
board_first_load: true,
```

- [ ] **Step 4: Build and verify**

Run: `cargo build`
Expected: compiles cleanly (some unused field warnings OK for now)

---

### Task 3: Default `sprint:@current` on first board load

**Files:**
- Modify: `src/tui/app/input.rs:1991-2036` (`load_board_issues` function)
- Modify: `src/tui/app/input.rs:938-949` (`Action::OpenBoard` handler)

- [ ] **Step 1: Auto-select current milestone on first load**

In `load_board_issues()`, after fetching milestones (line ~2026-2028), add logic to auto-select current milestone on first load:

```rust
// Also fetch milestones
if let Ok(milestones) = crate::github::fetch_milestones(&repo_path) {
    self.board_milestones = milestones;
}

// Auto-select current sprint on first load
if self.board_first_load {
    self.board_first_load = false;
    if let Some(current) = crate::github::current_milestone(&self.board_milestones) {
        self.board_milestone_filter = Some(current.title.clone());
        // Re-fetch with milestone filter
        let milestone = self.board_milestone_filter.clone();
        match crate::github::fetch_issues(&repo_path, milestone.as_deref()) {
            Ok(issues) => {
                let mut grouped: Vec<Vec<crate::github::GitHubIssue>> =
                    (0..col_count).map(|_| Vec::new()).collect();
                for issue in issues {
                    let col = crate::github::assign_column(&issue, &column_labels);
                    if col < col_count {
                        grouped[col].push(issue);
                    }
                }
                self.board_issues = grouped;
                self.board_error = None;
            }
            Err(e) => {
                self.board_error = Some(format!("{e}"));
            }
        }
    }
}
```

Wait — this duplicates the fetch logic. Better approach: restructure so milestone is determined *before* fetch. Move milestone fetch before issue fetch:

```rust
pub(super) fn load_board_issues(&mut self) {
    let Some(project) = self.selected_project() else { return; };
    if !project.is_git_linked {
        self.board_error = Some("Project not linked to git".to_string());
        return;
    }

    let repo_path = project.repo_path.clone();
    let column_labels = self.config.board.column_labels();
    let col_count = self.board_columns.len();

    // Fetch milestones first (needed for auto-select)
    if let Ok(milestones) = crate::github::fetch_milestones(&repo_path) {
        self.board_milestones = milestones;
    }

    // Auto-select current sprint on first load
    if self.board_first_load {
        self.board_first_load = false;
        if let Some(current) = crate::github::current_milestone(&self.board_milestones) {
            self.board_milestone_filter = Some(current.title.clone());
        }
    }

    let milestone = self.board_milestone_filter.clone();

    match crate::github::fetch_issues(&repo_path, milestone.as_deref()) {
        Ok(issues) => {
            let mut grouped: Vec<Vec<crate::github::GitHubIssue>> =
                (0..col_count).map(|_| Vec::new()).collect();
            for issue in issues {
                let col = crate::github::assign_column(&issue, &column_labels);
                if col < col_count {
                    grouped[col].push(issue);
                }
            }
            self.board_issues = grouped;
            self.board_error = None;
        }
        Err(e) => {
            self.board_error = Some(format!("{e}"));
            self.board_issues = (0..col_count).map(|_| Vec::new()).collect();
        }
    }

    self.board_column_index = self.board_column_index.min(col_count.saturating_sub(1));
    if let Some(col_issues) = self.board_issues.get(self.board_column_index) {
        self.board_issue_index = self.board_issue_index.min(col_issues.len().saturating_sub(1));
    }
}
```

- [ ] **Step 2: Reset `board_first_load` when switching projects**

In the `Action::OpenBoard` handler, reset `board_first_load = true` so changing projects re-triggers auto-select:

Actually no — `board_first_load` should reset per board open, not per project. Set it true before `load_board_issues`:

```rust
Action::OpenBoard => {
    if let Some(project) = self.selected_project() {
        if project.is_git_linked {
            self.board_column_index = 0;
            self.board_issue_index = 0;
            self.board_first_load = true;  // <-- re-trigger auto-select
            self.board_filter.clear();     // <-- clear filter
            self.board_filter_cursor = 0;
            self.load_board_issues();
            self.input_mode = InputMode::BoardView;
        } else {
            self.show_toast("Project not linked to git", ToastStyle::Info);
        }
    }
}
```

Also update the `PaletteAction::SprintBoard` handler similarly.

- [ ] **Step 3: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

- [ ] **Step 4: Commit**

```bash
git add src/github.rs src/tui/app/mod.rs src/tui/app/input.rs src/tui/app/initialization.rs
git commit -m "feat(board): filter issues to @me assignee and auto-select current sprint"
```

---

### Task 4: Board text filter (`/` key)

**Files:**
- Modify: `src/tui/app/input.rs:1852-1908` (board key handler)
- Modify: `src/tui/app/input.rs:220-266` (paste handler)
- Modify: `src/tui/app/input.rs:48-52` (dispatch)

- [ ] **Step 1: Add `/` key to board view handler**

In `handle_board_key()`, add a case for `/`:

```rust
KeyCode::Char('/') => {
    self.board_filter.clear();
    self.board_filter_cursor = 0;
    self.input_mode = InputMode::BoardFilter;
}
```

- [ ] **Step 2: Add `BoardFilter` dispatch in main key handler**

In `handle_key()` (around line 48-52), add the `BoardFilter` case:

```rust
InputMode::BoardFilter => self.handle_board_filter_key(code, modifiers)?,
```

- [ ] **Step 3: Add paste handler for `BoardFilter`**

In `handle_paste()` (around line 254-261), add the `BoardFilter` case:

```rust
InputMode::BoardFilter => {
    self.board_filter
        .insert_str(self.board_filter_cursor.min(self.board_filter.len()), text);
    self.board_filter_cursor =
        (self.board_filter_cursor + text.len()).min(self.board_filter.len());
    self.apply_board_filter();
}
```

- [ ] **Step 4: Write `handle_board_filter_key` method**

Add new method after `handle_milestone_filter_key`:

```rust
pub(super) fn handle_board_filter_key(
    &mut self,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    match code {
        KeyCode::Enter | KeyCode::Esc => {
            self.input_mode = InputMode::BoardView;
        }
        _ => {
            if crate::tui::form::apply_text_edit(
                &mut self.board_filter,
                &mut self.board_filter_cursor,
                code,
                modifiers,
            ) {
                self.apply_board_filter();
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Write `apply_board_filter` method**

The filter works client-side on the already-fetched `board_issues`. We need to store unfiltered issues to re-apply the filter. Add a field `board_all_issues` to hold the full set, and filter into `board_issues`.

Add field to `App` struct in `mod.rs`:

```rust
pub board_all_issues: Vec<Vec<crate::github::GitHubIssue>>,
```

Initialize in `initialization.rs`:

```rust
board_all_issues: vec![],
```

In `load_board_issues()`, after grouping issues into `grouped`, store them in both:

```rust
self.board_all_issues = grouped.clone();
self.board_issues = grouped;
// Then apply any active filter
self.apply_board_filter();
```

The `apply_board_filter` method:

```rust
fn apply_board_filter(&mut self) {
    if self.board_filter.is_empty() {
        self.board_issues = self.board_all_issues.clone();
    } else {
        let query = self.board_filter.to_lowercase();
        self.board_issues = self
            .board_all_issues
            .iter()
            .map(|col| {
                col.iter()
                    .filter(|issue| {
                        issue.title.to_lowercase().contains(&query)
                            || issue.number.to_string().contains(&query)
                            || issue
                                .labels
                                .iter()
                                .any(|l| l.name.to_lowercase().contains(&query))
                            || issue
                                .assignees
                                .iter()
                                .any(|a| a.login.to_lowercase().contains(&query))
                    })
                    .cloned()
                    .collect()
            })
            .collect();
    }

    // Clamp selection
    let col_count = self.board_issues.len();
    self.board_column_index = self.board_column_index.min(col_count.saturating_sub(1));
    if let Some(col_issues) = self.board_issues.get(self.board_column_index) {
        self.board_issue_index = self.board_issue_index.min(col_issues.len().saturating_sub(1));
    } else {
        self.board_issue_index = 0;
    }
}
```

- [ ] **Step 6: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

- [ ] **Step 7: Commit**

```bash
git add src/tui/app/mod.rs src/tui/app/input.rs src/tui/app/initialization.rs
git commit -m "feat(board): add text filter with / key (searches title, number, labels)"
```

---

### Task 5: Pagination / improved scrolling in board columns

**Files:**
- Modify: `src/tui/app/input.rs:1852-1908` (board key handler)
- Modify: `src/github.rs:56` (increase limit)

- [ ] **Step 1: Increase fetch limit**

In `src/github.rs`, change `--limit` from `"100"` to `"500"` to support larger boards:

```rust
let mut args = vec![
    "issue", "list", "--json", fields, "--limit", "500", "--state", "all",
    "--assignee", "@me",
];
```

- [ ] **Step 2: Add page-up/page-down and G/gg navigation to board**

In `handle_board_key()`, add these keybindings:

```rust
// Page down in current column
KeyCode::Char('d') if modifiers == KeyModifiers::CONTROL => {
    if let Some(col_issues) = self.board_issues.get(self.board_column_index) {
        let page = 10;
        self.board_issue_index =
            (self.board_issue_index + page).min(col_issues.len().saturating_sub(1));
    }
}
// Page up in current column
KeyCode::Char('u') if modifiers == KeyModifiers::CONTROL => {
    self.board_issue_index = self.board_issue_index.saturating_sub(10);
}
// Jump to bottom of column
KeyCode::Char('G') => {
    if let Some(col_issues) = self.board_issues.get(self.board_column_index) {
        self.board_issue_index = col_issues.len().saturating_sub(1);
    }
}
// Jump to top of column
KeyCode::Char('g') => {
    self.board_issue_index = 0;
}
```

- [ ] **Step 3: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

- [ ] **Step 4: Commit**

```bash
git add src/github.rs src/tui/app/input.rs
git commit -m "feat(board): add page navigation (Ctrl+D/U, G/g) and increase issue limit"
```

---

### Task 6: Update board UI — hints, filter bar, `BoardFilter` routing

**Files:**
- Modify: `src/tui/ui/board.rs:29-84` (header and hints)
- Modify: `src/tui/ui/board.rs:18-27` (layout for filter bar)
- Modify: `src/tui/ui/mod.rs:58-68` (routing)

- [ ] **Step 1: Update hints line to show `b` and `/`**

In `draw_board_header()`, update the hints line to include `b/Esc:back` (instead of just `Esc:back`) and add `/:filter`:

```rust
let hints = Line::from(vec![
    Span::styled("  h/l", Style::default().fg(theme.text_accent)),
    Span::styled(":column  ", Style::default().fg(theme.text_secondary)),
    Span::styled("j/k", Style::default().fg(theme.text_accent)),
    Span::styled(":issue  ", Style::default().fg(theme.text_secondary)),
    Span::styled("Enter", Style::default().fg(theme.text_accent)),
    Span::styled(":create task  ", Style::default().fg(theme.text_secondary)),
    Span::styled("o", Style::default().fg(theme.text_accent)),
    Span::styled(":open  ", Style::default().fg(theme.text_secondary)),
    Span::styled("/", Style::default().fg(theme.text_accent)),
    Span::styled(":filter  ", Style::default().fg(theme.text_secondary)),
    Span::styled("m", Style::default().fg(theme.text_accent)),
    Span::styled(":milestone  ", Style::default().fg(theme.text_secondary)),
    Span::styled("R", Style::default().fg(theme.text_accent)),
    Span::styled(":refresh  ", Style::default().fg(theme.text_secondary)),
    Span::styled("b/Esc", Style::default().fg(theme.text_accent)),
    Span::styled(":back", Style::default().fg(theme.text_secondary)),
]);
```

- [ ] **Step 2: Add filter bar to board layout**

Modify `draw_board()` to show a filter input bar when `BoardFilter` mode is active or when a filter is set:

```rust
pub(super) fn draw_board(frame: &mut Frame, app: &App, area: Rect) {
    let show_filter_bar = app.input_mode == InputMode::BoardFilter || !app.board_filter.is_empty();
    let constraints = if show_filter_bar {
        vec![
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Min(0),
        ]
    } else {
        vec![Constraint::Length(2), Constraint::Min(0)]
    };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    draw_board_header(frame, app, layout[0]);

    if show_filter_bar {
        draw_filter_bar(frame, app, layout[1]);
        draw_board_columns(frame, app, layout[2]);
    } else {
        draw_board_columns(frame, app, layout[1]);
    }
}
```

- [ ] **Step 3: Write `draw_filter_bar` function**

```rust
fn draw_filter_bar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let is_editing = app.input_mode == InputMode::BoardFilter;

    let label_style = Style::default().fg(if is_editing {
        theme.text_accent
    } else {
        theme.text_secondary
    });
    let text_style = Style::default().fg(theme.text_primary);

    let filter_text = if app.board_filter.is_empty() && !is_editing {
        String::new()
    } else {
        app.board_filter.clone()
    };

    let filtered_total: usize = app.board_issues.iter().map(Vec::len).sum();
    let all_total: usize = app.board_all_issues.iter().map(Vec::len).sum();

    let mut spans = vec![
        Span::styled(" / ", label_style),
        Span::styled(&filter_text, text_style),
    ];

    if !app.board_filter.is_empty() {
        spans.push(Span::styled(
            format!("  ({filtered_total}/{all_total})"),
            Style::default().fg(theme.text_secondary),
        ));
    }

    if is_editing {
        spans.push(Span::styled("▏", Style::default().fg(theme.text_accent)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
```

- [ ] **Step 4: Update `ui/mod.rs` to route `BoardFilter` mode**

In `draw()`, update the board rendering condition to include `BoardFilter`:

```rust
if app.input_mode == InputMode::BoardView
    || app.input_mode == InputMode::MilestoneFilter
    || app.input_mode == InputMode::BoardFilter
{
    let board_area = frame.area();
    frame.render_widget(Clear, board_area);
    draw_board(frame, app, board_area);

    if app.input_mode == InputMode::MilestoneFilter {
        draw_milestone_overlay(frame, app);
    }
    return;
}
```

- [ ] **Step 5: Show filter status in header**

In `draw_board_header()`, add filter indicator to the header line when a filter is active:

After the `(N issues)` span, add:

```rust
if !app.board_filter.is_empty() {
    // already shown in filter bar
}
```

Actually, the filter bar already shows the count. No additional header change needed.

- [ ] **Step 6: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --workspace`
Expected: no warnings

- [ ] **Step 8: Run tests**

Run: `cargo test --workspace`
Expected: all pass

- [ ] **Step 9: Commit**

```bash
git add src/tui/ui/board.rs src/tui/ui/mod.rs
git commit -m "feat(board): add filter bar UI, b/Esc hint, and BoardFilter routing"
```

---

### Task 7: Update CLAUDE.md documentation

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Update the TUI key actions table and board documentation**

In CLAUDE.md, no changes needed to the main keybindings table since `b` is already documented as "Sprint board". The board-internal keybindings are documented in the board header hints, not in CLAUDE.md.

- [ ] **Step 2: Final commit**

```bash
git add -A
git commit -m "feat(board): enhance github board with @me filter, pagination, text search, current sprint default"
```
