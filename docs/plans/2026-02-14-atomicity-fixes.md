# Atomicity Fixes Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Fix all multi-step operations that can leave the system in inconsistent state by adding SQLite transactions, rollback/cleanup logic, and proper error handling.

**Architecture:** Add a `Store::transaction()` helper that wraps `conn.unchecked_transaction()` (since all store methods take `&self`, not `&mut self`). Use it in all multi-statement DB operations. For mixed DB+side-effect operations, restructure to do side-effects first (reversible) and DB last, or add cleanup-on-failure logic.

**Tech Stack:** rusqlite 0.32 (`unchecked_transaction`), existing `Store`/`session` modules.

---

### Task 1: Add `Store::transaction()` helper

Expose rusqlite's `unchecked_transaction` through a clean helper on `Store` so all query methods can use it.

**Files:**
- Modify: `src/store/mod.rs:94-96`

**Step 1: Add the transaction helper method**

In `src/store/mod.rs`, add to the `impl Store` block (after `open_in_memory`):

```rust
/// Begin a SQLite transaction. Uses `unchecked_transaction` because
/// all `Store` methods take `&self`.
pub fn transaction(&self) -> Result<rusqlite::Transaction<'_>> {
    Ok(self.conn.unchecked_transaction()?)
}
```

**Step 2: Verify it compiles**

Run: `cargo build`
Expected: compiles clean

**Step 3: Commit**

```bash
git add src/store/mod.rs
git commit -m "feat(store): add transaction() helper for atomic DB operations"
```

---

### Task 2: Wrap `delete_project()` in a transaction

3 sequential DELETEs with no atomicity. If the middle fails, tasks are gone but project remains.

**Files:**
- Modify: `src/store/queries.rs:53-61`
- Test: existing `test_delete_project` in same file

**Step 1: Write a test for cascading delete atomicity**

Add test in `src/store/queries.rs` `mod tests`:

```rust
#[test]
fn test_delete_project_is_atomic() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("doomed", "/tmp/doomed").unwrap();
    let task = store
        .create_task(&project.id, "t1", "", TaskMode::Supervised)
        .unwrap();
    let session = store
        .create_session(&project.id, "b", "/tmp/wt", "tab")
        .unwrap();
    store.assign_task_to_session(&task.id, &session.id).unwrap();

    store.delete_project(&project.id).unwrap();

    // All three tables should be clean
    assert!(store.list_projects().unwrap().is_empty());
    assert!(store.list_tasks_for_project(&project.id).unwrap().is_empty());
    assert!(store.list_active_sessions_for_project(&project.id).unwrap().is_empty());
}
```

**Step 2: Run tests to verify they pass (baseline)**

Run: `cargo test test_delete_project`

**Step 3: Wrap delete_project in a transaction**

Replace `delete_project` body:

```rust
pub fn delete_project(&self, id: &str) -> Result<()> {
    let tx = self.transaction()?;
    tx.execute("DELETE FROM tasks WHERE project_id = ?1", params![id])?;
    tx.execute("DELETE FROM sessions WHERE project_id = ?1", params![id])?;
    tx.execute("DELETE FROM projects WHERE id = ?1", params![id])?;
    tx.commit()?;
    Ok(())
}
```

**Step 4: Run tests**

Run: `cargo test test_delete_project`
Expected: both tests pass

**Step 5: Commit**

```bash
git add src/store/queries.rs
git commit -m "fix(store): wrap delete_project in transaction for atomicity"
```

---

### Task 3: Wrap `swap_task_order()` in a transaction

Two UPDATEs — if second fails, two tasks share the same sort_order.

**Files:**
- Modify: `src/store/queries.rs:132-153`

**Step 1: Wrap in transaction**

Replace `swap_task_order` body:

```rust
#[expect(clippy::similar_names, reason = "a/b suffix is clearest for swap")]
pub fn swap_task_order(&self, task_a_id: &str, task_b_id: &str) -> Result<()> {
    let tx = self.transaction()?;
    let order_a: i64 = tx.query_row(
        "SELECT sort_order FROM tasks WHERE id = ?1",
        params![task_a_id],
        |row| row.get(0),
    )?;
    let order_b: i64 = tx.query_row(
        "SELECT sort_order FROM tasks WHERE id = ?1",
        params![task_b_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "UPDATE tasks SET sort_order = ?1 WHERE id = ?2",
        params![order_b, task_a_id],
    )?;
    tx.execute(
        "UPDATE tasks SET sort_order = ?1 WHERE id = ?2",
        params![order_a, task_b_id],
    )?;
    tx.commit()?;
    Ok(())
}
```

**Step 2: Run tests**

Run: `cargo test test_task_sort_order`
Expected: PASS

**Step 3: Commit**

```bash
git add src/store/queries.rs
git commit -m "fix(store): wrap swap_task_order in transaction"
```

---

### Task 4: Merge `update_task_status` into a single statement

Status and timestamp are two separate UPDATEs. Merge into one.

**Files:**
- Modify: `src/store/queries.rs:195-218`

**Step 1: Rewrite update_task_status as a single statement**

```rust
pub fn update_task_status(&self, id: &str, status: TaskStatus) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    match status {
        TaskStatus::InProgress => {
            self.conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2,
                 started_at = COALESCE(started_at, ?2)
                 WHERE id = ?3",
                params![status.as_str(), now, id],
            )?;
        }
        TaskStatus::Done => {
            self.conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2, completed_at = ?2
                 WHERE id = ?3",
                params![status.as_str(), now, id],
            )?;
        }
        _ => {
            self.conn.execute(
                "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status.as_str(), now, id],
            )?;
        }
    }
    Ok(())
}
```

**Step 2: Run tests**

Run: `cargo test test_task_lifecycle`
Expected: PASS (started_at and completed_at still populated correctly)

**Step 3: Commit**

```bash
git add src/store/queries.rs
git commit -m "fix(store): merge update_task_status into single statement per status"
```

---

### Task 5: Wrap migrations in transactions

Each migration's SQL + version bump must be atomic. A partially applied migration can brick the DB on restart.

**Files:**
- Modify: `src/store/mod.rs:116-175`

**Step 1: Wrap each migration step in a transaction**

In the `migrate()` method, wrap each migration application in a transaction. Replace the `// Apply unapplied migrations in order` loop (lines 158-172):

```rust
// Apply unapplied migrations in order
for migration in MIGRATIONS.iter().filter(|m| m.version > current_version) {
    let tx = self.transaction()?;
    tx.execute_batch(migration.sql)?;
    if current_version == 0 && migration.version == MIGRATIONS[0].version {
        tx.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            rusqlite::params![migration.version],
        )?;
    } else {
        tx.execute(
            "UPDATE schema_version SET version = ?1",
            rusqlite::params![migration.version],
        )?;
    }
    tx.commit()?;
}
```

Also wrap the legacy detection path similarly (lines 144-153):

```rust
if has_projects {
    let tx = self.transaction()?;
    tx.execute("INSERT INTO schema_version (version) VALUES (1)", [])?;
    for migration in MIGRATIONS.iter().filter(|m| m.version > 1) {
        tx.execute_batch(migration.sql)?;
        tx.execute(
            "UPDATE schema_version SET version = ?1",
            rusqlite::params![migration.version],
        )?;
    }
    tx.commit()?;
    return Ok(());
}
```

**Step 2: Run all tests**

Run: `cargo test`
Expected: all pass (in-memory DBs run migrate on open)

**Step 3: Commit**

```bash
git add src/store/mod.rs
git commit -m "fix(store): wrap each migration in a transaction to prevent partial schema updates"
```

---

### Task 6: Fix `teardown_session` — don't abort on stats failure

`update_session_git_stats()` uses `?` — if this cosmetic DB write fails, the whole teardown aborts before cleaning up the Zellij tab and worktree.

**Files:**
- Modify: `src/session/mod.rs:113-141`

**Step 1: Change git stats capture to best-effort**

Replace the git stats block (lines 118-126):

```rust
// Best-effort: capture final git stats (don't abort teardown on failure)
if let Ok(stats) = get_git_stats(Path::new(&session.worktree_path)) {
    let _ = store.update_session_git_stats(
        session_id,
        stats.files_changed,
        stats.lines_added,
        stats.lines_removed,
    );
}
```

Changed `?` to `let _ =` so a DB error on stats doesn't prevent teardown.

**Step 2: Run existing tests**

Run: `cargo test`
Expected: PASS

**Step 3: Commit**

```bash
git add src/session/mod.rs
git commit -m "fix(session): don't abort teardown on git stats write failure"
```

---

### Task 7: Add rollback/cleanup to `create_session`

8 sequential steps with no rollback. If step 4 (DB insert) fails, orphaned worktree + Zellij tab remain. If step 8 (Claude launch) fails, task is stuck in `in_progress` forever.

**Files:**
- Modify: `src/session/mod.rs:52-110`

**Step 1: Restructure create_session with cleanup-on-failure**

The approach: wrap the whole thing so that on error, we clean up whatever was created. Use a helper closure pattern.

```rust
pub fn create_session(
    store: &Store,
    project_id: &str,
    branch_name: &str,
    task: Option<&Task>,
) -> Result<Session> {
    require_zellij()?;
    let project = store.get_project(project_id)?;
    let repo_path = Path::new(&project.repo_path);

    // 1. Create the worktree
    let worktree_path = create_worktree(repo_path, &project.name, branch_name)?;

    // 2. Merge config into worktree
    if let Err(e) = write_merged_config(repo_path, &worktree_path) {
        let _ = remove_worktree(repo_path, &worktree_path);
        return Err(e.context("write_merged_config failed, worktree cleaned up"));
    }

    // 3. Create Zellij tab
    let tab_name = format!("{}:{}", project.name, branch_name);
    if let Err(e) = create_zellij_tab(&tab_name, &worktree_path) {
        let _ = remove_worktree(repo_path, &worktree_path);
        return Err(e.context("create_zellij_tab failed, worktree cleaned up"));
    }

    // 4. Create session in DB
    let worktree_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;
    let session = match store.create_session(project_id, branch_name, worktree_str, &tab_name) {
        Ok(s) => s,
        Err(e) => {
            let _ = close_zellij_tab(&tab_name);
            let _ = remove_worktree(repo_path, &worktree_path);
            return Err(e.context("DB create_session failed, tab + worktree cleaned up"));
        }
    };

    // 5. Write MCP config
    if let Err(e) = write_mcp_config(&worktree_path, &session.id) {
        let _ = close_zellij_tab(&tab_name);
        let _ = remove_worktree(repo_path, &worktree_path);
        let _ = store.close_session(&session.id);
        return Err(e.context("write_mcp_config failed, session cleaned up"));
    }

    // 6. Pre-trust the worktree
    pre_trust_worktree(&worktree_path);

    // 7. Launch Claude with the task prompt
    if let Some(task) = task {
        store.assign_task_to_session(&task.id, &session.id)?;
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

        if let Err(e) = launch_claude_in_zellij(&tab_name, &prompt) {
            // Claude failed to launch — revert task to pending so it can be retried
            let _ = store.update_task_status(&task.id, TaskStatus::Pending);
            let _ = store.update_session_status(
                &session.id,
                ClaudeStatus::Error,
                &format!("Launch failed: {e}"),
            );
            // Don't tear down the session — user may want to investigate or retry
            return Err(e.context("launch_claude_in_zellij failed, task reverted to pending"));
        }
    }

    // 8. Return focus to the Claustre TUI tab
    return_to_claustre();

    Ok(session)
}
```

**Step 2: Run all tests**

Run: `cargo test`
Expected: PASS

**Step 3: Commit**

```bash
git add src/session/mod.rs
git commit -m "fix(session): add rollback/cleanup to create_session on partial failure"
```

---

### Task 8: Fix `feed_next_task` — revert task on launch failure

If `launch_claude_in_zellij` fails after marking task `in_progress`, the task is stuck forever.

**Files:**
- Modify: `src/session/mod.rs:155-179`

**Step 1: Add error recovery**

```rust
pub fn feed_next_task(store: &Store, session_id: &str) -> Result<bool> {
    require_zellij()?;
    // Don't feed tasks if rate limited
    if let Ok(state) = store.get_rate_limit_state()
        && state.is_rate_limited
    {
        tracing::info!("Skipping feed_next_task: rate limited");
        return Ok(false);
    }

    if let Some(task) = store.next_pending_task_for_session(session_id)? {
        let session = store.get_session(session_id)?;
        store.update_task_status(&task.id, TaskStatus::InProgress)?;
        store.update_session_status(
            session_id,
            ClaudeStatus::Working,
            &format!("Starting: {}", task.title),
        )?;
        let prompt = format!("{}{AUTONOMOUS_SUFFIX}", task.description);

        if let Err(e) = launch_claude_in_zellij(&session.zellij_tab_name, &prompt) {
            // Revert task so it can be retried
            let _ = store.update_task_status(&task.id, TaskStatus::Pending);
            let _ = store.update_session_status(
                session_id,
                ClaudeStatus::Error,
                &format!("Feed failed: {e}"),
            );
            return Err(e.context("feed_next_task: launch failed, task reverted to pending"));
        }

        Ok(true)
    } else {
        Ok(false)
    }
}
```

**Step 2: Run tests**

Run: `cargo test`
Expected: PASS

**Step 3: Commit**

```bash
git add src/session/mod.rs
git commit -m "fix(session): revert task to pending if feed_next_task launch fails"
```

---

### Task 9: Teardown active sessions before deleting a project

Deleting a project nukes DB records but leaves worktrees and Zellij tabs alive.

**Files:**
- Modify: `src/tui/app.rs` — the `DeleteTarget::Project` handler (around line 1130)

**Step 1: Teardown active sessions before DB delete**

Replace the `DeleteTarget::Project` arm:

```rust
DeleteTarget::Project => {
    // Teardown all active sessions before deleting DB records
    if let Ok(sessions) =
        self.store.list_active_sessions_for_project(&self.confirm_entity_id)
    {
        for session in &sessions {
            let _ = crate::session::teardown_session(&self.store, &session.id);
        }
    }
    self.store.delete_project(&self.confirm_entity_id)?;
    self.project_index = 0;
    self.show_toast(
        format!("Project '{name}' deleted"),
        ToastStyle::Success,
    );
}
```

**Step 2: Run tests**

Run: `cargo test`
Expected: PASS

**Step 3: Commit**

```bash
git add src/tui/app.rs
git commit -m "fix(tui): teardown active sessions before deleting project"
```

---

### Task 10: Release MCP mutex before external commands in `claustre_task_done`

`feed_next_task()` calls `launch_claude_in_zellij()` while the MCP store mutex is held. If Zellij hangs, the entire MCP server is blocked.

**Files:**
- Modify: `src/mcp/mod.rs` — `claustre_task_done` handler (around line 615-666)

**Step 1: Restructure to release lock before feed_next_task**

The key change: do all DB reads/writes under the lock, drop the lock, then call `feed_next_task` with a fresh lock acquisition (which happens internally).

```rust
"claustre_task_done" => {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .context("missing session_id")?;
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .context("missing summary")?;
    let pr_url = args.get("pr_url").and_then(|v| v.as_str());

    // Scope the lock to DB operations only
    let task_title = {
        let store = store.lock().await;

        let mut task_title = String::new();
        let tasks = {
            let session = store.get_session(session_id)?;
            store.list_tasks_for_project(&session.project_id)?
        };

        for task in &tasks {
            if task.session_id.as_deref() == Some(session_id)
                && task.status == TaskStatus::InProgress
            {
                task_title.clone_from(&task.title);
                store.update_task_status(&task.id, TaskStatus::InReview)?;
                if let Some(url) = pr_url {
                    store.update_task_pr_url(&task.id, url)?;
                }
                break;
            }
        }

        store.update_session_status(session_id, ClaudeStatus::Done, summary)?;

        // Fire notification
        if let Some(notify) = notify {
            notify(&task_title);
        }

        task_title
    }; // Lock released here

    // Auto-queue outside the lock — feed_next_task acquires its own store ref
    // We need to re-lock briefly for the DB operations inside feed_next_task
    let auto_fed = {
        let store = store.lock().await;
        crate::session::feed_next_task(&store, session_id).unwrap_or(false)
    };

    if auto_fed {
        Ok(format!(
            "Task marked as in_review. Next autonomous task queued. Summary: {summary}"
        ))
    } else {
        Ok(format!(
            "Task marked as in_review. No more queued tasks. Summary: {summary}"
        ))
    }
}
```

Wait — `feed_next_task` takes `&Store` and internally calls `launch_claude_in_zellij`. The problem is that `feed_next_task` does both DB writes AND external commands in one function. To truly fix this, we should split `feed_next_task` into "prepare" (DB, under lock) and "execute" (Zellij, no lock).

Actually, looking more carefully: the MCP `SharedStore` is `Arc<Mutex<Store>>`, but `feed_next_task` takes `&Store` (the inner value). So the lock MUST be held for the entire `feed_next_task` call since we pass the `&Store` ref. The real fix is to restructure `feed_next_task` so it returns the data needed for launch, and the caller does the launch outside the lock.

**Revised approach: split feed_next_task into prepare + launch**

In `src/session/mod.rs`, add a struct and split function:

```rust
/// Data needed to launch a Claude session, returned by `prepare_next_task`.
pub struct PreparedTask {
    pub tab_name: String,
    pub prompt: String,
    pub task_id: String,
    pub session_id: String,
}

/// Prepare the next autonomous task for launch (DB only, no external commands).
/// Returns `None` if rate limited or no pending tasks.
pub fn prepare_next_task(store: &Store, session_id: &str) -> Result<Option<PreparedTask>> {
    // Don't feed tasks if rate limited
    if let Ok(state) = store.get_rate_limit_state()
        && state.is_rate_limited
    {
        tracing::info!("Skipping feed_next_task: rate limited");
        return Ok(None);
    }

    if let Some(task) = store.next_pending_task_for_session(session_id)? {
        let session = store.get_session(session_id)?;
        store.update_task_status(&task.id, TaskStatus::InProgress)?;
        store.update_session_status(
            session_id,
            ClaudeStatus::Working,
            &format!("Starting: {}", task.title),
        )?;
        let prompt = format!("{}{AUTONOMOUS_SUFFIX}", task.description);
        Ok(Some(PreparedTask {
            tab_name: session.zellij_tab_name,
            prompt,
            task_id: task.id,
            session_id: session_id.to_string(),
        }))
    } else {
        Ok(None)
    }
}

/// Execute the launch for a prepared task (external commands only, no DB).
pub fn launch_prepared_task(prepared: &PreparedTask) -> Result<()> {
    require_zellij()?;
    launch_claude_in_zellij(&prepared.tab_name, &prepared.prompt)
}

/// Revert a prepared task back to pending (called if launch fails).
pub fn revert_prepared_task(store: &Store, prepared: &PreparedTask) {
    let _ = store.update_task_status(&prepared.task_id, TaskStatus::Pending);
    let _ = store.update_session_status(
        &prepared.session_id,
        ClaudeStatus::Error,
        "Launch failed, task reverted to pending",
    );
}
```

Then update `feed_next_task` to use these:

```rust
pub fn feed_next_task(store: &Store, session_id: &str) -> Result<bool> {
    require_zellij()?;
    if let Some(prepared) = prepare_next_task(store, session_id)? {
        if let Err(e) = launch_prepared_task(&prepared) {
            revert_prepared_task(store, &prepared);
            return Err(e.context("feed_next_task: launch failed, task reverted to pending"));
        }
        Ok(true)
    } else {
        Ok(false)
    }
}
```

Then in MCP handler, use prepare/launch split:

```rust
"claustre_task_done" => {
    let session_id = args
        .get("session_id")
        .and_then(|v| v.as_str())
        .context("missing session_id")?;
    let summary = args
        .get("summary")
        .and_then(|v| v.as_str())
        .context("missing summary")?;
    let pr_url = args.get("pr_url").and_then(|v| v.as_str());

    // All DB operations under the lock
    let (task_title, prepared) = {
        let store = store.lock().await;

        let mut task_title = String::new();
        let tasks = {
            let session = store.get_session(session_id)?;
            store.list_tasks_for_project(&session.project_id)?
        };

        for task in &tasks {
            if task.session_id.as_deref() == Some(session_id)
                && task.status == TaskStatus::InProgress
            {
                task_title.clone_from(&task.title);
                store.update_task_status(&task.id, TaskStatus::InReview)?;
                if let Some(url) = pr_url {
                    store.update_task_pr_url(&task.id, url)?;
                }
                break;
            }
        }

        store.update_session_status(session_id, ClaudeStatus::Done, summary)?;

        if let Some(notify) = notify {
            notify(&task_title);
        }

        let prepared = crate::session::prepare_next_task(&store, session_id)
            .unwrap_or(None);

        (task_title, prepared)
    }; // Lock released here

    // Launch outside the lock — Zellij commands won't block other MCP calls
    let auto_fed = if let Some(ref prepared) = prepared {
        match crate::session::launch_prepared_task(prepared) {
            Ok(()) => true,
            Err(e) => {
                tracing::error!("Auto-feed launch failed: {e}");
                let store = store.lock().await;
                crate::session::revert_prepared_task(&store, prepared);
                false
            }
        }
    } else {
        false
    };

    if auto_fed {
        Ok(format!(
            "Task marked as in_review. Next autonomous task queued. Summary: {summary}"
        ))
    } else {
        Ok(format!(
            "Task marked as in_review. No more queued tasks. Summary: {summary}"
        ))
    }
}
```

Also apply the same pattern to the `claustre_status` "done" fallback — it currently does NOT call `feed_next_task`, which stalls the autonomous pipeline. Add it:

In the `claustre_status` handler, after the `if claude_status == ClaudeStatus::Done` block processes the task transition, add auto-feed logic using the same prepare/launch pattern. Restructure the handler to scope the lock and do the launch outside.

**Step 2: Run tests**

Run: `cargo test`
Expected: PASS (MCP tests still work — `feed_next_task` is tested indirectly via `claustre_task_done`)

**Step 3: Run clippy**

Run: `cargo clippy`
Expected: no warnings

**Step 4: Commit**

```bash
git add src/session/mod.rs src/mcp/mod.rs
git commit -m "fix(mcp): release mutex before Zellij commands, add auto-feed to status-done fallback"
```

---

### Task 11: Final verification

**Step 1: Run the full test suite**

Run: `cargo test`
Expected: all 122+ tests pass

**Step 2: Run clippy**

Run: `cargo clippy`
Expected: no warnings

**Step 3: Run fmt check**

Run: `cargo fmt --check`
Expected: no output (already formatted)

**Step 4: Commit any remaining changes**

If any formatting or minor adjustments are needed.
