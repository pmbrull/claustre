//! Git worktree lifecycle, session setup, and teardown.
//!
//! Creates worktrees, writes merged config and hooks, and cleans up on
//! session completion.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::config;
use crate::store::{ClaudeStatus, Store, Task, TaskMode, TaskStatus};

/// Extra instructions appended to autonomous task prompts so Claude
/// works without waiting for user input.
pub const AUTONOMOUS_SUFFIX: &str = "\n\nIMPORTANT: This is an autonomous task. \
    Do NOT ask the user for clarification, confirmation, or recommendations. \
    Make your best judgment and complete the task fully on your own. \
    If something is ambiguous, pick the most reasonable option and proceed.";

/// Task completion instructions appended to every prompt (autonomous and supervised).
/// Tells Claude how to signal that work is done so the Stop hook can detect the PR.
pub fn completion_instructions(default_branch: &str, push_mode: crate::store::PushMode) -> String {
    match push_mode {
        crate::store::PushMode::Pr => format!(
            "\n\nWhen you finish your task:\n\
            1. Commit all changes with a descriptive commit message\n\
            2. Push the branch: `git push -u origin HEAD`\n\
            3. Create a pull request against `{default_branch}` using `gh pr create`\n\n\
            IMPORTANT: Do NOT include any 'Generated with Claude Code' or similar footer in the PR body."
        ),
        crate::store::PushMode::Push => "\n\nWhen you finish your task:\n\
            1. Commit all changes with a descriptive commit message\n\
            2. Push the branch: `git push -u origin HEAD`"
            .to_string(),
    }
}

/// Wrap a command so that after it exits, the PTY drops to an interactive shell.
///
/// This makes the Claude pane behave like a normal terminal: when Claude finishes,
/// the user gets a shell prompt instead of a stuck/frozen pane.
///
/// Uses `/bin/sh -c '"$@"; exec $SHELL' _ <original-cmd...>` so that:
/// 1. `"$@"` runs the original command with proper argument handling
/// 2. When it exits, `exec $SHELL` replaces the wrapper with the user's shell
/// 3. The PTY stays alive and interactive
pub fn wrap_cmd_with_shell_fallback(cmd: Vec<String>) -> Vec<String> {
    let mut wrapped = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        r#""$@"; exec "${SHELL:-/bin/sh}" -l"#.to_string(),
        "_".to_string(),
    ];
    wrapped.extend(cmd);
    wrapped
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

/// Information needed by the TUI to spawn PTY terminals after session setup.
pub struct SessionSetup {
    pub session: crate::store::Session,
    pub tab_label: String,
    /// The Claude command to run in the PTY (already wrapped with shell fallback).
    /// `None` when no task was assigned (bare session).
    pub claude_cmd: Option<Vec<String>>,
    pub worktree_path: String,
}

/// Guard that cleans up partially-created session resources on failure.
///
/// If `create_session()` fails after creating the worktree or DB row, this
/// guard ensures the orphaned resources are removed. Call `disarm()` on
/// success to prevent cleanup.
struct SessionCleanupGuard<'a> {
    store: &'a Store,
    repo_path: &'a Path,
    worktree_path: Option<PathBuf>,
    session_id: Option<String>,
}

impl<'a> SessionCleanupGuard<'a> {
    fn new(store: &'a Store, repo_path: &'a Path) -> Self {
        Self {
            store,
            repo_path,
            worktree_path: None,
            session_id: None,
        }
    }

    /// Prevent cleanup — called when session creation succeeds.
    fn disarm(mut self) {
        self.worktree_path = None;
        self.session_id = None;
    }
}

impl Drop for SessionCleanupGuard<'_> {
    fn drop(&mut self) {
        if let Some(ref session_id) = self.session_id {
            let _ = self.store.close_session(session_id);
        }
        if let Some(ref wt_path) = self.worktree_path {
            let _ = remove_worktree(self.repo_path, wt_path);
        }
    }
}

/// Create a full session: worktree, config, DB record, hooks.
/// Returns a `SessionSetup` with the info needed for the TUI to spawn PTY terminals.
///
/// When `base_branch` is `Some`, the worktree is created from that branch instead
/// of the project's default branch. PRs will also target the base branch.
/// This supports hotfix/release workflows where work targets a non-default branch.
///
/// When `remote_enabled` is true, Claude is launched with `--remote`.
pub fn create_session(
    store: &Store,
    project_id: &str,
    branch_name: &str,
    task: Option<&Task>,
    base_branch: Option<&str>,
    remote_enabled: bool,
) -> Result<SessionSetup> {
    let project = store.get_project(project_id)?;
    let repo_path = Path::new(&project.repo_path);
    let mut guard = SessionCleanupGuard::new(store, repo_path);

    // Use the task's base branch if set, otherwise fall back to project default
    let effective_base = base_branch.unwrap_or(&project.default_branch);

    // 1. Create the worktree from the effective base branch
    let wt_mode = WorktreeMode::NewBranch {
        default_branch: effective_base,
    };
    let worktree_path = create_worktree(repo_path, &project.name, branch_name, wt_mode)?;
    guard.worktree_path = Some(worktree_path.clone());

    // 2. Copy IDE run configurations so IntelliJ/etc. work in worktrees
    copy_run_directory(repo_path, &worktree_path)?;

    // 3. Merge config into worktree
    write_merged_config(repo_path, &worktree_path)?;

    // 4. Create session in DB
    let tab_label = format!("{}:{}", project.name, branch_name);
    let worktree_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;
    let session = store.create_session(project_id, branch_name, worktree_str, &tab_label)?;
    guard.session_id = Some(session.id.clone());

    // 5. Write session ID file and hooks
    fs::write(worktree_path.join(".claustre_session_id"), &session.id)?;
    write_hooks(&worktree_path)?;

    // 6. Hide claustre-managed files from git status
    configure_git_excludes(&worktree_path);

    // 7. Pre-trust the worktree so Claude doesn't prompt on first launch
    pre_trust_worktree(&worktree_path);

    // 8. Build the Claude command (spawned by the TUI as a local PTY)
    let mut claude_cmd = None;
    if let Some(task) = task {
        store.assign_task_to_session(&task.id, &session.id)?;
        store.update_task_status(&task.id, TaskStatus::Working)?;

        store.update_session_status(
            &session.id,
            ClaudeStatus::Working,
            &format!("Starting: {}", task.title),
        )?;

        let cmd = match task.mode {
            TaskMode::Autonomous => {
                // Autonomous: feed-next runs Claude as a blocking subprocess loop
                let claustre_exe = std::env::current_exe()
                    .unwrap_or_else(|_| std::path::PathBuf::from("claustre"));
                let mut cmd = vec![
                    claustre_exe.to_string_lossy().to_string(),
                    "feed-next".to_string(),
                    "--session-id".to_string(),
                    session.id.clone(),
                ];
                if remote_enabled {
                    cmd.push("--remote".to_string());
                }
                cmd
            }
            TaskMode::Exploration => {
                // Exploration: launch Claude interactively with no prompt
                let mut cmd = vec!["claude".to_string()];
                if remote_enabled {
                    cmd.push("--remote".to_string());
                }
                cmd
            }
            TaskMode::Supervised => {
                // Supervised: launch Claude directly with the prompt
                let instructions = completion_instructions(effective_base, task.push_mode);
                let prompt = if let Some(subtask) = store.next_pending_subtask(&task.id)? {
                    store.update_subtask_status(&subtask.id, TaskStatus::Working)?;
                    format!("{}{instructions}", subtask.description)
                } else {
                    format!("{}{instructions}", task.description)
                };
                let mut cmd = vec!["claude".to_string()];
                if remote_enabled {
                    cmd.push("--remote".to_string());
                }
                cmd.push(prompt);
                cmd
            }
        };

        // Wrap the command so the PTY drops to a shell after Claude exits
        claude_cmd = Some(wrap_cmd_with_shell_fallback(cmd));
    }

    // Success — prevent guard from cleaning up
    guard.disarm();

    Ok(SessionSetup {
        session,
        tab_label,
        claude_cmd,
        worktree_path: worktree_str.to_string(),
    })
}

/// Tear down a session: remove worktree, update DB.
/// The TUI is responsible for removing the session tab (dropping the PTY handles).
pub fn teardown_session(store: &Store, session_id: &str) -> Result<()> {
    let session = store.get_session(session_id)?;
    let project = store.get_project(&session.project_id)?;
    let repo_path = Path::new(&project.repo_path);

    // Capture final git stats
    if let Ok(stats) = get_git_stats(Path::new(&session.worktree_path), &project.default_branch) {
        store.update_session_git_stats(
            session_id,
            stats.files_changed,
            stats.lines_added,
            stats.lines_removed,
        )?;
    }

    // Remove worktree
    let _ = remove_worktree(repo_path, Path::new(&session.worktree_path));

    // Remove the trust entry from ~/.claude.json so stale worktree paths don't accumulate
    remove_trust_entry(Path::new(&session.worktree_path));

    // Clean up progress tmp dir
    if let Ok(progress_dir) = config::session_progress_dir(session_id) {
        let _ = fs::remove_dir_all(progress_dir);
    }

    // Update DB
    store.close_session(session_id)?;

    Ok(())
}

// ── Internal helpers ──

/// Configuration for worktree creation: which branch to base the new worktree on.
#[derive(Clone, Copy)]
enum WorktreeMode<'a> {
    /// Create a new branch from `origin/<base_branch>`.
    NewBranch { default_branch: &'a str },
}

/// Create a git worktree for a session.
///
/// Fetches the base branch from origin, then creates a new local branch
/// starting from `origin/<base_branch>`. If the local branch already exists,
/// falls back to checking it out directly.
fn create_worktree(
    repo_path: &Path,
    project_name: &str,
    branch_name: &str,
    mode: WorktreeMode<'_>,
) -> Result<PathBuf> {
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

    let WorktreeMode::NewBranch { default_branch } = &mode;
    let fetch_branch = *default_branch;
    let origin_ref = format!("origin/{default_branch}");

    // Fetch latest from origin
    let fetch_output = Command::new("git")
        .args(["-C", repo_str, "fetch", "origin", fetch_branch])
        .output()
        .context("failed to run git fetch origin")?;

    if !fetch_output.status.success() {
        bail!(
            "git fetch origin {fetch_branch} failed: {}",
            String::from_utf8_lossy(&fetch_output.stderr)
        );
    }

    // Create worktree with a new branch based on origin/<base_branch>
    let args = [
        "-C",
        repo_str,
        "worktree",
        "add",
        "-b",
        branch_name,
        wt_str,
        &origin_ref,
    ];

    let output = Command::new("git")
        .args(args)
        .output()
        .context("failed to run git worktree add")?;

    if !output.status.success() {
        // Branch might already exist — try checking it out directly
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

/// Copy the `.run/` directory from the parent repo into the worktree so that
/// IDE run configurations (`IntelliJ`, etc.) are available without reconfiguring.
fn copy_run_directory(repo_path: &Path, worktree_path: &Path) -> Result<()> {
    let run_src = repo_path.join(".run");
    if run_src.is_dir() {
        let run_dst = worktree_path.join(".run");
        copy_dir_recursive(&run_src, &run_dst)?;
    }
    Ok(())
}

/// Recursively copy a directory tree from `src` to `dst`.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
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

/// Write hook scripts and register them in `.claude/settings.local.json`.
///
/// Three hooks work together:
/// - **`UserPromptSubmit`**: fires when the user sends a prompt. Resumes
///   `in_review` tasks back to `working` so the TUI reflects activity
///   immediately.
/// - **`TaskCompleted`**: primary hook for syncing Claude's internal task progress
///   and token usage to claustre. Fires each time Claude marks a task completed.
/// - **`Stop`**: final validation + PR detection. Ensures progress and usage are
///   up to date after the full turn, and transitions the task to `in_review`
///   when a PR is detected.
fn write_hooks(worktree_path: &Path) -> Result<()> {
    let hooks_dir = worktree_path.join(".claude").join("hooks");
    fs::create_dir_all(&hooks_dir)?;

    // ── Shared helper sourced by both hooks ──
    // Reads progress files + extracts token usage, sets USAGE_ARGS.
    // Only the Stop hook calls extract_usage() — the TaskCompleted hook
    // only needs sync_progress() to avoid redundant JSONL scanning.
    let common_script = r#"#!/bin/bash
# Shared helper for claustre hooks — sourced, not executed directly.
# Expects SESSION_ID and WORKTREE_ROOT to be set by the caller.

LOG="$HOME/.claustre/hook-debug.log"

# Read Claude's internal task progress and write to claustre tmp dir
sync_progress() {
    local TASK_DIR="$HOME/.claude/tasks/$SESSION_ID"
    local PROGRESS_DIR="$HOME/.claustre/tmp/$SESSION_ID"

    if [ -d "$TASK_DIR" ]; then
        mkdir -p "$PROGRESS_DIR"
        local PROGRESS="["
        local FIRST=true
        for f in "$TASK_DIR"/[0-9]*.json; do
            [ -f "$f" ] || continue
            local ITEM
            ITEM=$(jq -c '{subject: (.subject // ""), status: (.status // "pending")}' "$f" 2>/dev/null) || continue
            if $FIRST; then FIRST=false; else PROGRESS="$PROGRESS,"; fi
            PROGRESS="$PROGRESS$ITEM"
        done
        PROGRESS="$PROGRESS]"
        printf '%s' "$PROGRESS" > "$PROGRESS_DIR/progress.json"
    fi
}

# Extract cumulative token usage from Claude's JSONL conversation log.
# Sets USAGE_ARGS with --input-tokens / --output-tokens flags.
# NOTE: Only called by the Stop hook (final sweep). The TaskCompleted hook
# skips this to avoid redundant full-file jq scans on every internal task.
# Optimization: reads only the last 200 lines of the JSONL to avoid scanning
# multi-megabyte conversation logs. Token usage is cumulative in assistant
# messages, so summing the last batch is sufficient.
extract_usage() {
    USAGE_ARGS=""
    CLAUDE_SID=""
    local PROJECT_HASH
    PROJECT_HASH=$(printf '%s' "$WORKTREE_ROOT" | sed 's/[^a-zA-Z0-9]/-/g')
    local PROJECT_DIR="$HOME/.claude/projects/$PROJECT_HASH"

    if [ -d "$PROJECT_DIR" ]; then
        local LATEST
        LATEST=$(ls -t "$PROJECT_DIR"/*.jsonl 2>/dev/null | head -1)
        if [ -n "$LATEST" ]; then
            # Extract Claude's internal session ID from the JSONL filename
            CLAUDE_SID=$(basename "$LATEST" .jsonl)

            local INPUT_T OUTPUT_T
            read -r INPUT_T OUTPUT_T < <(
                tail -200 "$LATEST" \
                | jq -r 'select(.type == "assistant") | .message.usage | [(.input_tokens // 0) + (.cache_creation_input_tokens // 0) + (.cache_read_input_tokens // 0), (.output_tokens // 0)] | @tsv' 2>/dev/null \
                | awk 'BEGIN{sum_in=0; sum_out=0} {sum_in+=$1; sum_out+=$2} END{print sum_in, sum_out}'
            )
            if [ "${INPUT_T:-0}" -gt 0 ] || [ "${OUTPUT_T:-0}" -gt 0 ]; then
                USAGE_ARGS="--input-tokens $INPUT_T --output-tokens $OUTPUT_T"
            fi
        fi
    fi
}
"#;
    let common_path = hooks_dir.join("_claustre-common.sh");
    fs::write(&common_path, common_script)?;

    // ── TaskCompleted hook ──
    // Syncs Claude's internal task progress each time it marks a task done.
    // Only calls sync_progress() — token extraction is deferred to the Stop
    // hook to avoid redundant full-file JSONL scans on every internal task.
    let task_completed_script = r#"#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKTREE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
source "$SCRIPT_DIR/_claustre-common.sh"

SESSION_ID=$(cat "$WORKTREE_ROOT/.claustre_session_id" 2>/dev/null)
if [ -z "$SESSION_ID" ]; then
    echo "$(date -u +%FT%TZ) SKIP task-completed: no session id at WORKTREE_ROOT=$WORKTREE_ROOT" >> "$LOG"
    exit 0
fi

sync_progress

echo "$(date -u +%FT%TZ) task-completed sid=$SESSION_ID" >> "$LOG"
claustre session-update --session-id "$SESSION_ID" 2>> "$LOG"
echo "$(date -u +%FT%TZ) task-completed sid=$SESSION_ID exit=$?" >> "$LOG"
exit 0
"#;
    let tc_path = hooks_dir.join("task-completed-hook.sh");
    fs::write(&tc_path, task_completed_script)?;

    // ── Stop hook ──
    // Final validation: ensures progress/usage are current, detects PRs.
    let stop_script = r#"#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKTREE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
source "$SCRIPT_DIR/_claustre-common.sh"

SESSION_ID=$(cat "$WORKTREE_ROOT/.claustre_session_id" 2>/dev/null)
if [ -z "$SESSION_ID" ]; then
    echo "$(date -u +%FT%TZ) SKIP stop: no session id at WORKTREE_ROOT=$WORKTREE_ROOT" >> "$LOG"
    exit 0
fi

sync_progress
extract_usage

# Build common args for session-update
CSID_ARGS=""
if [ -n "$CLAUDE_SID" ]; then
    CSID_ARGS="--claude-session-id $CLAUDE_SID"
fi

# Check for open PR on current branch only (no fallback to other branches —
# gh pr list would pick up PRs from unrelated sessions and cause cross-session spam)
PR_URL=$(cd "$WORKTREE_ROOT" && gh pr view --json url --jq '.url' 2>/dev/null)

if [ -n "$PR_URL" ]; then
    echo "$(date -u +%FT%TZ) stop sid=$SESSION_ID pr=$PR_URL usage='$USAGE_ARGS' csid=$CLAUDE_SID" >> "$LOG"
    claustre session-update --session-id "$SESSION_ID" --pr-url "$PR_URL" $USAGE_ARGS $CSID_ARGS 2>> "$LOG"
else
    echo "$(date -u +%FT%TZ) stop sid=$SESSION_ID no-pr usage='$USAGE_ARGS' csid=$CLAUDE_SID" >> "$LOG"
    claustre session-update --session-id "$SESSION_ID" $USAGE_ARGS $CSID_ARGS 2>> "$LOG"
fi
echo "$(date -u +%FT%TZ) stop sid=$SESSION_ID exit=$?" >> "$LOG"
exit 0
"#;
    let stop_path = hooks_dir.join("stop-hook.sh");
    fs::write(&stop_path, stop_script)?;

    // ── UserPromptSubmit hook ──
    // Lightweight: just signals that the user is actively interacting,
    // so in_review tasks get resumed back to working immediately.
    let user_prompt_script = r#"#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKTREE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG="$HOME/.claustre/hook-debug.log"

SESSION_ID=$(cat "$WORKTREE_ROOT/.claustre_session_id" 2>/dev/null)
if [ -z "$SESSION_ID" ]; then
    exit 0
fi

echo "$(date -u +%FT%TZ) user-prompt sid=$SESSION_ID" >> "$LOG"
claustre session-update --session-id "$SESSION_ID" --resumed 2>> "$LOG"
exit 0
"#;
    let up_path = hooks_dir.join("user-prompt-hook.sh");
    fs::write(&up_path, user_prompt_script)?;

    // Make all hook scripts executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&common_path, &tc_path, &stop_path, &up_path] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
    }

    // Write .claude/settings.local.json with both hook configurations.
    // Must be settings.local.json (not settings.json) because Claude Code
    // only executes hooks from user-controlled settings files.
    // Use absolute paths because Claude Code runs hooks from its current
    // working directory, which may be a subdirectory if Claude cd'd during
    // the session.
    let tc_abs_str = tc_path
        .to_str()
        .context("hook path contains invalid UTF-8")?;
    let stop_abs_str = stop_path
        .to_str()
        .context("hook path contains invalid UTF-8")?;
    let up_abs_str = up_path
        .to_str()
        .context("hook path contains invalid UTF-8")?;
    // Shell-quote hook paths so spaces in project names (e.g. "Docs OM")
    // don't cause word splitting when Claude Code runs `/bin/sh -c <command>`.
    let tc_cmd = shell_quote(tc_abs_str);
    let stop_cmd = shell_quote(stop_abs_str);
    let up_cmd = shell_quote(up_abs_str);
    let settings = serde_json::json!({
        "env": {
            // Signals to global hooks that this is a claustre-managed session.
            // Hooks like claude-md-check.sh and todo-scanner.sh can check this
            // to skip token-wasting work (CLAUDE.md updates, TODO scanning)
            // that bloats context in autonomous task sessions.
            "CLAUSTRE_SESSION": "1"
        },
        "hooks": {
            "UserPromptSubmit": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": up_cmd,
                    "timeout": 10
                }]
            }],
            "TaskCompleted": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": tc_cmd,
                    "timeout": 30
                }]
            }],
            "Stop": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": stop_cmd,
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

/// POSIX shell-quote a string so it's safe to embed in `/bin/sh -c`.
/// Wraps in single quotes and escapes any embedded single quotes.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

struct GitStats {
    files_changed: i64,
    lines_added: i64,
    lines_removed: i64,
}

fn get_git_stats(worktree_path: &Path, default_branch: &str) -> Result<GitStats> {
    let wt_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;
    let origin_branch = format!("origin/{default_branch}");
    let output = Command::new("git")
        .args(["-C", wt_str, "diff", "--numstat", &origin_branch])
        .output()
        .context("failed to run git diff --numstat")?;

    if !output.status.success() {
        bail!(
            "git diff --numstat failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_numstat(&stdout))
}

/// Parse `git diff --numstat` output into aggregate stats.
///
/// Each line is `<added>\t<removed>\t<filename>`. Binary files show `-` for counts.
/// This is locale-independent and machine-parseable, unlike `--stat`.
fn parse_git_numstat(output: &str) -> GitStats {
    let mut files_changed: i64 = 0;
    let mut lines_added: i64 = 0;
    let mut lines_removed: i64 = 0;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let added = parts.next().unwrap_or("0");
        let removed = parts.next().unwrap_or("0");
        // Binary files show "-" for both counts — count the file but skip line stats
        files_changed += 1;
        lines_added += added.parse::<i64>().unwrap_or(0);
        lines_removed += removed.parse::<i64>().unwrap_or(0);
    }

    GitStats {
        files_changed,
        lines_added,
        lines_removed,
    }
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

/// Remove the trust entry for a worktree path from `~/.claude.json`.
///
/// This is the inverse of `pre_trust_worktree()`. Called during teardown to prevent
/// stale worktree paths from accumulating in the trust file.
fn remove_trust_entry(worktree_path: &Path) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let claude_json_path = home.join(".claude.json");

    let Ok(content) = fs::read_to_string(&claude_json_path) else {
        return;
    };
    let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&content) else {
        return;
    };

    let wt_key = worktree_path.to_string_lossy().to_string();
    if let Some(projects) = config.get_mut("projects").and_then(|p| p.as_object_mut())
        && projects.remove(&wt_key).is_some()
    {
        let _ = fs::write(
            &claude_json_path,
            serde_json::to_string_pretty(&config).unwrap_or_default(),
        );
    }
}

/// Files claustre writes into worktrees that should be hidden from `git status`.
const CLAUSTRE_MANAGED_FILES: &[&str] = &[
    ".claustre_session_id",
    ".claude/settings.local.json",
    ".claude/hooks/_claustre-common.sh",
    ".claude/hooks/task-completed-hook.sh",
    ".claude/hooks/stop-hook.sh",
    ".claude/hooks/user-prompt-hook.sh",
];

/// Hide claustre-managed files from `git status` in the worktree.
///
/// Uses two mechanisms:
/// - **Exclude file** (`<git-dir>/info/exclude`): hides untracked files,
///   scoped to this worktree only (doesn't touch `.gitignore`).
/// - **`skip-worktree` bit**: hides modifications to tracked files that
///   claustre overwrites (e.g., hook scripts already committed to the repo).
///
/// Best-effort — failures are logged but don't fail session creation.
fn configure_git_excludes(worktree_path: &Path) {
    let Some(wt_str) = worktree_path.to_str() else {
        return;
    };

    // 1. Find the worktree-specific git dir (e.g., <repo>/.git/worktrees/<name>)
    let output = Command::new("git")
        .args(["-C", wt_str, "rev-parse", "--git-dir"])
        .output();
    let git_dir = match output {
        Ok(ref o) if o.status.success() => {
            let raw = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let path = PathBuf::from(&raw);
            // git rev-parse --git-dir returns a path relative to the worktree
            if path.is_absolute() {
                path
            } else {
                worktree_path.join(path)
            }
        }
        _ => return,
    };

    // 2. Append claustre patterns to the exclude file
    let exclude_path = git_dir.join("info").join("exclude");
    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    if !existing.contains("# claustre managed files") {
        let _ = fs::create_dir_all(exclude_path.parent().expect("exclude has parent"));
        let mut content = existing;
        content.push_str("\n# claustre managed files\n");
        for pattern in CLAUSTRE_MANAGED_FILES {
            content.push('/');
            content.push_str(pattern);
            content.push('\n');
        }
        let _ = fs::write(&exclude_path, content);
    }

    // 3. Mark tracked files with skip-worktree so modifications don't show
    for file in CLAUSTRE_MANAGED_FILES {
        let _ = Command::new("git")
            .args(["-C", wt_str, "update-index", "--skip-worktree", file])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── generate_branch_name ──

    #[test]
    fn branch_name_basic_slug() {
        let name = generate_branch_name("Add login page");
        assert!(name.starts_with("task/add-login-page-"));
        // Should end with an 8-char hex UUID suffix
        let suffix = name.rsplit('-').next().unwrap();
        assert_eq!(suffix.len(), 8);
    }

    #[test]
    fn branch_name_special_chars() {
        let name = generate_branch_name("Fix bug #123: handle edge-case!");
        // All non-alphanumeric chars become hyphens, consecutive hyphens collapsed
        assert!(name.starts_with("task/fix-bug-123-handle-edge-case-"));
    }

    #[test]
    fn branch_name_truncation() {
        let long_title = "a".repeat(100);
        let name = generate_branch_name(&long_title);
        // The slug portion (between "task/" and the last "-<uuid>") should be at most 40 chars
        let without_prefix = name.strip_prefix("task/").unwrap();
        let slug_end = without_prefix.rfind('-').unwrap();
        assert!(slug_end <= 40);
    }

    #[test]
    fn branch_name_empty_title() {
        let name = generate_branch_name("");
        // Empty slug still produces a valid branch with UUID
        assert!(name.starts_with("task/"));
    }

    // ── completion_instructions ──

    #[test]
    fn completion_instructions_contains_branch() {
        let instructions = completion_instructions("develop", crate::store::PushMode::Pr);
        assert!(instructions.contains("develop"));
        assert!(instructions.contains("gh pr create"));
    }

    #[test]
    fn completion_instructions_push_mode() {
        let instructions = completion_instructions("develop", crate::store::PushMode::Push);
        assert!(!instructions.contains("gh pr create"));
        assert!(instructions.contains("git push"));
    }

    #[test]
    fn completion_instructions_targets_release_branch() {
        let instructions = completion_instructions("release/1.0", crate::store::PushMode::Pr);
        assert!(instructions.contains("release/1.0"));
        assert!(instructions.contains("gh pr create"));
        assert!(!instructions.contains("main"));
    }

    // ── wrap_cmd_with_shell_fallback ──

    #[test]
    fn wrap_preserves_args() {
        let cmd = vec!["claude".to_string(), "hello world".to_string()];
        let wrapped = wrap_cmd_with_shell_fallback(cmd);
        assert_eq!(wrapped[0], "/bin/sh");
        assert_eq!(wrapped[1], "-c");
        // Original args are passed after the shell wrapper
        assert_eq!(wrapped[4], "claude");
        assert_eq!(wrapped[5], "hello world");
    }

    #[test]
    fn wrap_adds_shell_wrapper() {
        let cmd = vec!["echo".to_string(), "test".to_string()];
        let wrapped = wrap_cmd_with_shell_fallback(cmd);
        // The -c argument should contain exec $SHELL
        assert!(wrapped[2].contains("exec"));
        assert!(wrapped[2].contains("SHELL"));
    }

    // ── shell_quote ──

    #[test]
    fn shell_quote_simple_string() {
        assert_eq!(shell_quote("hello"), "'hello'");
    }

    #[test]
    fn shell_quote_embedded_single_quotes() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn shell_quote_spaces() {
        assert_eq!(shell_quote("hello world"), "'hello world'");
    }

    // ── parse_git_numstat ──

    #[test]
    fn parse_git_numstat_full() {
        let output = "8\t5\tsrc/main.rs\n3\t2\tsrc/lib.rs\n";
        let stats = parse_git_numstat(output);
        assert_eq!(stats.files_changed, 2);
        assert_eq!(stats.lines_added, 11);
        assert_eq!(stats.lines_removed, 7);
    }

    #[test]
    fn parse_git_numstat_insertions_only() {
        let output = "15\t0\tsrc/new_file.rs\n";
        let stats = parse_git_numstat(output);
        assert_eq!(stats.files_changed, 1);
        assert_eq!(stats.lines_added, 15);
        assert_eq!(stats.lines_removed, 0);
    }

    #[test]
    fn parse_git_numstat_deletions_only() {
        let output = "0\t10\tsrc/old.rs\n0\t5\tsrc/removed.rs\n0\t5\tsrc/gone.rs\n";
        let stats = parse_git_numstat(output);
        assert_eq!(stats.files_changed, 3);
        assert_eq!(stats.lines_added, 0);
        assert_eq!(stats.lines_removed, 20);
    }

    #[test]
    fn parse_git_numstat_empty_output() {
        let stats = parse_git_numstat("");
        assert_eq!(stats.files_changed, 0);
        assert_eq!(stats.lines_added, 0);
        assert_eq!(stats.lines_removed, 0);
    }

    #[test]
    fn parse_git_numstat_binary_files() {
        // Binary files show "-" for added/removed counts
        let output = "-\t-\timage.png\n5\t3\tsrc/main.rs\n";
        let stats = parse_git_numstat(output);
        assert_eq!(stats.files_changed, 2);
        assert_eq!(stats.lines_added, 5);
        assert_eq!(stats.lines_removed, 3);
    }

    // ── copy_dir_recursive ──

    #[test]
    fn copy_dir_recursive_copies_files_and_subdirs() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        // Create src structure: file.txt, sub/nested.txt
        fs::write(src.path().join("file.txt"), "hello").unwrap();
        fs::create_dir(src.path().join("sub")).unwrap();
        fs::write(src.path().join("sub").join("nested.txt"), "world").unwrap();

        let dst_path = dst.path().join("copied");
        copy_dir_recursive(src.path(), &dst_path).unwrap();

        assert_eq!(
            fs::read_to_string(dst_path.join("file.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs::read_to_string(dst_path.join("sub").join("nested.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn copy_dir_recursive_noop_for_nonexistent_src() {
        let dst = tempfile::tempdir().unwrap();
        // Source doesn't exist — should return Ok without creating dst
        let result = copy_dir_recursive(Path::new("/nonexistent"), &dst.path().join("out"));
        assert!(result.is_ok());
    }

    // ── copy_dir_contents ──

    #[test]
    fn copy_dir_contents_copies_files_only() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        fs::write(src.path().join("a.txt"), "alpha").unwrap();
        fs::write(src.path().join("b.txt"), "beta").unwrap();
        // Subdirectory should NOT be copied (copy_dir_contents only copies files)
        fs::create_dir(src.path().join("subdir")).unwrap();
        fs::write(src.path().join("subdir").join("c.txt"), "gamma").unwrap();

        copy_dir_contents(src.path(), dst.path()).unwrap();

        assert_eq!(
            fs::read_to_string(dst.path().join("a.txt")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs::read_to_string(dst.path().join("b.txt")).unwrap(),
            "beta"
        );
        // Subdirectory should not be copied
        assert!(!dst.path().join("subdir").exists());
    }

    #[test]
    fn copy_dir_contents_overrides_existing_files() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        fs::write(dst.path().join("config.txt"), "old").unwrap();
        fs::write(src.path().join("config.txt"), "new").unwrap();

        copy_dir_contents(src.path(), dst.path()).unwrap();

        assert_eq!(
            fs::read_to_string(dst.path().join("config.txt")).unwrap(),
            "new"
        );
    }

    // ── parse_git_numstat edge cases ──

    #[test]
    fn parse_git_numstat_whitespace_lines() {
        let output = "  \n\n5\t3\tsrc/main.rs\n  \n";
        let stats = parse_git_numstat(output);
        assert_eq!(stats.files_changed, 1);
        assert_eq!(stats.lines_added, 5);
        assert_eq!(stats.lines_removed, 3);
    }

    // ── AUTONOMOUS_SUFFIX ──

    #[test]
    fn autonomous_suffix_contains_key_instructions() {
        assert!(AUTONOMOUS_SUFFIX.contains("autonomous"));
        assert!(AUTONOMOUS_SUFFIX.contains("Do NOT ask"));
    }

    // ── CLAUSTRE_MANAGED_FILES ──

    #[test]
    fn managed_files_list_is_not_empty() {
        assert!(!CLAUSTRE_MANAGED_FILES.is_empty());
        assert!(CLAUSTRE_MANAGED_FILES.contains(&".claustre_session_id"));
        assert!(CLAUSTRE_MANAGED_FILES.contains(&".claude/settings.local.json"));
    }
}
