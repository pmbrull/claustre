use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::config;
use crate::store::{ClaudeStatus, Session, Store, Task, TaskMode, TaskStatus};

/// The name given to the Zellij tab running the Claustre TUI.
pub const CLAUSTRE_TAB_NAME: &str = "claustre";

/// Extra instructions appended to autonomous task prompts so Claude
/// works without waiting for user input.
pub const AUTONOMOUS_SUFFIX: &str = "\n\nIMPORTANT: This is an autonomous task. \
    Do NOT ask the user for clarification, confirmation, or recommendations. \
    Make your best judgment and complete the task fully on your own. \
    If something is ambiguous, pick the most reasonable option and proceed.";

/// Verify that we're running inside a Zellij session.
/// Without `ZELLIJ_SESSION_NAME`, `zellij action` commands may target the wrong session.
fn require_zellij() -> Result<()> {
    if std::env::var("ZELLIJ_SESSION_NAME").is_err() {
        bail!(
            "Not running inside a Zellij session. \
             Session operations require Zellij (ZELLIJ_SESSION_NAME not set)."
        );
    }
    Ok(())
}

/// Generate a branch name from a task title.
/// Format: `task/<slugified-title>-<short-uuid>`
pub fn generate_branch_name(title: &str) -> String {
    let slug: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let slug: String = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.len() > 40 { &slug[..40] } else { &slug };
    let slug = slug.trim_end_matches('-');
    let short_id = &Uuid::new_v4().to_string()[..8];
    format!("task/{slug}-{short_id}")
}

/// Create a full session: worktree, config, Zellij tab, and optionally launch Claude.
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
    write_merged_config(repo_path, &worktree_path)?;

    // 3. Create Zellij tab
    let tab_name = format!("{}:{}", project.name, branch_name);
    create_zellij_tab(&tab_name, &worktree_path)?;

    // 4. Create session in DB (need the ID for MCP config)
    let worktree_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;
    let session = store.create_session(project_id, branch_name, worktree_str, &tab_name)?;

    // 5. Write session ID file and Stop hook for deterministic state updates
    fs::write(worktree_path.join(".claustre_session_id"), &session.id)?;
    write_stop_hook(&worktree_path)?;

    // 6. Pre-trust the worktree so Claude doesn't prompt on first launch
    pre_trust_worktree(&worktree_path);

    // 7. Launch Claude with the task prompt
    if let Some(task) = task {
        store.assign_task_to_session(&task.id, &session.id)?;
        store.update_task_status(&task.id, TaskStatus::InProgress)?;

        // Mark session as working immediately so the TUI shows the right status
        store.update_session_status(
            &session.id,
            ClaudeStatus::Working,
            &format!("Starting: {}", task.title),
        )?;

        if task.mode == TaskMode::Autonomous {
            // Autonomous: launch feed-next which runs Claude as a blocking subprocess loop
            launch_feed_next_in_zellij(&tab_name, &session.id)?;
        } else {
            // Supervised: launch Claude directly with the prompt
            let prompt = if let Some(subtask) = store.next_pending_subtask(&task.id)? {
                store.update_subtask_status(&subtask.id, TaskStatus::InProgress)?;
                subtask.description
            } else {
                task.description.clone()
            };
            launch_claude_in_zellij(&tab_name, &prompt, &session.id)?;
        }
    }

    // 8. Return focus to the Claustre TUI tab
    return_to_claustre();

    Ok(session)
}

/// Tear down a session: close Zellij tab, remove worktree, update DB.
pub fn teardown_session(store: &Store, session_id: &str) -> Result<()> {
    let session = store.get_session(session_id)?;
    let project = store.get_project(&session.project_id)?;
    let repo_path = Path::new(&project.repo_path);

    // Capture final git stats
    if let Ok(stats) = get_git_stats(Path::new(&session.worktree_path)) {
        store.update_session_git_stats(
            session_id,
            stats.files_changed,
            stats.lines_added,
            stats.lines_removed,
        )?;
    }

    // Close Zellij tab
    let _ = close_zellij_tab(&session.zellij_tab_name);

    // Remove worktree
    let _ = remove_worktree(repo_path, Path::new(&session.worktree_path));

    // Clean up progress tmp dir
    if let Ok(progress_dir) = config::session_progress_dir(session_id) {
        let _ = fs::remove_dir_all(progress_dir);
    }

    // Update DB
    store.close_session(session_id)?;

    // Return focus to the claustre TUI tab
    return_to_claustre();

    Ok(())
}

/// Jump to a session's Zellij tab
#[expect(
    dead_code,
    reason = "useful utility, not yet wired to task-centric TUI"
)]
pub fn goto_session(session: &Session) -> Result<()> {
    require_zellij()?;
    Command::new("zellij")
        .args(["action", "go-to-tab-name", &session.zellij_tab_name])
        .status()
        .context("failed to switch Zellij tab")?;
    Ok(())
}

/// Rename the current Zellij tab to [`CLAUSTRE_TAB_NAME`].
/// Call this once on TUI startup so that `create_session` can return focus here.
pub fn name_claustre_tab() {
    let _ = Command::new("zellij")
        .args(["action", "rename-tab", CLAUSTRE_TAB_NAME])
        .status();
}

// ── Internal helpers ──

fn create_worktree(repo_path: &Path, project_name: &str, branch_name: &str) -> Result<PathBuf> {
    let worktree_base = config::worktree_base_dir()?;
    let worktree_path = worktree_base.join(project_name).join(branch_name);

    if let Some(parent) = worktree_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let repo_str = repo_path
        .to_str()
        .context("repo path contains invalid UTF-8")?;
    let wt_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;

    let output = Command::new("git")
        .args(["-C", repo_str, "worktree", "add", "-b", branch_name, wt_str])
        .output()
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        // Branch might already exist, try without -b
        let output = Command::new("git")
            .args(["-C", repo_str, "worktree", "add", wt_str, branch_name])
            .output()
            .context("failed to run git worktree add")?;

        if !output.status.success() {
            bail!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    Ok(worktree_path)
}

fn remove_worktree(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let repo_str = repo_path
        .to_str()
        .context("repo path contains invalid UTF-8")?;
    let wt_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;
    Command::new("git")
        .args(["-C", repo_str, "worktree", "remove", "--force", wt_str])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .context("failed to remove worktree")?;
    Ok(())
}

fn write_merged_config(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    // Merge CLAUDE.md
    let merged = config::merge_claude_md(repo_path)?;
    if !merged.is_empty() {
        fs::write(worktree_path.join("CLAUDE.md"), merged)?;
    }

    // Copy hooks: global first, then project overrides
    let global_hooks = config::global_hooks_dir()?;
    let project_hooks = repo_path.join(".claustre").join("hooks");
    let target_hooks = worktree_path.join(".claude").join("hooks");

    let has_hooks = global_hooks.exists() || project_hooks.exists();
    if has_hooks {
        fs::create_dir_all(&target_hooks)?;
    }

    if global_hooks.exists() {
        copy_dir_contents(&global_hooks, &target_hooks)?;
    }
    if project_hooks.exists() {
        // Project hooks override global ones with the same filename
        copy_dir_contents(&project_hooks, &target_hooks)?;
    }

    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_file() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn write_stop_hook(worktree_path: &Path) -> Result<()> {
    let hooks_dir = worktree_path.join(".claude").join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    // Stop hook script: reads Claude's task progress, then checks for PR
    // and calls `claustre session-update`
    let hook_script = r#"#!/bin/bash
SESSION_ID=$(cat "$PWD/.claustre_session_id" 2>/dev/null)
[ -z "$SESSION_ID" ] && exit 0

# Read Claude's internal task progress and write to claustre tmp dir
TASK_DIR="$HOME/.claude/tasks/$SESSION_ID"
PROGRESS_DIR="$HOME/.claustre/tmp/$SESSION_ID"

if [ -d "$TASK_DIR" ]; then
    mkdir -p "$PROGRESS_DIR"
    # Build JSON array from individual task files using jq
    PROGRESS="["
    FIRST=true
    for f in "$TASK_DIR"/[0-9]*.json; do
        [ -f "$f" ] || continue
        ITEM=$(jq -c '{subject: (.subject // ""), status: (.status // "pending")}' "$f" 2>/dev/null) || continue
        if $FIRST; then FIRST=false; else PROGRESS="$PROGRESS,"; fi
        PROGRESS="$PROGRESS$ITEM"
    done
    PROGRESS="$PROGRESS]"
    printf '%s' "$PROGRESS" > "$PROGRESS_DIR/progress.json"
fi

# Extract cumulative token usage from Claude's JSONL conversation log
USAGE_ARGS=""
JSONL_DIR="$HOME/.claude/projects"
if [ -d "$JSONL_DIR" ]; then
    # Find the most recently modified JSONL file
    LATEST=$(find "$JSONL_DIR" -path '*/sessions/*.jsonl' -type f -print0 2>/dev/null \
        | xargs -0 ls -t 2>/dev/null | head -1)
    if [ -n "$LATEST" ]; then
        # Extract max input_tokens and sum of output_tokens using jq
        read -r INPUT_T OUTPUT_T < <(
            jq -r 'select(.usage) | .usage | [(.input_tokens // 0), (.output_tokens // 0)] | @tsv' "$LATEST" 2>/dev/null \
            | awk 'BEGIN{max_in=0; sum_out=0} {if($1>max_in) max_in=$1; sum_out+=$2} END{print max_in, sum_out}'
        )
        if [ "${INPUT_T:-0}" -gt 0 ] || [ "${OUTPUT_T:-0}" -gt 0 ]; then
            COST=$(awk "BEGIN{printf \"%.6f\", ($INPUT_T * 15.0 + $OUTPUT_T * 75.0) / 1000000.0}")
            USAGE_ARGS="--input-tokens $INPUT_T --output-tokens $OUTPUT_T --cost $COST"
        fi
    fi
fi

# Check for open PR on current branch
PR_URL=$(gh pr view --json url --jq '.url' 2>/dev/null)

if [ -n "$PR_URL" ]; then
    claustre session-update --session-id "$SESSION_ID" --pr-url "$PR_URL" $USAGE_ARGS
else
    claustre session-update --session-id "$SESSION_ID" $USAGE_ARGS
fi
exit 0
"#;
    let hook_path = hooks_dir.join("stop-hook.sh");
    fs::write(&hook_path, hook_script)?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755))?;
    }

    // Write .claude/settings.local.json with the Stop hook configuration.
    // Must be settings.local.json (not settings.json) because Claude Code
    // only executes hooks from user-controlled settings files.
    let settings = serde_json::json!({
        "hooks": {
            "Stop": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": ".claude/hooks/stop-hook.sh",
                    "timeout": 30
                }]
            }]
        }
    });
    fs::write(
        worktree_path.join(".claude").join("settings.local.json"),
        serde_json::to_string_pretty(&settings)?,
    )?;

    Ok(())
}

fn create_zellij_tab(tab_name: &str, cwd: &Path) -> Result<()> {
    let cwd_str = cwd.to_str().context("cwd path contains invalid UTF-8")?;
    Command::new("zellij")
        .args(["action", "new-tab", "--name", tab_name, "--cwd", cwd_str])
        .status()
        .context("failed to create Zellij tab")?;
    Ok(())
}

fn close_zellij_tab(tab_name: &str) -> Result<()> {
    // Zellij doesn't have a direct "close tab by name" command,
    // so we verify the tab exists first, then go to it and close it.
    // Without this check, go-to-tab-name silently succeeds (exit 0) even
    // for non-existent tabs, and close-tab would kill the current tab
    // (i.e. the claustre TUI).
    let output = Command::new("zellij")
        .args(["action", "query-tab-names"])
        .output()
        .context("failed to query Zellij tab names")?;

    let tab_names = String::from_utf8_lossy(&output.stdout);
    let tab_exists = tab_names.lines().any(|line| line.trim() == tab_name);

    if tab_exists {
        let _ = Command::new("zellij")
            .args(["action", "go-to-tab-name", tab_name])
            .status();
        let _ = Command::new("zellij")
            .args(["action", "close-tab"])
            .status();
    }

    Ok(())
}

fn launch_feed_next_in_zellij(tab_name: &str, session_id: &str) -> Result<()> {
    let output = Command::new("zellij")
        .args(["action", "query-tab-names"])
        .output()
        .context("failed to query Zellij tab names")?;

    let tab_names = String::from_utf8_lossy(&output.stdout);
    let tab_exists = tab_names.lines().any(|line| line.trim() == tab_name);

    if !tab_exists {
        bail!("Zellij tab '{tab_name}' no longer exists, skipping launch");
    }

    Command::new("zellij")
        .args(["action", "go-to-tab-name", tab_name])
        .status()
        .context("failed to switch to Zellij tab")?;

    let cmd = format!(
        "CLAUDE_CODE_TASK_LIST_ID={session_id} claustre feed-next --session-id {session_id}\n"
    );
    Command::new("zellij")
        .args(["action", "write-chars", &cmd])
        .status()
        .context("failed to write to Zellij pane")?;

    Ok(())
}

fn launch_claude_in_zellij(tab_name: &str, prompt: &str, session_id: &str) -> Result<()> {
    // Verify the tab exists before writing. go-to-tab-name silently succeeds
    // (exit 0) even for non-existent tabs, which would cause write-chars to
    // type into whatever pane is currently focused — potentially a user's
    // unrelated Claude Code session.
    let output = Command::new("zellij")
        .args(["action", "query-tab-names"])
        .output()
        .context("failed to query Zellij tab names")?;

    let tab_names = String::from_utf8_lossy(&output.stdout);
    let tab_exists = tab_names.lines().any(|line| line.trim() == tab_name);

    if !tab_exists {
        bail!("Zellij tab '{tab_name}' no longer exists, skipping launch");
    }

    // Go to the tab
    Command::new("zellij")
        .args(["action", "go-to-tab-name", tab_name])
        .status()
        .context("failed to switch to Zellij tab")?;

    // Write the claude command to the pane (prompt as positional argument)
    let cmd = format!(
        "CLAUDE_CODE_TASK_LIST_ID={session_id} claude {}\n",
        shell_escape(prompt)
    );
    Command::new("zellij")
        .args(["action", "write-chars", &cmd])
        .status()
        .context("failed to write to Zellij pane")?;

    Ok(())
}

fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

struct GitStats {
    files_changed: i64,
    lines_added: i64,
    lines_removed: i64,
}

fn get_git_stats(worktree_path: &Path) -> Result<GitStats> {
    let wt_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;
    let output = Command::new("git")
        .args(["-C", wt_str, "diff", "--stat"])
        .output()
        .context("failed to run git diff --stat")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files_changed: i64 = 0;
    let mut lines_added: i64 = 0;
    let mut lines_removed: i64 = 0;

    // Parse the summary line like "3 files changed, 10 insertions(+), 5 deletions(-)"
    for line in stdout.lines() {
        if line.contains("file") && line.contains("changed") {
            for part in line.split(',') {
                let part = part.trim();
                if part.contains("file")
                    && let Some(n) = part.split_whitespace().next()
                {
                    files_changed = n.parse().unwrap_or(0);
                } else if part.contains("insertion")
                    && let Some(n) = part.split_whitespace().next()
                {
                    lines_added = n.parse().unwrap_or(0);
                } else if part.contains("deletion")
                    && let Some(n) = part.split_whitespace().next()
                {
                    lines_removed = n.parse().unwrap_or(0);
                }
            }
        }
    }

    Ok(GitStats {
        files_changed,
        lines_added,
        lines_removed,
    })
}

/// Switch Zellij focus back to the Claustre TUI tab.
fn return_to_claustre() {
    let _ = Command::new("zellij")
        .args(["action", "go-to-tab-name", CLAUSTRE_TAB_NAME])
        .status();
}

/// Pre-seed trust for a worktree path in `~/.claude.json` so Claude Code
/// doesn't prompt "trust folder contents" on first launch.
fn pre_trust_worktree(worktree_path: &Path) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let claude_json_path = home.join(".claude.json");

    // Read existing config or start fresh
    let mut config: serde_json::Value = fs::read_to_string(&claude_json_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let projects = config.as_object_mut().and_then(|obj| {
        obj.entry("projects")
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
    });

    let Some(projects) = projects else { return };

    let wt_key = worktree_path.to_string_lossy().to_string();
    let entry = projects
        .entry(&wt_key)
        .or_insert_with(|| serde_json::json!({}));

    if let Some(obj) = entry.as_object_mut() {
        obj.insert(
            "hasTrustDialogAccepted".to_string(),
            serde_json::Value::Bool(true),
        );
    }

    // Best-effort write — don't fail the session if this doesn't work
    let _ = fs::write(
        &claude_json_path,
        serde_json::to_string_pretty(&config).unwrap_or_default(),
    );
}
