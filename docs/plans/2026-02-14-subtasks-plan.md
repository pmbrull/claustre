# Subtasks Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add subtasks to tasks so multiple sequential work items can run in the same session/branch, with the task as the review unit.

**Architecture:** New `subtasks` table with FK to `tasks`. Subtasks are lightweight work items (title, description, status). When a task with subtasks is launched, subtasks are fed sequentially. `claustre_task_done` handles subtask completion + auto-feeding.

**Tech Stack:** Rust, rusqlite, ratatui, tokio (MCP server)

---

### Task 1: Add subtasks table and model

**Files:**
- Modify: `src/store/mod.rs` (migration v5)
- Modify: `src/store/models.rs` (Subtask struct, SubtaskStatus)
- Modify: `src/store/queries.rs` (subtask CRUD)

**Step 1: Write failing test**

In `src/store/queries.rs` tests:

```rust
#[test]
fn test_create_and_list_subtasks() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();
    let task = store
        .create_task(&project.id, "parent", "", TaskMode::Autonomous)
        .unwrap();

    let s1 = store.create_subtask(&task.id, "step 1", "do first thing").unwrap();
    let s2 = store.create_subtask(&task.id, "step 2", "do second thing").unwrap();

    assert_eq!(s1.title, "step 1");
    assert_eq!(s1.status, TaskStatus::Pending);

    let subtasks = store.list_subtasks_for_task(&task.id).unwrap();
    assert_eq!(subtasks.len(), 2);
    assert_eq!(subtasks[0].title, "step 1");
    assert_eq!(subtasks[1].title, "step 2");
}
```

**Step 2: Add migration v5**

In `src/store/mod.rs`, append to MIGRATIONS:

```rust
Migration {
    version: 5,
    sql: "
        CREATE TABLE subtasks (
            id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
            title TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'pending',
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            started_at TEXT,
            completed_at TEXT
        );
    ",
},
```

**Step 3: Add Subtask model**

In `src/store/models.rs`, add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub sort_order: i64,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}
```

Reuse `TaskStatus` for subtasks (Pending, InProgress, InReview, Done, Error — even though subtasks skip InReview, parsing still works).

**Step 4: Add subtask CRUD to queries.rs**

```rust
// ── Subtasks ──

pub fn create_subtask(&self, task_id: &str, title: &str, description: &str) -> Result<Subtask> {
    let id = Uuid::new_v4().to_string();
    let max_order: i64 = self.conn.query_row(
        "SELECT COALESCE(MAX(sort_order), 0) FROM subtasks WHERE task_id = ?1",
        params![task_id],
        |row| row.get(0),
    )?;
    self.conn.execute(
        "INSERT INTO subtasks (id, task_id, title, description, sort_order) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, task_id, title, description, max_order + 1],
    )?;
    self.get_subtask(&id)
}

pub fn get_subtask(&self, id: &str) -> Result<Subtask> {
    let subtask = self.conn.query_row(
        "SELECT id, task_id, title, description, status, sort_order,
                created_at, started_at, completed_at
         FROM subtasks WHERE id = ?1",
        params![id],
        Self::row_to_subtask,
    )?;
    Ok(subtask)
}

pub fn list_subtasks_for_task(&self, task_id: &str) -> Result<Vec<Subtask>> {
    let mut stmt = self.conn.prepare(
        "SELECT id, task_id, title, description, status, sort_order,
                created_at, started_at, completed_at
         FROM subtasks WHERE task_id = ?1
         ORDER BY sort_order, created_at",
    )?;
    let subtasks = stmt
        .query_map(params![task_id], Self::row_to_subtask)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(subtasks)
}

pub fn update_subtask_status(&self, id: &str, status: TaskStatus) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    match status {
        TaskStatus::InProgress => {
            self.conn.execute(
                "UPDATE subtasks SET status = ?1, started_at = COALESCE(started_at, ?2) WHERE id = ?3",
                params![status.as_str(), now, id],
            )?;
        }
        TaskStatus::Done => {
            self.conn.execute(
                "UPDATE subtasks SET status = ?1, completed_at = ?2 WHERE id = ?3",
                params![status.as_str(), now, id],
            )?;
        }
        _ => {
            self.conn.execute(
                "UPDATE subtasks SET status = ?1 WHERE id = ?2",
                params![status.as_str(), id],
            )?;
        }
    }
    Ok(())
}

pub fn delete_subtask(&self, id: &str) -> Result<()> {
    self.conn.execute("DELETE FROM subtasks WHERE id = ?1", params![id])?;
    Ok(())
}

pub fn next_pending_subtask(&self, task_id: &str) -> Result<Option<Subtask>> {
    let result = self.conn.query_row(
        "SELECT id, task_id, title, description, status, sort_order,
                created_at, started_at, completed_at
         FROM subtasks
         WHERE task_id = ?1 AND status = 'pending'
         ORDER BY sort_order, created_at
         LIMIT 1",
        params![task_id],
        Self::row_to_subtask,
    );
    match result {
        Ok(subtask) => Ok(Some(subtask)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn subtask_count(&self, task_id: &str) -> Result<(i64, i64)> {
    let (total, done): (i64, i64) = self.conn.query_row(
        "SELECT COUNT(*), SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END) FROM subtasks WHERE task_id = ?1",
        params![task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((total, done))
}

fn row_to_subtask(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subtask> {
    let status_str: String = row.get(4)?;
    Ok(Subtask {
        id: row.get(0)?,
        task_id: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        status: status_str.parse().unwrap_or(TaskStatus::Pending),
        sort_order: row.get(5)?,
        created_at: row.get(6)?,
        started_at: row.get(7)?,
        completed_at: row.get(8)?,
    })
}
```

Export `Subtask` from `src/store/mod.rs` pub use line.

**Step 5: Run tests + clippy, commit**

Run: `cargo test && cargo clippy`
Commit: `feat: add subtasks table, model, and CRUD queries (migration v5)`

---

### Task 2: Update MCP `claustre_task_done` for subtask handling

**Files:**
- Modify: `src/mcp/mod.rs` (claustre_task_done handler)
- Modify: `src/session/mod.rs` (update feed logic for subtasks)

**Step 1: Write failing test**

In `src/mcp/mod.rs` tests, add a test for subtask completion flow:

```rust
#[tokio::test]
async fn claustre_task_done_with_subtasks_feeds_next() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();
    let session = store.create_session(&project.id, "b", "/tmp/wt", "tab").unwrap();
    let task = store.create_task(&project.id, "parent", "", TaskMode::Autonomous).unwrap();
    store.assign_task_to_session(&task.id, &session.id).unwrap();
    store.update_task_status(&task.id, TaskStatus::InProgress).unwrap();

    let s1 = store.create_subtask(&task.id, "step 1", "first").unwrap();
    let s2 = store.create_subtask(&task.id, "step 2", "second").unwrap();
    store.update_subtask_status(&s1.id, TaskStatus::InProgress).unwrap();

    let shared: SharedStore = Arc::new(Mutex::new(store));

    let req = make_tool_call(1, "claustre_task_done", &serde_json::json!({
        "session_id": session.id,
        "summary": "Step 1 done"
    }));

    let resp = handle_request(&req, &shared, None).await.unwrap();
    assert!(resp.result.is_some());

    let store = shared.lock().await;
    // Subtask 1 should be done
    let st1 = store.get_subtask(&s1.id).unwrap();
    assert_eq!(st1.status, TaskStatus::Done);

    // Subtask 2 should be in_progress (fed)
    let st2 = store.get_subtask(&s2.id).unwrap();
    assert_eq!(st2.status, TaskStatus::InProgress);

    // Parent task should still be in_progress
    let t = store.get_task(&task.id).unwrap();
    assert_eq!(t.status, TaskStatus::InProgress);
}
```

And a test for when the last subtask completes:

```rust
#[tokio::test]
async fn claustre_task_done_last_subtask_marks_task_in_review() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();
    let session = store.create_session(&project.id, "b", "/tmp/wt", "tab").unwrap();
    let task = store.create_task(&project.id, "parent", "", TaskMode::Autonomous).unwrap();
    store.assign_task_to_session(&task.id, &session.id).unwrap();
    store.update_task_status(&task.id, TaskStatus::InProgress).unwrap();

    let s1 = store.create_subtask(&task.id, "only step", "do it").unwrap();
    store.update_subtask_status(&s1.id, TaskStatus::InProgress).unwrap();

    let shared: SharedStore = Arc::new(Mutex::new(store));

    let req = make_tool_call(1, "claustre_task_done", &serde_json::json!({
        "session_id": session.id,
        "summary": "All done",
        "pr_url": "https://github.com/org/repo/pull/1"
    }));

    let resp = handle_request(&req, &shared, None).await.unwrap();
    assert!(resp.result.is_some());

    let store = shared.lock().await;
    let st1 = store.get_subtask(&s1.id).unwrap();
    assert_eq!(st1.status, TaskStatus::Done);

    // Parent task should be in_review since all subtasks done
    let t = store.get_task(&task.id).unwrap();
    assert_eq!(t.status, TaskStatus::InReview);
}
```

**Step 2: Update `claustre_task_done` handler**

In `src/mcp/mod.rs`, the `claustre_task_done` handler currently:
1. Finds the in-progress task for the session
2. Marks it `in_review`
3. Feeds next autonomous task

New behavior:
1. Find the in-progress task for the session
2. Check if it has subtasks
3. If it has subtasks:
   a. Find the in-progress subtask, mark it `done`
   b. If there's a next pending subtask, mark it `in_progress` and prepare to feed it
   c. If no more subtasks, mark task `in_review` and prepare to feed next task for session
4. If no subtasks: mark task `in_review` (current behavior)

The key change is in the DB operations section (under the lock). After finding the task, add:

```rust
let subtasks = store.list_subtasks_for_task(&task.id)?;
if !subtasks.is_empty() {
    // Mark current in-progress subtask as done
    for st in &subtasks {
        if st.status == TaskStatus::InProgress {
            store.update_subtask_status(&st.id, TaskStatus::Done)?;
            break;
        }
    }

    // Check for next pending subtask
    if let Some(next_st) = store.next_pending_subtask(&task.id)? {
        // Feed next subtask
        store.update_subtask_status(&next_st.id, TaskStatus::InProgress)?;
        // Prepare launch with subtask description as prompt
        // ... (prepare_next_subtask logic)
    } else {
        // All subtasks done — mark task in_review
        store.update_task_status(&task.id, TaskStatus::InReview)?;
        // ... existing feed_next_task logic
    }
} else {
    // No subtasks — existing behavior
    store.update_task_status(&task.id, TaskStatus::InReview)?;
}
```

You'll need to handle the "prepare to launch" pattern the same way the existing code does (prepare under lock, launch outside lock).

**Step 3: Update session launch to use subtasks**

In `src/session/mod.rs`, the `create_session` function launches Claude with `task.description` as the prompt. If the task has subtasks, it should instead launch with the first subtask's description.

Update the launch section in `create_session` (around line 86-104):

```rust
if let Some(task) = task {
    store.assign_task_to_session(&task.id, &session.id)?;
    store.update_task_status(&task.id, TaskStatus::InProgress)?;
    store.update_session_status(&session.id, ClaudeStatus::Working, &format!("Starting: {}", task.title))?;

    // If task has subtasks, launch the first one
    let prompt = if let Some(subtask) = store.next_pending_subtask(&task.id)? {
        store.update_subtask_status(&subtask.id, TaskStatus::InProgress)?;
        if task.mode == TaskMode::Autonomous {
            format!("{}{AUTONOMOUS_SUFFIX}", subtask.description)
        } else {
            subtask.description.clone()
        }
    } else if task.mode == TaskMode::Autonomous {
        format!("{}{AUTONOMOUS_SUFFIX}", task.description)
    } else {
        task.description.clone()
    };
    launch_claude_in_zellij(&tab_name, &prompt)?;
}
```

**Step 4: Run tests + clippy, commit**

Run: `cargo test && cargo clippy`
Commit: `feat: update claustre_task_done to handle subtask completion flow`

---

### Task 3: Add subtask management to TUI

**Files:**
- Modify: `src/tui/app.rs` (subtask state, input handlers)
- Modify: `src/tui/ui.rs` (render subtask indicators and panel)

**Step 1: Add subtask data to App state**

In `src/tui/app.rs`, add to App struct:

```rust
pub subtasks: Vec<Subtask>,
pub subtask_index: usize,
```

Add `InputMode::NewSubtask` variant.

In `refresh_data()`, when a task is selected, also load its subtasks:

```rust
// After loading tasks, load subtasks for selected task
if let Some(task) = self.visible_tasks().get(self.task_index) {
    self.subtasks = self.store.list_subtasks_for_task(&task.id)?;
} else {
    self.subtasks.clear();
}
```

**Step 2: Add subtask indicators in task list**

In `src/tui/ui.rs`, when rendering task list items, show subtask progress:

```rust
let (total, done) = app.store.subtask_count(&task.id).unwrap_or((0, 0));
if total > 0 {
    spans.push(Span::styled(
        format!(" ({done}/{total})"),
        Style::default().fg(Color::DarkGray),
    ));
}
```

Actually, for performance, pre-fetch subtask counts during `refresh_data` into a HashMap, similar to `project_summaries`. Store it as `pub subtask_counts: HashMap<String, (i64, i64)>` and populate during refresh.

**Step 3: Add `s` key handler for subtask management**

When focused on Tasks and pressing `s`, enter a subtask view/add mode:

```rust
(KeyCode::Char('s'), _) => {
    if self.focus == Focus::Tasks
        && let Some(task) = self.visible_tasks().get(self.task_index)
    {
        self.subtasks = self.store.list_subtasks_for_task(&task.id)?;
        self.input_mode = InputMode::NewSubtask;
        self.input_buffer.clear();
    }
}
```

**Step 4: Add subtask input handler**

Similar to the task form but simpler — just a description field:

```rust
fn handle_subtask_input_key(&mut self, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Enter => {
            if !self.input_buffer.is_empty() {
                if let Some(task) = self.visible_tasks().get(self.task_index) {
                    let task_id = task.id.clone();
                    let description = std::mem::take(&mut self.input_buffer);
                    let fallback = fallback_title(&description);
                    self.store.create_subtask(&task_id, &fallback, &description)?;
                    self.refresh_data()?;
                    self.show_toast("Subtask added", ToastStyle::Success);
                }
            }
            // Stay in subtask mode to add more
        }
        KeyCode::Esc => {
            self.input_buffer.clear();
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

**Step 5: Render subtask panel**

In `src/tui/ui.rs`, add a floating panel for subtask management (similar to new-session panel). Show existing subtasks as a list plus an input field for adding new ones.

**Step 6: Update task hints**

```rust
Focus::Tasks => " n:new  e:edit  s:subtasks  l:launch  r:review  o:PR  d:del  /:filter  J/K:reorder  ?:help"
```

**Step 7: Run tests + clippy, commit**

Run: `cargo test && cargo clippy`
Commit: `feat: add subtask management UI to TUI`

---

### Task 4: Final verification and cleanup

**Files:** All

**Step 1: Run full test suite**

Run: `cargo test`

**Step 2: Run clippy**

Run: `cargo clippy`

**Step 3: Run format check**

Run: `cargo fmt --check`

**Step 4: Update design doc**

Update `docs/plans/2026-02-14-subtasks-design.md` if any design decisions changed during implementation.

**Step 5: Commit**

```bash
git add docs/
git commit -m "docs: add subtask design and implementation plan"
```
