use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

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
pub const COMPLETION_INSTRUCTIONS: &str = "\n\nWhen you finish your task:\n\
    1. Commit all changes with a descriptive commit message\n\
    2. Push the branch: `git push -u origin HEAD`\n\
    3. Create a pull request against `main` using `gh pr create`\n\n\
    IMPORTANT: Do NOT include any 'Generated with Claude Code' or similar footer in the PR body.";

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
    /// Path to the session-host's Unix socket (None if no task was assigned).
    pub socket_path: Option<PathBuf>,
    pub worktree_path: String,
}

/// Create a full session: worktree, config, DB record, hooks.
/// Returns a `SessionSetup` with the info needed for the TUI to spawn PTY terminals.
pub fn create_session(
    store: &Store,
    project_id: &str,
    branch_name: &str,
    task: Option<&Task>,
) -> Result<SessionSetup> {
    let project = store.get_project(project_id)?;
    let repo_path = Path::new(&project.repo_path);

    // 1. Create the worktree
    let worktree_path = create_worktree(repo_path, &project.name, branch_name)?;

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

    // 5. Write session ID file and hooks
    fs::write(worktree_path.join(".claustre_session_id"), &session.id)?;
    write_hooks(&worktree_path)?;

    // 6. Pre-trust the worktree so Claude doesn't prompt on first launch
    pre_trust_worktree(&worktree_path);

    // 7. Build the Claude command and spawn session-host
    let mut socket_path = None;
    if let Some(task) = task {
        store.assign_task_to_session(&task.id, &session.id)?;
        store.update_task_status(&task.id, TaskStatus::Working)?;

        store.update_session_status(
            &session.id,
            ClaudeStatus::Working,
            &format!("Starting: {}", task.title),
        )?;

        let claude_cmd = if task.mode == TaskMode::Autonomous {
            // Autonomous: feed-next runs Claude as a blocking subprocess loop
            let claustre_exe =
                std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("claustre"));
            vec![
                claustre_exe.to_string_lossy().to_string(),
                "feed-next".to_string(),
                "--session-id".to_string(),
                session.id.clone(),
            ]
        } else {
            // Supervised: launch Claude directly with the prompt
            let prompt = if let Some(subtask) = store.next_pending_subtask(&task.id)? {
                store.update_subtask_status(&subtask.id, TaskStatus::Working)?;
                format!("{}{COMPLETION_INSTRUCTIONS}", subtask.description)
            } else {
                format!("{}{COMPLETION_INSTRUCTIONS}", task.description)
            };
            vec!["claude".to_string(), prompt]
        };

        // Spawn session-host as a detached process and wait for socket
        spawn_session_host(&session.id, &claude_cmd, worktree_str)?;
        let sock = config::session_socket_path(&session.id)?;
        wait_for_socket(&sock, std::time::Duration::from_secs(10))?;
        socket_path = Some(sock);
    }

    Ok(SessionSetup {
        session,
        tab_label,
        socket_path,
        worktree_path: worktree_str.to_string(),
    })
}

/// Tear down a session: remove worktree, update DB.
/// The TUI is responsible for removing the session tab (dropping the PTY handles).
pub fn teardown_session(store: &Store, session_id: &str) -> Result<()> {
    let session = store.get_session(session_id)?;
    let project = store.get_project(&session.project_id)?;
    let repo_path = Path::new(&project.repo_path);

    // Send Shutdown to session-host (if running)
    if let Ok(socket_path) = config::session_socket_path(session_id) {
        if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket_path) {
            let _ = crate::pty::protocol::write_client_message(
                &mut stream,
                &crate::pty::protocol::ClientMessage::Shutdown,
            );
        }
        let _ = fs::remove_file(&socket_path);
    }
    if let Ok(pid_path) = config::session_pid_path(session_id) {
        let _ = fs::remove_file(&pid_path);
    }

    // Capture final git stats
    if let Ok(stats) = get_git_stats(Path::new(&session.worktree_path)) {
        store.update_session_git_stats(
            session_id,
            stats.files_changed,
            stats.lines_added,
            stats.lines_removed,
        )?;
    }

    // Remove worktree
    let _ = remove_worktree(repo_path, Path::new(&session.worktree_path));

    // Clean up progress tmp dir
    if let Ok(progress_dir) = config::session_progress_dir(session_id) {
        let _ = fs::remove_dir_all(progress_dir);
    }

    // Update DB
    store.close_session(session_id)?;

    Ok(())
}

// ── Internal helpers ──

/// Spawn a detached session-host process that owns the PTY.
fn spawn_session_host(session_id: &str, cmd_args: &[String], worktree_path: &str) -> Result<()> {
    let claustre_exe =
        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("claustre"));
    let mut host_cmd = std::process::Command::new(claustre_exe);
    host_cmd.args([
        "session-host",
        "--session-id",
        session_id,
        "--worktree-path",
        worktree_path,
        "--",
    ]);
    host_cmd.args(cmd_args);
    host_cmd.stdin(std::process::Stdio::null());
    host_cmd.stdout(std::process::Stdio::null());
    host_cmd.stderr(std::process::Stdio::null());
    // SAFETY: setsid() creates a new session so the child survives parent exit;
    // no memory-safety implications.
    #[cfg(unix)]
    unsafe {
        host_cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    host_cmd
        .spawn()
        .context("failed to spawn session-host process")?;
    Ok(())
}

/// Poll until the session-host socket file appears on disk.
fn wait_for_socket(path: &Path, timeout: std::time::Duration) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    bail!(
        "session-host socket did not appear within {}s",
        timeout.as_secs()
    )
}

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

    // Fetch latest from origin so the worktree starts from an up-to-date main
    let fetch_output = Command::new("git")
        .args(["-C", repo_str, "fetch", "origin", "main"])
        .output()
        .context("failed to run git fetch origin")?;

    if !fetch_output.status.success() {
        bail!(
            "git fetch origin failed: {}",
            String::from_utf8_lossy(&fetch_output.stderr)
        );
    }

    // Create worktree branching off origin/main
    let output = Command::new("git")
        .args([
            "-C",
            repo_str,
            "worktree",
            "add",
            "-b",
            branch_name,
            wt_str,
            "origin/main",
        ])
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
extract_usage() {
    USAGE_ARGS=""
    local PROJECT_HASH
    PROJECT_HASH=$(printf '%s' "$WORKTREE_ROOT" | sed 's/[^a-zA-Z0-9]/-/g')
    local PROJECT_DIR="$HOME/.claude/projects/$PROJECT_HASH"

    if [ -d "$PROJECT_DIR" ]; then
        local LATEST
        LATEST=$(ls -t "$PROJECT_DIR"/*.jsonl 2>/dev/null | head -1)
        if [ -n "$LATEST" ]; then
            local INPUT_T OUTPUT_T
            read -r INPUT_T OUTPUT_T < <(
                jq -r 'select(.type == "assistant") | .message.usage | [(.input_tokens // 0) + (.cache_creation_input_tokens // 0) + (.cache_read_input_tokens // 0), (.output_tokens // 0)] | @tsv' "$LATEST" 2>/dev/null \
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
    // Primary hook: syncs progress + usage each time Claude completes an internal task.
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
extract_usage

echo "$(date -u +%FT%TZ) task-completed sid=$SESSION_ID usage='$USAGE_ARGS'" >> "$LOG"
claustre session-update --session-id "$SESSION_ID" $USAGE_ARGS 2>> "$LOG"
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

# Check for open PR on current branch only (no fallback to other branches —
# gh pr list would pick up PRs from unrelated sessions and cause cross-session spam)
PR_URL=$(cd "$WORKTREE_ROOT" && gh pr view --json url --jq '.url' 2>/dev/null)

if [ -n "$PR_URL" ]; then
    echo "$(date -u +%FT%TZ) stop sid=$SESSION_ID pr=$PR_URL usage='$USAGE_ARGS'" >> "$LOG"
    claustre session-update --session-id "$SESSION_ID" --pr-url "$PR_URL" $USAGE_ARGS 2>> "$LOG"
else
    echo "$(date -u +%FT%TZ) stop sid=$SESSION_ID no-pr usage='$USAGE_ARGS'" >> "$LOG"
    claustre session-update --session-id "$SESSION_ID" $USAGE_ARGS 2>> "$LOG"
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

fn get_git_stats(worktree_path: &Path) -> Result<GitStats> {
    let wt_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;
    let output = Command::new("git")
        .args(["-C", wt_str, "diff", "--stat", "origin/main"])
        .output()
        .context("failed to run git diff --stat origin/main")?;

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
