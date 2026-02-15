# Claude Task Progress Display — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Show Claude Code's internal task progress (todo items) in the Claustre TUI session detail panel.

**Architecture:** Stop hook reads Claude's task JSON files from `~/.claude/tasks/<session_id>/`, writes consolidated progress to `~/.claustre/tmp/<session_id>/progress.json`, and `session-update` reads that file into a new `claude_progress` TEXT column on the sessions table. TUI deserializes and renders inline.

**Tech Stack:** Rust (rusqlite, serde_json, ratatui), Bash (stop hook)

---

### Task 1: Add `ClaudeProgressItem` model and `claude_progress` field to Session

**Files:**
- Modify: `src/store/models.rs:187-202` (Session struct)
- Modify: `src/store/models.rs:1-4` (imports)

**Step 1: Add `ClaudeProgressItem` struct to models.rs**

Add after the `Subtask` struct (line 132):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeProgressItem {
    pub subject: String,
    pub status: String,
}
```

**Step 2: Add `claude_progress` field to Session struct**

Add after `closed_at` field (line 201):

```rust
pub claude_progress: Vec<ClaudeProgressItem>,
```

**Step 3: Run `cargo build` to see expected compile errors**

Run: `cargo build 2>&1 | head -30`
Expected: errors in `row_to_session` and anywhere Session is constructed — this is correct, we'll fix in the next task.

**Step 4: Commit**

```bash
git add src/store/models.rs
git commit -m "feat: add ClaudeProgressItem model and claude_progress field to Session"
```

---

### Task 2: Add migration v6 and update session queries

**Files:**
- Modify: `src/store/mod.rs:113` (add migration after v5)
- Modify: `src/store/mod.rs:8-9` (re-export `ClaudeProgressItem`)
- Modify: `src/store/queries.rs:349-366` (`row_to_session`)
- Modify: `src/store/queries.rs:320-331` (`get_session` SELECT)
- Modify: `src/store/queries.rs:333-347` (`list_active_sessions_for_project` SELECT)

**Step 1: Add migration v6 to `MIGRATIONS` array**

In `src/store/mod.rs`, add after the v5 migration (after line 113, before the closing `];`):

```rust
Migration {
    version: 6,
    sql: "
        ALTER TABLE sessions ADD COLUMN claude_progress TEXT NOT NULL DEFAULT '';
    ",
},
```

**Step 2: Re-export `ClaudeProgressItem` from `src/store/mod.rs`**

Update the `pub use models` line to include `ClaudeProgressItem`:

```rust
pub use models::{
    ClaudeProgressItem, ClaudeStatus, Project, RateLimitState, Session, Subtask, Task, TaskMode,
    TaskStatus,
};
```

**Step 3: Update `row_to_session` to parse `claude_progress`**

In `src/store/queries.rs`, update `row_to_session` (line 349). The column is at index 13 (after `closed_at` at index 12). Parse the JSON string:

```rust
fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let status_str: String = row.get(5)?;
    let progress_str: String = row.get(13)?;
    let claude_progress = if progress_str.is_empty() {
        vec![]
    } else {
        serde_json::from_str(&progress_str).unwrap_or_default()
    };
    Ok(Session {
        id: row.get(0)?,
        project_id: row.get(1)?,
        branch_name: row.get(2)?,
        worktree_path: row.get(3)?,
        zellij_tab_name: row.get(4)?,
        claude_status: status_str.parse().unwrap_or(ClaudeStatus::Idle),
        status_message: row.get(6)?,
        last_activity_at: row.get(7)?,
        files_changed: row.get(8)?,
        lines_added: row.get(9)?,
        lines_removed: row.get(10)?,
        created_at: row.get(11)?,
        closed_at: row.get(12)?,
        claude_progress,
    })
}
```

**Step 4: Update all SELECT statements that use `row_to_session` to include `claude_progress`**

In `get_session` (line 321-330), update the SELECT:

```sql
SELECT id, project_id, branch_name, worktree_path, zellij_tab_name,
       claude_status, status_message, last_activity_at,
       files_changed, lines_added, lines_removed,
       created_at, closed_at, claude_progress
FROM sessions WHERE id = ?1
```

In `list_active_sessions_for_project` (line 334-341), update the SELECT:

```sql
SELECT id, project_id, branch_name, worktree_path, zellij_tab_name,
       claude_status, status_message, last_activity_at,
       files_changed, lines_added, lines_removed,
       created_at, closed_at, claude_progress
FROM sessions
WHERE project_id = ?1 AND closed_at IS NULL
ORDER BY created_at
```

**Step 5: Run tests**

Run: `cargo test 2>&1`
Expected: all existing tests pass. The new column has a default value, so existing test data works.

**Step 6: Commit**

```bash
git add src/store/mod.rs src/store/queries.rs
git commit -m "feat: add migration v6 for claude_progress column and update session queries"
```

---

### Task 3: Add `update_session_progress` store method with test

**Files:**
- Modify: `src/store/queries.rs` (add method after `close_session`, line ~411)
- Modify: `src/store/queries.rs` (add test at end of tests module)

**Step 1: Write the test**

Add to the `#[cfg(test)] mod tests` block at the end of `src/store/queries.rs`:

```rust
#[test]
fn test_update_session_progress() {
    let store = Store::open_in_memory().unwrap();
    let project = store.create_project("proj", "/tmp/proj").unwrap();
    let session = store
        .create_session(&project.id, "branch", "/tmp/wt", "tab")
        .unwrap();

    // Initially empty
    assert!(session.claude_progress.is_empty());

    // Update with progress items
    let progress = vec![
        ClaudeProgressItem { subject: "Step 1".into(), status: "completed".into() },
        ClaudeProgressItem { subject: "Step 2".into(), status: "in_progress".into() },
        ClaudeProgressItem { subject: "Step 3".into(), status: "pending".into() },
    ];
    store.update_session_progress(&session.id, &progress).unwrap();

    let s = store.get_session(&session.id).unwrap();
    assert_eq!(s.claude_progress.len(), 3);
    assert_eq!(s.claude_progress[0].subject, "Step 1");
    assert_eq!(s.claude_progress[0].status, "completed");
    assert_eq!(s.claude_progress[1].status, "in_progress");
    assert_eq!(s.claude_progress[2].status, "pending");

    // Update again (replace)
    let progress2 = vec![
        ClaudeProgressItem { subject: "Step 1".into(), status: "completed".into() },
    ];
    store.update_session_progress(&session.id, &progress2).unwrap();
    let s = store.get_session(&session.id).unwrap();
    assert_eq!(s.claude_progress.len(), 1);
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test test_update_session_progress 2>&1`
Expected: compile error — `update_session_progress` not found.

**Step 3: Implement `update_session_progress`**

Add after `close_session` in `src/store/queries.rs`:

```rust
pub fn update_session_progress(
    &self,
    id: &str,
    progress: &[ClaudeProgressItem],
) -> Result<()> {
    let json = serde_json::to_string(progress)?;
    self.conn.execute(
        "UPDATE sessions SET claude_progress = ?1 WHERE id = ?2",
        params![json, id],
    )?;
    Ok(())
}
```

Add `use super::models::ClaudeProgressItem;` to the imports at the top of `queries.rs` (line 7).

**Step 4: Run test to verify it passes**

Run: `cargo test test_update_session_progress -v 2>&1`
Expected: PASS

**Step 5: Commit**

```bash
git add src/store/queries.rs
git commit -m "feat: add update_session_progress store method"
```

---

### Task 4: Add `progress_dir` config helper and tmp dir creation

**Files:**
- Modify: `src/config/mod.rs` (add `progress_dir` function after `worktree_base_dir`, line ~124)
- Modify: `src/config/mod.rs:127-132` (update `ensure_dirs`)

**Step 1: Add `progress_dir` function**

After `worktree_base_dir()` (line 124), add:

```rust
/// Returns the path to the tmp progress directory for a session
pub fn session_progress_dir(session_id: &str) -> Result<PathBuf> {
    Ok(base_dir()?.join("tmp").join(session_id))
}

/// Returns the path to the progress.json file for a session
pub fn session_progress_file(session_id: &str) -> Result<PathBuf> {
    Ok(session_progress_dir(session_id)?.join("progress.json"))
}
```

**Step 2: Update `ensure_dirs` to create tmp directory**

In `ensure_dirs()`, add after the worktrees line:

```rust
fs::create_dir_all(base_dir()?.join("tmp")).context("failed to create ~/.claustre/tmp/")?;
```

**Step 3: Run `cargo build`**

Run: `cargo build 2>&1`
Expected: clean compile

**Step 4: Commit**

```bash
git add src/config/mod.rs
git commit -m "feat: add session progress dir helpers to config"
```

---

### Task 5: Update `session-update` CLI to read progress file

**Files:**
- Modify: `src/main.rs:343-365` (SessionUpdate handler)

**Step 1: Update the `SessionUpdate` handler**

Replace the handler block (lines 343-365) with:

```rust
Commands::SessionUpdate { session_id, pr_url } => {
    let store = store::Store::open()?;
    store.migrate()?;

    // Always set session to idle
    store.update_session_status(&session_id, store::ClaudeStatus::Idle, "")?;

    // Read Claude's task progress from tmp file (if it exists)
    if let Ok(progress_path) = config::session_progress_file(&session_id)
        && progress_path.exists()
    {
        if let Ok(content) = fs::read_to_string(&progress_path) {
            if let Ok(items) = serde_json::from_str::<Vec<store::ClaudeProgressItem>>(&content) {
                let _ = store.update_session_progress(&session_id, &items);
            }
        }
    }

    // If a PR URL was provided, transition the in-progress task
    if let Some(ref url) = pr_url
        && let Some(task) = store.in_progress_task_for_session(&session_id)?
    {
        store.update_task_pr_url(&task.id, url)?;
        store.update_task_status(&task.id, store::TaskStatus::InReview)?;

        // Fire notification
        let cfg = config::load()?;
        if cfg.notifications.enabled {
            cfg.notifications.notify(&task.title);
        }
    }

    Ok(())
}
```

**Step 2: Run `cargo build`**

Run: `cargo build 2>&1`
Expected: clean compile

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: session-update reads progress.json from tmp dir"
```

---

### Task 6: Update stop hook to read Claude's tasks and write progress.json

**Files:**
- Modify: `src/session/mod.rs:271-318` (`write_stop_hook` function)

**Step 1: Update the stop hook script**

Replace the hook script in `write_stop_hook` (lines 276-288):

```rust
let hook_script = r#"#!/bin/bash
SESSION_ID=$(cat "$PWD/.claustre_session_id" 2>/dev/null)
[ -z "$SESSION_ID" ] && exit 0

# Read Claude's internal task progress and write to claustre tmp dir
TASK_DIR="$HOME/.claude/tasks/$SESSION_ID"
PROGRESS_DIR="$HOME/.claustre/tmp/$SESSION_ID"

if [ -d "$TASK_DIR" ]; then
    mkdir -p "$PROGRESS_DIR"
    python3 -c "
import json, glob, os
task_dir = os.path.expanduser('$TASK_DIR')
progress_dir = os.path.expanduser('$PROGRESS_DIR')
items = []
for f in sorted(glob.glob(os.path.join(task_dir, '[0-9]*.json'))):
    try:
        with open(f) as fh:
            d = json.load(fh)
            items.append({'subject': d.get('subject', ''), 'status': d.get('status', 'pending')})
    except (json.JSONDecodeError, IOError):
        pass
with open(os.path.join(progress_dir, 'progress.json'), 'w') as out:
    json.dump(items, out)
" 2>/dev/null
fi

# Check for open PR on current branch
PR_URL=$(gh pr view --json url --jq '.url' 2>/dev/null)

if [ -n "$PR_URL" ]; then
    claustre session-update --session-id "$SESSION_ID" --pr-url "$PR_URL"
else
    claustre session-update --session-id "$SESSION_ID"
fi
exit 0
"#;
```

**Step 2: Run `cargo build`**

Run: `cargo build 2>&1`
Expected: clean compile

**Step 3: Commit**

```bash
git add src/session/mod.rs
git commit -m "feat: stop hook reads Claude task files and writes progress.json"
```

---

### Task 7: Set `CLAUDE_CODE_TASK_LIST_ID` when launching sessions

**Files:**
- Modify: `src/session/mod.rs:355-380` (`launch_feed_next_in_zellij`)
- Modify: `src/session/mod.rs:382-410` (`launch_claude_in_zellij`)
- Modify: `src/main.rs:538-543` (`run_feed_next` — the `Command::new("claude")` call)

**Step 1: Update `launch_feed_next_in_zellij`**

Change the command string (line 373) to prefix with the env var:

```rust
let cmd = format!("CLAUDE_CODE_TASK_LIST_ID={session_id} claustre feed-next --session-id {session_id}\n");
```

**Step 2: Update `launch_claude_in_zellij`**

The function needs to accept session_id. Update the signature (line 382):

```rust
fn launch_claude_in_zellij(tab_name: &str, prompt: &str, session_id: &str) -> Result<()> {
```

Update the command string (line 406):

```rust
let cmd = format!("CLAUDE_CODE_TASK_LIST_ID={session_id} claude {}\n", shell_escape(prompt));
```

**Step 3: Update the call site in `create_session`**

In `create_session` (line 109), pass the session ID:

```rust
launch_claude_in_zellij(&tab_name, &prompt, &session.id)?;
```

**Step 4: Update `run_feed_next` to set the env var for the Claude subprocess**

In `src/main.rs`, update the `Command::new("claude")` call (line 540):

```rust
let status = std::process::Command::new("claude")
    .arg(&prompt)
    .env("CLAUDE_CODE_TASK_LIST_ID", session_id)
    .status()
    .context("failed to run claude")?;
```

**Step 5: Run `cargo build`**

Run: `cargo build 2>&1`
Expected: clean compile

**Step 6: Commit**

```bash
git add src/session/mod.rs src/main.rs
git commit -m "feat: set CLAUDE_CODE_TASK_LIST_ID when launching Claude sessions"
```

---

### Task 8: Render Claude progress in session detail panel

**Files:**
- Modify: `src/tui/ui.rs:321-414` (`draw_session_detail` function)

**Step 1: Add progress rendering after git stats**

In `draw_session_detail`, after the `last_activity_at` block (line 398) and before the PR URL block (line 400), add:

```rust
// Show Claude's internal task progress
if !session.claude_progress.is_empty() {
    let completed = session.claude_progress.iter().filter(|p| p.status == "completed").count();
    let total = session.claude_progress.len();
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(
            format!("  Progress: ({completed}/{total})"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));
    for item in &session.claude_progress {
        let (symbol, color) = match item.status.as_str() {
            "completed" => ("\u{2713}", Color::Green),
            "in_progress" => ("\u{25cf}", Color::Yellow),
            _ => ("\u{2610}", Color::DarkGray),
        };
        lines.push(Line::from(vec![
            Span::styled(format!("    {symbol} "), Style::default().fg(color)),
            Span::styled(&item.subject, Style::default().fg(Color::White)),
        ]));
    }
}
```

**Step 2: Run `cargo build`**

Run: `cargo build 2>&1`
Expected: clean compile

**Step 3: Commit**

```bash
git add src/tui/ui.rs
git commit -m "feat: render Claude task progress in session detail panel"
```

---

### Task 9: Clean up tmp dir on session teardown

**Files:**
- Modify: `src/session/mod.rs:119-147` (`teardown_session`)

**Step 1: Add tmp dir cleanup**

In `teardown_session`, after the worktree removal (line 139) and before the DB update (line 142), add:

```rust
// Clean up progress tmp dir
if let Ok(progress_dir) = config::session_progress_dir(session_id) {
    let _ = fs::remove_dir_all(progress_dir);
}
```

**Step 2: Run `cargo build`**

Run: `cargo build 2>&1`
Expected: clean compile

**Step 3: Commit**

```bash
git add src/session/mod.rs
git commit -m "feat: clean up progress tmp dir on session teardown"
```

---

### Task 10: Run full test suite and clippy

**Step 1: Run all tests**

Run: `cargo test 2>&1`
Expected: all tests pass

**Step 2: Run clippy**

Run: `cargo clippy 2>&1`
Expected: no warnings

**Step 3: Run format check**

Run: `cargo fmt --check 2>&1`
Expected: no changes needed

**Step 4: Final commit if any fixups were needed**

```bash
git add -A
git commit -m "chore: fix clippy and formatting issues"
```
