# cmux-Inspired Improvements Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add five improvements inspired by cmux: post-worktree setup hook, branch cleanup on teardown, DB-filesystem reconciliation on startup, merge action in TUI, and idempotent session creation.

**Architecture:** All five features are independent changes touching `session/mod.rs`, `store/queries.rs`, `tui/app.rs`, and `tui/ui.rs`. No new modules needed. The setup hook runs a `.claustre/setup` script after worktree creation. Branch cleanup adds `git branch -d` to teardown. Reconciliation queries DB sessions on startup and closes any whose worktree directory no longer exists. Merge adds a new TUI action (keybind `m`) that runs `git merge` or `git merge --squash`. Idempotent creation detects existing worktrees and reuses them.

**Tech Stack:** Rust, std::process::Command (git), ratatui TUI, rusqlite

---

### Task 1: Post-Worktree Setup Hook

Run `.claustre/setup` (or fallback to repo root's `.claustre/setup`) after worktree creation. This lets users install deps, symlink secrets, run codegen per-worktree.

**Files:**
- Modify: `src/session/mod.rs` — `create_session()` and new `run_setup_hook()` helper

**Step 1: Write the setup hook runner**

In `src/session/mod.rs`, add a helper function after `write_merged_config`:

```rust
/// Run the project's `.claustre/setup` hook in the worktree if it exists.
/// Checks worktree first, then falls back to the repo root's copy.
fn run_setup_hook(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let worktree_hook = worktree_path.join(".claustre").join("setup");
    let repo_hook = repo_path.join(".claustre").join("setup");

    let hook_path = if worktree_hook.exists() {
        Some(worktree_hook)
    } else if repo_hook.exists() {
        Some(repo_hook)
    } else {
        None
    };

    if let Some(hook) = hook_path {
        tracing::info!("Running setup hook: {}", hook.display());
        let status = Command::new("bash")
            .arg(&hook)
            .current_dir(worktree_path)
            .status()
            .with_context(|| format!("failed to run setup hook: {}", hook.display()))?;

        if !status.success() {
            tracing::warn!("Setup hook exited with status: {}", status);
        }
    }

    Ok(())
}
```

**Step 2: Wire the hook into `create_session()`**

In `create_session()`, add after step 2 (write_merged_config) and before step 3 (create_zellij_tab):

```rust
    // 2b. Run setup hook (best-effort — don't fail session creation)
    if let Err(e) = run_setup_hook(repo_path, &worktree_path) {
        tracing::warn!("Setup hook failed: {e}");
    }
```

**Step 3: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

**Step 4: Commit**

```bash
git add src/session/mod.rs
git commit -m "feat: run .claustre/setup hook after worktree creation"
```

---

### Task 2: Branch Cleanup on Teardown

After removing a worktree, delete the branch with `git branch -d` (safe delete — only works if merged). This prevents stale branch accumulation.

**Files:**
- Modify: `src/session/mod.rs` — `teardown_session()` and new `delete_branch()` helper

**Step 1: Add the branch delete helper**

In `src/session/mod.rs`, add after `remove_worktree`:

```rust
/// Attempt to delete a branch using safe delete (`git branch -d`).
/// This only succeeds if the branch is fully merged, which is fine —
/// unmerged branches should be kept for manual recovery.
fn delete_branch(repo_path: &Path, branch_name: &str) -> Result<()> {
    let repo_str = repo_path
        .to_str()
        .context("repo path contains invalid UTF-8")?;
    let output = Command::new("git")
        .args(["-C", repo_str, "branch", "-d", branch_name])
        .output()
        .context("failed to run git branch -d")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        tracing::info!("Branch '{branch_name}' not deleted (likely unmerged): {stderr}");
    }

    Ok(())
}
```

**Step 2: Wire into `teardown_session()`**

In `teardown_session()`, add after the worktree removal line `let _ = remove_worktree(...)`:

```rust
    // Delete branch (safe — only if merged)
    let _ = delete_branch(repo_path, &session.branch_name);
```

**Step 3: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

**Step 4: Commit**

```bash
git add src/session/mod.rs
git commit -m "feat: delete merged branches on session teardown"
```

---

### Task 3: DB-Filesystem Reconciliation on Startup

On TUI startup, check all "active" sessions in the DB. If a session's worktree directory no longer exists on disk, close it in the DB. This catches orphaned state from manual cleanup or crashes.

**Files:**
- Modify: `src/store/queries.rs` — new `list_all_active_sessions()` query
- Modify: `src/tui/app.rs` — call reconciliation in `App::new()`

**Step 1: Write a failing test for the new query**

In `src/store/queries.rs`, add at the end of the `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn test_list_all_active_sessions() {
        let store = Store::open_in_memory().unwrap();
        let p1 = store.create_project("p1", "/tmp/p1").unwrap();
        let p2 = store.create_project("p2", "/tmp/p2").unwrap();

        let s1 = store
            .create_session(&p1.id, "b1", "/tmp/wt1", "tab1")
            .unwrap();
        let s2 = store
            .create_session(&p2.id, "b2", "/tmp/wt2", "tab2")
            .unwrap();
        store.close_session(&s1.id).unwrap();

        let active = store.list_all_active_sessions().unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, s2.id);
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_list_all_active_sessions`
Expected: FAIL — method `list_all_active_sessions` does not exist

**Step 3: Implement the query**

In `src/store/queries.rs`, in the Sessions section (after `list_active_sessions_for_project`):

```rust
    pub fn list_all_active_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, branch_name, worktree_path, zellij_tab_name,
                    claude_status, status_message, last_activity_at,
                    files_changed, lines_added, lines_removed,
                    created_at, closed_at
             FROM sessions
             WHERE closed_at IS NULL
             ORDER BY created_at",
        )?;
        let sessions = stmt
            .query_map([], Self::row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }
```

**Step 4: Run test to verify it passes**

Run: `cargo test test_list_all_active_sessions`
Expected: PASS

**Step 5: Add reconciliation to `App::new()`**

In `src/tui/app.rs`, at the top of `App::new()` (right after `let projects = store.list_projects()?;`), add:

```rust
        // Reconcile DB with filesystem: close sessions whose worktrees no longer exist
        reconcile_sessions(&store);
```

And add the free function near the bottom of the file (next to `build_project_summaries`):

```rust
/// Close any active sessions whose worktree directory no longer exists on disk.
/// This catches orphaned state from manual cleanup, crashes, or external worktree removal.
fn reconcile_sessions(store: &Store) {
    let Ok(active_sessions) = store.list_all_active_sessions() else {
        return;
    };
    for session in &active_sessions {
        if !std::path::Path::new(&session.worktree_path).exists() {
            tracing::info!(
                "Reconciling orphaned session '{}' (worktree gone: {})",
                session.id,
                session.worktree_path
            );
            let _ = store.close_session(&session.id);
        }
    }
}
```

**Step 6: Build and run all tests**

Run: `cargo build && cargo test`
Expected: compiles cleanly, all tests pass

**Step 7: Commit**

```bash
git add src/store/queries.rs src/tui/app.rs
git commit -m "feat: reconcile DB sessions with filesystem on startup"
```

---

### Task 4: Merge Action in TUI

Add a `m` keybinding in the Sessions panel that merges the selected session's branch into the project's main branch. Show a confirmation overlay with regular merge vs squash merge options.

**Files:**
- Modify: `src/tui/app.rs` — new `InputMode::ConfirmMerge`, merge state fields, key handlers
- Modify: `src/tui/ui.rs` — draw the merge confirmation overlay
- Modify: `src/session/mod.rs` — new `merge_session()` function

**Step 1: Add `merge_session()` to `session/mod.rs`**

Add at the end of the public functions (after `feed_prompt_to_session`):

```rust
/// Merge a session's branch into the target branch (usually main/master).
/// If `squash` is true, uses `--squash` (stages changes without committing).
/// Returns the target branch name on success.
pub fn merge_session_branch(
    repo_path: &Path,
    branch_name: &str,
    squash: bool,
) -> Result<String> {
    let repo_str = repo_path
        .to_str()
        .context("repo path contains invalid UTF-8")?;

    // Detect the default branch (main or master)
    let output = Command::new("git")
        .args(["-C", repo_str, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("failed to detect current branch")?;
    let target_branch = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if target_branch == branch_name {
        bail!("Cannot merge branch '{branch_name}' into itself");
    }

    // Check for uncommitted changes in the worktree
    let diff_check = Command::new("git")
        .args(["-C", repo_str, "diff", "--quiet"])
        .status()
        .context("failed to check for uncommitted changes")?;
    if !diff_check.success() {
        bail!("Repository has uncommitted changes — commit or stash first");
    }

    // Perform the merge
    let mut merge_args = vec!["-C", repo_str, "merge"];
    if squash {
        merge_args.push("--squash");
    }
    merge_args.push(branch_name);

    let output = Command::new("git")
        .args(&merge_args)
        .output()
        .context("failed to run git merge")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Merge failed: {stderr}");
    }

    Ok(target_branch)
}
```

**Step 2: Add merge state to TUI**

In `src/tui/app.rs`, add to the `InputMode` enum:

```rust
    ConfirmMerge,
```

Add fields to the `App` struct (after the confirm delete fields):

```rust
    // Merge state
    pub merge_session_id: String,
    pub merge_branch_name: String,
    pub merge_squash: bool,
```

Initialize them in `App::new()`:

```rust
            merge_session_id: String::new(),
            merge_branch_name: String::new(),
            merge_squash: false,
```

**Step 3: Add the `m` keybinding**

In `handle_normal_key`, add a new arm (after the `o` / Open PR keybinding):

```rust
            // Merge session branch
            (KeyCode::Char('m'), _) => {
                if self.focus == Focus::Sessions
                    && let Some(session) = self.selected_session()
                {
                    self.merge_session_id = session.id.clone();
                    self.merge_branch_name = session.branch_name.clone();
                    self.merge_squash = false;
                    self.input_mode = InputMode::ConfirmMerge;
                }
            }
```

**Step 4: Add the merge confirmation key handler**

In `src/tui/app.rs`, add a new method to `impl App`:

```rust
    fn handle_confirm_merge_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                self.merge_squash = !self.merge_squash;
            }
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                let session = self.store.get_session(&self.merge_session_id)?;
                let project = self.store.get_project(&session.project_id)?;
                let repo_path = std::path::Path::new(&project.repo_path);

                match crate::session::merge_session_branch(
                    repo_path,
                    &self.merge_branch_name,
                    self.merge_squash,
                ) {
                    Ok(target) => {
                        let kind = if self.merge_squash { "Squash merged" } else { "Merged" };
                        self.show_toast(
                            format!("{kind} '{}' into '{target}'", self.merge_branch_name),
                            ToastStyle::Success,
                        );
                        if self.merge_squash {
                            self.show_toast(
                                format!("{kind} into '{target}' — review and commit"),
                                ToastStyle::Success,
                            );
                        }
                    }
                    Err(e) => {
                        self.show_toast(format!("Merge failed: {e}"), ToastStyle::Error);
                    }
                }
                self.refresh_data()?;
            }
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }
```

**Step 5: Wire the handler into the event loop**

In `app.rs`, in the `run()` method's match on `self.input_mode`, add:

```rust
                    InputMode::ConfirmMerge => self.handle_confirm_merge_key(key.code)?,
```

**Step 6: Draw the merge confirmation overlay**

In `src/tui/ui.rs`, in the `draw()` function's match on `app.input_mode`, add a case:

```rust
        InputMode::ConfirmMerge => draw_merge_confirm(frame, app),
```

Add the drawing function:

```rust
fn draw_merge_confirm(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 50u16.min(area.width.saturating_sub(4));
    let height = 7;
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .title(" Merge Branch ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));

    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    let lines = vec![
        Line::from(vec![
            Span::raw("Branch: "),
            Span::styled(
                &app.merge_branch_name,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw("Strategy: "),
            if app.merge_squash {
                Span::raw("  merge  ")
            } else {
                Span::styled(
                    "[ merge ]",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )
            },
            Span::raw("  "),
            if app.merge_squash {
                Span::styled(
                    "[ squash ]",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  squash  ")
            },
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            "Enter: confirm  |  Tab: toggle  |  Esc: cancel",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
}
```

**Step 7: Add `m` to the help overlay**

In `src/tui/ui.rs`, find the `draw_help_overlay` function and add to the Sessions keybindings section:

```
m — Merge session branch
```

**Step 8: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

**Step 9: Commit**

```bash
git add src/session/mod.rs src/tui/app.rs src/tui/ui.rs
git commit -m "feat: add merge action (m) for session branches in TUI"
```

---

### Task 5: Idempotent Session Creation

If `create_session` is called and the worktree already exists on disk (e.g., from a previous crashed session), reuse it instead of failing. If a DB session also exists for that worktree, resume it.

**Files:**
- Modify: `src/session/mod.rs` — update `create_session()` to handle existing worktrees
- Modify: `src/store/queries.rs` — new `find_active_session_by_worktree()` query

**Step 1: Write a failing test for the query**

In `src/store/queries.rs` tests:

```rust
    #[test]
    fn test_find_active_session_by_worktree() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "branch", "/tmp/wt", "tab")
            .unwrap();

        // Should find the active session
        let found = store
            .find_active_session_by_worktree("/tmp/wt")
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, session.id);

        // After closing, should not find it
        store.close_session(&session.id).unwrap();
        let found = store
            .find_active_session_by_worktree("/tmp/wt")
            .unwrap();
        assert!(found.is_none());
    }
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_find_active_session_by_worktree`
Expected: FAIL — method does not exist

**Step 3: Implement the query**

In `src/store/queries.rs`, in the Sessions section:

```rust
    pub fn find_active_session_by_worktree(&self, worktree_path: &str) -> Result<Option<Session>> {
        let result = self.conn.query_row(
            "SELECT id, project_id, branch_name, worktree_path, zellij_tab_name,
                    claude_status, status_message, last_activity_at,
                    files_changed, lines_added, lines_removed,
                    created_at, closed_at
             FROM sessions
             WHERE worktree_path = ?1 AND closed_at IS NULL
             LIMIT 1",
            rusqlite::params![worktree_path],
            Self::row_to_session,
        );
        match result {
            Ok(session) => Ok(Some(session)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
```

**Step 4: Run test to verify it passes**

Run: `cargo test test_find_active_session_by_worktree`
Expected: PASS

**Step 5: Update `create_session()` for idempotency**

In `src/session/mod.rs`, modify `create_session()`. Replace the current step 1 (create worktree) block with logic that checks if the worktree already exists:

The key change is in `create_worktree()` — if the worktree path already exists and is a valid git worktree, skip creation:

```rust
fn create_worktree(repo_path: &Path, project_name: &str, branch_name: &str) -> Result<PathBuf> {
    let worktree_base = config::worktree_base_dir()?;
    let worktree_path = worktree_base.join(project_name).join(branch_name);

    // Idempotent: if worktree already exists, reuse it
    if worktree_path.exists() {
        tracing::info!("Worktree already exists, reusing: {}", worktree_path.display());
        return Ok(worktree_path);
    }

    // ... rest of existing create_worktree code unchanged ...
}
```

And in `create_session()`, before creating a new DB session, check if one already exists for this worktree:

After computing `worktree_str` and before step 4 (Create session in DB), add:

```rust
    // 3b. Idempotent: if a DB session already exists for this worktree, reuse it
    if let Some(existing) = store.find_active_session_by_worktree(worktree_str)? {
        tracing::info!("Reusing existing session {} for worktree", existing.id);

        // Still launch Claude with the task if provided
        if let Some(task) = task {
            store.assign_task_to_session(&task.id, &existing.id)?;
            store.update_task_status(&task.id, TaskStatus::InProgress)?;
            store.update_session_status(
                &existing.id,
                ClaudeStatus::Working,
                &format!("Starting: {}", task.title),
            )?;

            let prompt = if let Some(subtask) = store.next_pending_subtask(&task.id)? {
                store.update_subtask_status(&subtask.id, TaskStatus::InProgress)?;
                if task.mode == TaskMode::Autonomous {
                    format!("{}{AUTONOMOUS_SUFFIX}", subtask.description)
                } else {
                    subtask.description
                }
            } else if task.mode == TaskMode::Autonomous {
                format!("{}{AUTONOMOUS_SUFFIX}", task.description)
            } else {
                task.description.clone()
            };
            launch_claude_in_zellij(&existing.zellij_tab_name, &prompt, &existing.id)?;
        }

        return_to_claustre();
        return Ok(existing);
    }
```

**Step 6: Build and run all tests**

Run: `cargo build && cargo test`
Expected: compiles cleanly, all tests pass

**Step 7: Commit**

```bash
git add src/session/mod.rs src/store/queries.rs
git commit -m "feat: idempotent session creation — reuse existing worktrees"
```

---

### Task 6: Final Verification

**Step 1: Run full test suite**

Run: `cargo test`
Expected: all tests pass

**Step 2: Run clippy**

Run: `cargo clippy`
Expected: no warnings

**Step 3: Check formatting**

Run: `cargo fmt --check`
Expected: no formatting issues

**Step 4: Final commit (if any fixups needed)**

```bash
git add -A
git commit -m "chore: clippy and formatting fixes"
```
