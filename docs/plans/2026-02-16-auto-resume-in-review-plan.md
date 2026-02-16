# Auto-resume in_review Tasks Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Automatically transition `in_review` tasks back to `in_progress` when the user continues chatting with Claude in the session's Zellij tab.

**Architecture:** Add a `UserPromptSubmit` hook that calls `claustre session-update --resumed`. The `session-update` handler checks for an `in_review` task on the session and transitions it back to `in_progress` + `Working`.

**Tech Stack:** Rust (clap CLI), bash (hook script)

---

### Task 1: Add `--resumed` flag to `SessionUpdate` CLI args

**Files:**
- Modify: `src/main.rs:84-100` (SessionUpdate variant)

**Step 1: Add the flag**

In the `SessionUpdate` variant of the `Commands` enum, add a `resumed` boolean flag after `cost`:

```rust
    SessionUpdate {
        /// Session ID to update
        #[arg(long)]
        session_id: String,
        /// PR URL — if provided, transitions the in-progress task to `in_review`
        #[arg(long)]
        pr_url: Option<String>,
        /// Cumulative input tokens from this session's conversation
        #[arg(long)]
        input_tokens: Option<i64>,
        /// Cumulative output tokens from this session's conversation
        #[arg(long)]
        output_tokens: Option<i64>,
        /// Estimated cost in USD
        #[arg(long)]
        cost: Option<f64>,
        /// Signal that the user resumed interaction — transitions in_review back to in_progress
        #[arg(long)]
        resumed: bool,
    },
```

**Step 2: Destructure the new field**

Update the match arm at `src/main.rs:352-358` to destructure `resumed`:

```rust
        Commands::SessionUpdate {
            session_id,
            pr_url,
            input_tokens,
            output_tokens,
            cost,
            resumed,
        } => {
```

**Step 3: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: Warning about unused variable `resumed`, no errors.

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: add --resumed flag to session-update CLI"
```

---

### Task 2: Handle `--resumed` in the `session-update` handler

**Files:**
- Modify: `src/main.rs:378-399` (SessionUpdate handler logic)

**Step 1: Add resume logic**

Insert a new branch after the PR-detection block (line 390) and before the idle fallback (line 391). The full handler should read:

```rust
            // If a PR URL was provided, transition the in-progress task and mark session done
            if let Some(ref url) = pr_url
                && let Some(task) = store.in_progress_task_for_session(&session_id)?
            {
                store.update_task_pr_url(&task.id, url)?;
                store.update_task_status(&task.id, store::TaskStatus::InReview)?;
                store.update_session_status(&session_id, store::ClaudeStatus::Done, "")?;

                // Fire notification
                let cfg = config::load()?;
                if cfg.notifications.enabled {
                    cfg.notifications.notify(&task.title);
                }
            } else if resumed
                && let Some(task) = store.in_review_task_for_session(&session_id)?
            {
                // User resumed interaction on an in_review task — transition back
                store.update_task_status(&task.id, store::TaskStatus::InProgress)?;
                store.update_session_status(
                    &session_id,
                    store::ClaudeStatus::Working,
                    &format!("Resumed: {}", task.title),
                )?;
            } else if store.in_progress_task_for_session(&session_id)?.is_none() {
                // No in-progress task — session is truly idle
                store.update_session_status(&session_id, store::ClaudeStatus::Idle, "")?;
            }
```

**Step 2: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: Clean build, no warnings.

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m "feat: handle --resumed flag to transition in_review back to in_progress"
```

---

### Task 3: Add `UserPromptSubmit` hook script and registration

**Files:**
- Modify: `src/session/mod.rs:299-469` (write_hooks function)

**Step 1: Update the doc comment**

Change lines 299-306 to mention three hooks:

```rust
/// Write hook scripts and register them in `.claude/settings.local.json`.
///
/// Three hooks work together:
/// - **`UserPromptSubmit`**: fires when the user sends a prompt. Resumes
///   `in_review` tasks back to `in_progress` so the TUI reflects activity
///   immediately.
/// - **`TaskCompleted`**: primary hook for syncing Claude's internal task progress
///   and token usage to claustre. Fires each time Claude marks a task completed.
/// - **`Stop`**: final validation + PR detection. Ensures progress and usage are
///   up to date after the full turn, and transitions the task to `in_review`
///   when a PR is detected.
```

**Step 2: Add the `UserPromptSubmit` hook script**

After the stop hook script (after line 421, `fs::write(&stop_path, stop_script)?;`), add:

```rust
    // ── UserPromptSubmit hook ──
    // Lightweight: just signals that the user is actively interacting,
    // so in_review tasks get resumed back to in_progress immediately.
    let user_prompt_script = r#"#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# Source common only for LOG variable
LOG="$HOME/.claustre/hook-debug.log"

SESSION_ID=$(cat "$PWD/.claustre_session_id" 2>/dev/null)
if [ -z "$SESSION_ID" ]; then
    exit 0
fi

echo "$(date -u +%FT%TZ) user-prompt sid=$SESSION_ID" >> "$LOG"
claustre session-update --session-id "$SESSION_ID" --resumed 2>> "$LOG"
exit 0
"#;
    let up_path = hooks_dir.join("user-prompt-hook.sh");
    fs::write(&up_path, user_prompt_script)?;
```

**Step 3: Add to the chmod loop**

Change the permissions loop at lines 427-429 from:

```rust
        for path in [&common_path, &tc_path, &stop_path] {
```

to:

```rust
        for path in [&common_path, &tc_path, &stop_path, &up_path] {
```

**Step 4: Register in `settings.local.json`**

Get the absolute path and add the `UserPromptSubmit` entry. After line 443 (`let stop_abs_str = ...`), add:

```rust
    let up_abs_str = up_path
        .to_str()
        .context("hook path contains invalid UTF-8")?;
```

Then update the JSON at lines 444-463 to include the new hook:

```rust
    let settings = serde_json::json!({
        "hooks": {
            "UserPromptSubmit": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": up_abs_str,
                    "timeout": 10
                }]
            }],
            "TaskCompleted": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": tc_abs_str,
                    "timeout": 30
                }]
            }],
            "Stop": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": stop_abs_str,
                    "timeout": 30
                }]
            }]
        }
    });
```

Note: `UserPromptSubmit` gets a shorter 10s timeout since it does minimal work.

**Step 5: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: Clean build.

**Step 6: Run clippy**

Run: `cargo clippy 2>&1 | tail -20`
Expected: No warnings.

**Step 7: Commit**

```bash
git add src/session/mod.rs
git commit -m "feat: add UserPromptSubmit hook to auto-resume in_review tasks"
```

---

### Task 4: Update CLAUDE.md documentation

**Files:**
- Modify: `CLAUDE.md`

**Step 1: Update the Hooks section**

In the "### Hooks" section, add documentation for the `UserPromptSubmit` hook alongside the existing `TaskCompleted` and `Stop` hook descriptions. Add after the Stop hook description:

```markdown
**`UserPromptSubmit` hook** (resume signal) — fires when the user sends a prompt:
1. Reads session ID from `.claustre_session_id`
2. Calls `claustre session-update --session-id <ID> --resumed`
3. If the session has an `in_review` task, transitions it back to `in_progress` and sets session to `Working`
```

**Step 2: Update the Task status lifecycle**

Add the resume transition to the status lifecycle table:

```markdown
| `in_review → in_progress` | UserPromptSubmit hook detects user activity and calls `session-update --resumed` | `main.rs` `SessionUpdate` handler |
```

**Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: document UserPromptSubmit hook and in_review resume flow"
```
