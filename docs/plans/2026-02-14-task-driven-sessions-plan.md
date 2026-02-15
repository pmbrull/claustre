# Task-Driven Sessions Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Allow tasks to choose between a new isolated session (parallel) or the project's default session (sequential), and move review from per-task to per-session.

**Architecture:** Add `needs_new_session` boolean to tasks. Remove `InReview` from `TaskStatus` — tasks go `pending → in_progress → done`. Review moves to session level (`ClaudeStatus::Done` = needs review). Default sessions are identified by `branch_name = 'default'` convention and created lazily.

**Tech Stack:** Rust, rusqlite, ratatui, tokio (MCP server)

---

### Task 1: Remove `InReview` from `TaskStatus`

**Files:**
- Modify: `src/store/models.rs:14-65` (TaskStatus enum)
- Modify: `src/store/models.rs:208-283` (tests)

**Step 1: Write failing test**

In `src/store/models.rs` tests, add a test that `"in_review"` parses as `Done` (backward compat):

```rust
#[test]
fn task_status_in_review_parses_as_done() {
    assert_eq!("in_review".parse::<TaskStatus>().unwrap(), TaskStatus::Done);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib store::models::tests::task_status_in_review_parses_as_done`
Expected: FAIL — `"in_review"` returns `Err`

**Step 3: Remove `InReview` variant and add backward-compat parsing**

In `src/store/models.rs`, remove the `InReview` variant from `TaskStatus`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
    Error,
}
```

Update `as_str()` — remove the `InReview` arm.

Update `symbol()` — remove the `InReview` arm.

Update `FromStr` — map `"in_review"` to `Done` for backward compat with existing DB rows:

```rust
fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
        "pending" => Ok(Self::Pending),
        "in_progress" => Ok(Self::InProgress),
        "in_review" => Ok(Self::Done), // backward compat
        "done" => Ok(Self::Done),
        "error" => Ok(Self::Error),
        _ => Err(format!("unknown task status: {s}")),
    }
}
```

Update existing tests:
- `task_status_round_trip`: remove `InReview` from the list
- `task_status_symbols`: remove the `InReview` assertion

**Step 4: Fix all compilation errors**

The compiler will flag every reference to `TaskStatus::InReview`. Fix them all:

- `src/tui/app.rs:621-623` — `r` key handler: change match to only `TaskStatus::InProgress` (remove `InReview`). Actually, since `r` currently marks `InReview|InProgress → Done`, and now tasks go straight to `Done` via MCP, rethink what `r` does. For now, keep it working on `InProgress` tasks (manual mark-done).
- `src/tui/app.rs:2369-2413` — update review tests: `review_task_marks_done` should set task to `InProgress` (not `InReview`). Delete `review_only_works_on_in_review_tasks` test. Rename test to something like `review_marks_in_progress_done`.
- `src/tui/ui.rs:204` — status bar "needs attention" count: change from `InReview` to checking sessions with `ClaudeStatus::Done`. For now, remove the `InReview` filter and use `has_review_sessions` (will be added in Task 5).
- `src/tui/ui.rs:355` — session detail current task: remove `InReview` from filter, keep just `InProgress`.
- `src/tui/ui.rs:469` — task list item color: remove `InReview` arm.
- `src/tui/ui.rs:218` — hints: change `r:review` to `r:done` (since it now just marks tasks done).
- `src/mcp/mod.rs:588-613` — `claustre_status` "done" fallback: change `InReview` to `Done`.
- `src/mcp/mod.rs:615-666` — `claustre_task_done`: change `InReview` to `Done`.
- `src/mcp/mod.rs:1068-1088` — `claustre_status_done_transitions_task_to_in_review`: rename test, assert `Done` instead of `InReview`.
- `src/mcp/mod.rs:1093-1112` — `claustre_task_done_marks_in_review`: rename test, assert `Done`.
- `src/store/queries.rs:497-504` — `has_review_tasks`: keep for now but change to check `status = 'done'` with `closed_at IS NULL` on session. Will be replaced in Task 5.

**Step 5: Run full test suite**

Run: `cargo test`
Expected: All tests pass

**Step 6: Run clippy**

Run: `cargo clippy`
Expected: No warnings

**Step 7: Commit**

```bash
git add src/store/models.rs src/tui/app.rs src/tui/ui.rs src/mcp/mod.rs src/store/queries.rs
git commit -m "refactor: remove InReview task status, tasks go pending→in_progress→done"
```

---

### Task 2: Add `needs_new_session` to data model

**Files:**
- Modify: `src/store/mod.rs:86-92` (add Migration v5)
- Modify: `src/store/models.rs:101-119` (Task struct)
- Modify: `src/store/queries.rs:64-82` (create_task)
- Modify: `src/store/queries.rs:110-123` (update_task)
- Modify: `src/store/queries.rs:156-177` (row_to_task)
- Test: `src/store/queries.rs` (existing tests + new ones)

**Step 1: Write failing test**

In `src/store/queries.rs` tests, add:

```rust
#[test]
fn test_create_task_needs_new_session() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();

    // Default: needs_new_session = true
    let t1 = store
        .create_task(&project.id, "isolated", "", TaskMode::Autonomous, true)
        .unwrap();
    assert!(t1.needs_new_session);

    // Explicit: needs_new_session = false
    let t2 = store
        .create_task(&project.id, "queued", "", TaskMode::Autonomous, false)
        .unwrap();
    assert!(!t2.needs_new_session);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib store::queries::tests::test_create_task_needs_new_session`
Expected: FAIL — `create_task` doesn't accept 5th argument

**Step 3: Add migration v5**

In `src/store/mod.rs`, append to `MIGRATIONS`:

```rust
Migration {
    version: 5,
    sql: "
        ALTER TABLE tasks ADD COLUMN needs_new_session INTEGER NOT NULL DEFAULT 1;
    ",
},
```

**Step 4: Add field to Task struct**

In `src/store/models.rs`, add to `Task`:

```rust
pub needs_new_session: bool,
```

**Step 5: Update `create_task`**

In `src/store/queries.rs`, add `needs_new_session: bool` parameter:

```rust
pub fn create_task(
    &self,
    project_id: &str,
    title: &str,
    description: &str,
    mode: TaskMode,
    needs_new_session: bool,
) -> Result<Task> {
```

Update the INSERT to include the new column:

```rust
self.conn.execute(
    "INSERT INTO tasks (id, project_id, title, description, mode, sort_order, needs_new_session) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    params![id, project_id, title, description, mode.as_str(), max_order + 1, needs_new_session],
)?;
```

**Step 6: Update `row_to_task`**

Add reading column index 16 (after `pr_url` at 15):

```rust
needs_new_session: row.get::<_, i64>(16).map(|v| v != 0).unwrap_or(true),
```

Update ALL queries that use `row_to_task` to include `needs_new_session` in their SELECT column list. The affected queries are in:
- `get_task` (line ~86)
- `list_tasks_for_project` (line ~98)
- `next_pending_task_for_session` (line ~258)

Each SELECT needs `, needs_new_session` appended after `pr_url`.

**Step 7: Update `update_task`**

Add `needs_new_session: bool` parameter:

```rust
pub fn update_task(
    &self,
    id: &str,
    title: &str,
    description: &str,
    mode: TaskMode,
    needs_new_session: bool,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    self.conn.execute(
        "UPDATE tasks SET title = ?1, description = ?2, mode = ?3, updated_at = ?4, needs_new_session = ?5 WHERE id = ?6",
        params![title, description, mode.as_str(), now, needs_new_session, id],
    )?;
    Ok(())
}
```

**Step 8: Fix all existing callers**

Every call to `create_task` and `update_task` needs the new parameter. Pass `true` to preserve existing behavior:

- `src/tui/app.rs` — `handle_input_key` (line ~797): pass `self.new_task_needs_new_session` (added in Task 4)
- `src/tui/app.rs` — `handle_edit_task_key` (line ~1554): pass `self.new_task_needs_new_session`
- `src/store/queries.rs` tests — all `create_task` calls: add `true` as 5th arg
- `src/tui/app.rs` tests — all `create_task` calls: add `true` as 5th arg
- `src/mcp/mod.rs` tests (if any `create_task` calls): add `true`

For now, hardcode `true` in TUI callers to keep behavior unchanged. Task 4 will wire up the form field.

**Step 9: Run tests**

Run: `cargo test`
Expected: All pass

**Step 10: Run clippy**

Run: `cargo clippy`
Expected: No warnings

**Step 11: Commit**

```bash
git add src/store/mod.rs src/store/models.rs src/store/queries.rs src/tui/app.rs src/mcp/mod.rs
git commit -m "feat: add needs_new_session column to tasks (migration v5)"
```

---

### Task 3: Add default session query

**Files:**
- Modify: `src/store/queries.rs` (add `get_default_session`)
- Test: `src/store/queries.rs`

**Step 1: Write failing test**

```rust
#[test]
fn test_get_default_session() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();

    // No default session yet
    assert!(store.get_default_session(&project.id).unwrap().is_none());

    // Create a default session
    let session = store
        .create_session(&project.id, "default", "/tmp/wt", "proj:default")
        .unwrap();
    let found = store.get_default_session(&project.id).unwrap().unwrap();
    assert_eq!(found.id, session.id);

    // Close it — should return None again
    store.close_session(&session.id).unwrap();
    assert!(store.get_default_session(&project.id).unwrap().is_none());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib store::queries::tests::test_get_default_session`
Expected: FAIL — method doesn't exist

**Step 3: Implement `get_default_session`**

In `src/store/queries.rs`, add to the Sessions section:

```rust
/// Find the active default session for a project (branch_name = 'default', not closed).
pub fn get_default_session(&self, project_id: &str) -> Result<Option<Session>> {
    let result = self.conn.query_row(
        "SELECT id, project_id, branch_name, worktree_path, zellij_tab_name,
                claude_status, status_message, last_activity_at,
                files_changed, lines_added, lines_removed,
                created_at, closed_at
         FROM sessions
         WHERE project_id = ?1 AND branch_name = 'default' AND closed_at IS NULL
         LIMIT 1",
        params![project_id],
        Self::row_to_session,
    );
    match result {
        Ok(session) => Ok(Some(session)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}
```

**Step 4: Run test**

Run: `cargo test --lib store::queries::tests::test_get_default_session`
Expected: PASS

**Step 5: Also add `has_review_sessions` query**

This replaces `has_review_tasks` for the sidebar review indicator. A session "needs review" when `claude_status = 'done'` and it's still open.

Write the test first:

```rust
#[test]
fn test_has_review_sessions() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();
    let session = store
        .create_session(&project.id, "b", "/tmp/wt", "tab")
        .unwrap();

    assert!(!store.has_review_sessions(&project.id).unwrap());

    store
        .update_session_status(&session.id, ClaudeStatus::Done, "all done")
        .unwrap();
    assert!(store.has_review_sessions(&project.id).unwrap());

    store.close_session(&session.id).unwrap();
    assert!(!store.has_review_sessions(&project.id).unwrap());
}
```

Implement:

```rust
pub fn has_review_sessions(&self, project_id: &str) -> Result<bool> {
    let has: bool = self.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE project_id = ?1 AND claude_status = 'done' AND closed_at IS NULL)",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(has)
}
```

**Step 6: Replace `has_review_tasks` with `has_review_sessions` in TUI**

In `src/tui/app.rs:1777`, change:

```rust
let has_review = store.has_review_sessions(&project.id).unwrap_or(false);
```

Remove `has_review_tasks` from `src/store/queries.rs` (dead code).

**Step 7: Update the status bar in `src/tui/ui.rs:201-212`**

Change the "needs attention" count from filtering tasks by `InReview` to checking sessions with `ClaudeStatus::Done`:

```rust
let needs_attention = app
    .sessions
    .iter()
    .filter(|s| s.claude_status == ClaudeStatus::Done)
    .count();
if needs_attention > 0 {
    Line::from(vec![Span::styled(
        format!(" {needs_attention} session(s) ready for review "),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )])
}
```

**Step 8: Run tests + clippy**

Run: `cargo test && cargo clippy`
Expected: All pass, no warnings

**Step 9: Commit**

```bash
git add src/store/queries.rs src/tui/app.rs src/tui/ui.rs
git commit -m "feat: add default session query and session-level review indicators"
```

---

### Task 4: Add session field to task form in TUI

**Files:**
- Modify: `src/tui/app.rs:90-167` (App struct — add `new_task_needs_new_session`)
- Modify: `src/tui/app.rs:233-278` (App::new — init new field)
- Modify: `src/tui/app.rs:754-785` (handle_task_form_shared_key — 3 fields)
- Modify: `src/tui/app.rs:787-844` (handle_input_key — use new field)
- Modify: `src/tui/app.rs:848-860` (save/load field helpers)
- Modify: `src/tui/app.rs:1117-1122` (reset_task_form)
- Modify: `src/tui/app.rs:1544-1581` (handle_edit_task_key — use new field)
- Modify: `src/tui/ui.rs:1030-1128` (draw_task_form_panel — render session field)
- Test: `src/tui/app.rs` (existing + new tests)

**Step 1: Write failing test**

In `src/tui/app.rs` tests:

```rust
#[test]
fn task_form_has_session_field() {
    let mut app = test_app_with_project();
    app.input_mode = InputMode::NewTask;
    assert_eq!(app.new_task_field, 0); // prompt

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.new_task_field, 1); // mode

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.new_task_field, 2); // session

    // Toggle session field
    assert!(app.new_task_needs_new_session);
    press(&mut app, KeyCode::Right);
    assert!(!app.new_task_needs_new_session);
    press(&mut app, KeyCode::Left);
    assert!(app.new_task_needs_new_session);

    // Tab wraps back to prompt
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.new_task_field, 0);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib tui::app::tests::task_form_has_session_field`
Expected: FAIL — `new_task_needs_new_session` doesn't exist

**Step 3: Add state field to App**

In `src/tui/app.rs`, after line 116 (`new_task_mode`), add:

```rust
pub new_task_needs_new_session: bool,
```

Initialize in `App::new` (after line 249 `new_task_mode`):

```rust
new_task_needs_new_session: true,
```

**Step 4: Update `reset_task_form`**

In `src/tui/app.rs:1117-1122`, add:

```rust
self.new_task_needs_new_session = true;
```

**Step 5: Update `handle_task_form_shared_key` to cycle 3 fields**

Change `% 2` to `% 3` for Tab, and update BackTab:

```rust
KeyCode::Tab => {
    self.save_current_task_field();
    self.new_task_field = (self.new_task_field + 1) % 3;
    self.load_current_task_field();
    true
}
KeyCode::BackTab => {
    self.save_current_task_field();
    self.new_task_field = if self.new_task_field == 0 { 2 } else { self.new_task_field - 1 };
    self.load_current_task_field();
    true
}
```

Add session toggle for field 2:

```rust
KeyCode::Left | KeyCode::Right if self.new_task_field == 2 => {
    self.new_task_needs_new_session = !self.new_task_needs_new_session;
    true
}
```

**Step 6: Update `handle_input_key` — pass `needs_new_session` to `create_task`**

At line ~797, change:

```rust
let task = self.store.create_task(
    &project_id,
    &fallback,
    &self.new_task_description,
    self.new_task_mode,
    self.new_task_needs_new_session,
)?;
```

At line ~811, change the autonomous auto-launch to be conditional:

```rust
if self.new_task_mode == crate::store::TaskMode::Autonomous {
    if self.new_task_needs_new_session {
        // Existing behavior: create isolated session
        let branch_name = crate::session::generate_branch_name(&task.title);
        match crate::session::create_session(
            &self.store,
            &project_id,
            &branch_name,
            Some(&task),
        ) {
            Ok(_) => {
                self.show_toast("Autonomous task launched", ToastStyle::Success);
            }
            Err(e) => {
                self.show_toast(format!("Auto-launch failed: {e}"), ToastStyle::Error);
            }
        }
    } else {
        // Default session: find or create, assign task
        match crate::session::assign_to_default_session(
            &self.store,
            &project_id,
            &task,
        ) {
            Ok(fed) => {
                let msg = if fed {
                    "Task started in default session"
                } else {
                    "Task queued in default session"
                };
                self.show_toast(msg, ToastStyle::Success);
            }
            Err(e) => {
                self.show_toast(
                    format!("Default session failed: {e}"),
                    ToastStyle::Error,
                );
            }
        }
    }
}
```

**Step 7: Update `handle_edit_task_key` — pass `needs_new_session` to `update_task`**

At line ~1554:

```rust
self.store.update_task(
    task_id,
    &fallback,
    &self.new_task_description,
    self.new_task_mode,
    self.new_task_needs_new_session,
)?;
```

Also update the edit task entry point (around line 595-610) to load `needs_new_session`:

```rust
self.new_task_needs_new_session = task.needs_new_session;
```

**Step 8: Update `draw_task_form_panel` in `src/tui/ui.rs`**

After the Mode field (line ~1113), add a Session field. Increase the panel height by 2 (one for label, one for spacing). The new field goes between Mode and Hints:

```rust
// Field 2: Session
let label_s = if app.new_task_field == 2 { highlight } else { dim };
let session_label = if app.new_task_needs_new_session { "new" } else { "default" };
let arrow_hint = if app.new_task_field == 2 { "  (\u{2190}/\u{2192} toggle)" } else { "" };
frame.render_widget(
    Paragraph::new(Line::from(vec![
        Span::styled("  Session: ", label_s),
        Span::styled(session_label, mode_s),
        Span::styled(arrow_hint, dim),
    ])),
    Rect::new(inner.x, inner.y + 5 + extra, inner.width, 1),
);
```

Move the hints down by 2:

```rust
// Hints (was at y+5, now at y+7)
frame.render_widget(
    Paragraph::new(Line::from(vec![
        Span::styled("  Tab", highlight),
        Span::styled(":field  ", dim),
        Span::styled("Enter", highlight),
        Span::styled(":create  ", dim),
        Span::styled("Esc", highlight),
        Span::styled(":cancel", dim),
    ])),
    Rect::new(inner.x, inner.y + 7 + extra, inner.width, 1),
);
```

Update panel height calculation (line ~1052): change `7u16` to `9u16`.

**Step 9: Update existing tests that check field cycling**

Tests around line 2667-2714 that check `new_task_field` cycling with `% 2` — update expected values for 3-field form.

**Step 10: Run tests + clippy**

Run: `cargo test && cargo clippy`
Expected: All pass, no warnings

**Step 11: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat: add session field (new/default) to task creation form"
```

---

### Task 5: Implement `assign_to_default_session` in session module

**Files:**
- Modify: `src/session/mod.rs` (add `assign_to_default_session`)
- Test: `src/session/mod.rs` (unit tests for branch name, existing tests still pass)
- Test: `src/store/queries.rs` (integration test via store)

**Step 1: Write failing test**

In `src/store/queries.rs` tests (testing the DB side):

```rust
#[test]
fn test_default_session_task_assignment() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();
    let session = store
        .create_session(&project.id, "default", "/tmp/wt", "proj:default")
        .unwrap();

    let task = store
        .create_task(&project.id, "queued task", "do stuff", TaskMode::Autonomous, false)
        .unwrap();

    store.assign_task_to_session(&task.id, &session.id).unwrap();

    let next = store.next_pending_task_for_session(&session.id).unwrap().unwrap();
    assert_eq!(next.id, task.id);
    assert!(!next.needs_new_session);
}
```

**Step 2: Run test**

Run: `cargo test --lib store::queries::tests::test_default_session_task_assignment`
Expected: PASS (this validates the DB plumbing works)

**Step 3: Implement `assign_to_default_session`**

In `src/session/mod.rs`, add:

```rust
/// Assign a task to the project's default session. Creates the session if it doesn't exist.
/// If the session is idle, starts the task immediately.
/// Returns `Ok(true)` if the task was started, `Ok(false)` if queued.
pub fn assign_to_default_session(
    store: &Store,
    project_id: &str,
    task: &Task,
) -> Result<bool> {
    require_zellij()?;
    let project = store.get_project(project_id)?;

    // Find or create default session
    let session = if let Some(session) = store.get_default_session(project_id)? {
        session
    } else {
        // Create a new default session
        let repo_path = Path::new(&project.repo_path);
        let worktree_path = create_worktree(repo_path, &project.name, "default")?;
        write_merged_config(repo_path, &worktree_path)?;
        let tab_name = format!("{}:default", project.name);
        create_zellij_tab(&tab_name, &worktree_path)?;
        let worktree_str = worktree_path
            .to_str()
            .context("worktree path contains invalid UTF-8")?;
        let session = store.create_session(project_id, "default", worktree_str, &tab_name)?;
        write_mcp_config(&worktree_path, &session.id)?;
        pre_trust_worktree(&worktree_path);
        session
    };

    // Assign task to session
    store.assign_task_to_session(&task.id, &session.id)?;

    // If session is idle or done (no active work), start the task now
    let is_idle = matches!(
        session.claude_status,
        ClaudeStatus::Idle | ClaudeStatus::Done
    );
    let has_active = store.session_has_active_tasks(&session.id)?;

    if is_idle && !has_active {
        store.update_task_status(&task.id, TaskStatus::InProgress)?;
        store.update_session_status(
            &session.id,
            ClaudeStatus::Working,
            &format!("Starting: {}", task.title),
        )?;
        let prompt = if task.mode == TaskMode::Autonomous {
            format!("{}{AUTONOMOUS_SUFFIX}", task.description)
        } else {
            task.description.clone()
        };
        launch_claude_in_zellij(&session.zellij_tab_name, &prompt)?;
        return_to_claustre();
        Ok(true)
    } else {
        return_to_claustre();
        Ok(false)
    }
}
```

**Step 4: Run tests + clippy**

Run: `cargo test && cargo clippy`
Expected: All pass, no warnings

**Step 5: Commit**

```bash
git add src/session/mod.rs src/store/queries.rs
git commit -m "feat: implement assign_to_default_session for sequential task queuing"
```

---

### Task 6: Update `l` key (launch) to respect `needs_new_session`

**Files:**
- Modify: `src/tui/app.rs:667-697` (launch key handler)

**Step 1: Update launch handler**

The `l` key currently always creates a new session. Update it to check `needs_new_session`:

```rust
(KeyCode::Char('l'), _) => {
    let task_data = if self.focus == Focus::Tasks {
        self.visible_tasks()
            .get(self.task_index)
            .filter(|t| t.status == crate::store::TaskStatus::Pending)
            .map(|t| (t.id.clone(), t.needs_new_session))
    } else {
        None
    };
    if let Some((task_id, needs_new_session)) = task_data
        && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
    {
        let task = self.store.get_task(&task_id)?;
        if needs_new_session {
            let branch_name = crate::session::generate_branch_name(&task.title);
            match crate::session::create_session(
                &self.store,
                &project_id,
                &branch_name,
                Some(&task),
            ) {
                Ok(_session) => {
                    self.refresh_data()?;
                    self.show_toast("Session launched", ToastStyle::Success);
                }
                Err(e) => {
                    self.show_toast(format!("Launch failed: {e}"), ToastStyle::Error);
                }
            }
        } else {
            match crate::session::assign_to_default_session(
                &self.store,
                &project_id,
                &task,
            ) {
                Ok(fed) => {
                    self.refresh_data()?;
                    let msg = if fed {
                        "Task started in default session"
                    } else {
                        "Task queued in default session"
                    };
                    self.show_toast(msg, ToastStyle::Success);
                }
                Err(e) => {
                    self.show_toast(format!("Launch failed: {e}"), ToastStyle::Error);
                }
            }
        }
    }
}
```

**Step 2: Run tests + clippy**

Run: `cargo test && cargo clippy`
Expected: All pass, no warnings

**Step 3: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: launch key respects needs_new_session flag"
```

---

### Task 7: Update `r` key to work at session level

**Files:**
- Modify: `src/tui/app.rs:617-654` (review key handler)
- Modify: `src/tui/ui.rs:218` (hints)

**Step 1: Update `r` key to operate on sessions**

The `r` key should work when focused on Sessions panel. When a session has `ClaudeStatus::Done`, pressing `r` tears it down (marking it as reviewed):

```rust
(KeyCode::Char('r'), _) => {
    match self.focus {
        Focus::Sessions => {
            if let Some(session) = self.selected_session()
                && session.claude_status == ClaudeStatus::Done
            {
                let sid = session.id.clone();
                match crate::session::teardown_session(&self.store, &sid) {
                    Ok(()) => {
                        self.show_toast("Session reviewed and closed", ToastStyle::Success);
                    }
                    Err(e) => {
                        self.show_toast(
                            format!("Teardown failed: {e}"),
                            ToastStyle::Error,
                        );
                    }
                }
                self.refresh_data()?;
            }
        }
        Focus::Tasks => {
            // Keep ability to manually mark in_progress tasks as done
            if let Some(task) = self.visible_tasks().get(self.task_index).copied()
                && task.status == crate::store::TaskStatus::InProgress
            {
                self.store
                    .update_task_status(&task.id, crate::store::TaskStatus::Done)?;
                self.show_toast("Task marked as done", ToastStyle::Success);
                self.refresh_data()?;
            }
        }
        _ => {}
    }
}
```

**Step 2: Update hints**

In `src/tui/ui.rs:216` (Sessions hints), add `r:review`:

```rust
Focus::Sessions => " Enter:goto  r:review  d:delete  s:new  j/k:nav  ?:help",
```

In `src/tui/ui.rs:218` (Tasks hints), change `r:review` to `r:done`:

```rust
Focus::Tasks => {
    " n:new  e:edit  l:launch  r:done  o:PR  d:del  /:filter  J/K:reorder  ?:help"
}
```

**Step 3: Update tests**

Update `review_task_marks_done` test (now only works on `InProgress` tasks).
Delete `review_only_works_on_in_review_tasks` if not already removed in Task 1.

**Step 4: Run tests + clippy**

Run: `cargo test && cargo clippy`
Expected: All pass, no warnings

**Step 5: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat: r key reviews sessions (teardown) and marks tasks done"
```

---

### Task 8: Update config instructions

**Files:**
- Modify: `src/config/mod.rs:177-201` (CLAUDE.md merge — task completion instructions)

**Step 1: Update the task completion instructions**

The merged CLAUDE.md tells Claude to call `claustre_task_done`. The instructions currently mention `in_review` — update to reflect the new `done` state. This is a text-only change.

In `src/config/mod.rs`, around line 178-201, the text should not reference `in_review`. The `claustre_task_done` tool still exists but now marks tasks as `done` directly. No behavioral change needed in the instructions — just verify the wording is correct and doesn't mention `in_review`.

Check line 200: `"- Do NOT call with \`state: \"done\"\` — use \`claustre_task_done\` instead when finished\n\n"` — this is still correct.

**Step 2: Run tests + clippy**

Run: `cargo test && cargo clippy`
Expected: All pass

**Step 3: Commit (if any changes)**

```bash
git add src/config/mod.rs
git commit -m "docs: update CLAUDE.md instructions to reflect session-level review"
```

---

### Task 9: Update snapshot tests and final verification

**Files:**
- Modify: `src/tui/app.rs` (snapshot tests)

**Step 1: Update snapshot tests**

- `snapshot_task_form` (line ~2857): should now also assert `output.contains("Session")`
- `snapshot_new_session_panel` — no change needed
- Any test asserting `in_review` in rendered output (line ~2962): update to expect `done`

**Step 2: Run full test suite**

Run: `cargo test`
Expected: All pass

**Step 3: Run clippy**

Run: `cargo clippy`
Expected: No warnings

**Step 4: Run format check**

Run: `cargo fmt --check`
Expected: No formatting issues

**Step 5: Commit**

```bash
git add src/tui/app.rs
git commit -m "test: update snapshot tests for session field and done status"
```
