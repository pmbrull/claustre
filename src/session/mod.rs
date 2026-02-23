//! Git worktree lifecycle, session setup, and teardown.
//!
//! Creates worktrees, writes merged config and hooks, spawns session-host
//! processes, and cleans up on session completion.

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
pub fn completion_instructions(default_branch: &str) -> String {
    format!(
        "\n\nWhen you finish your task:\n\
        1. Commit all changes with a descriptive commit message\n\
        2. Push the branch: `git push -u origin HEAD`\n\
        3. Create a pull request against `{default_branch}` using `gh pr create`\n\n\
        IMPORTANT: Do NOT include any 'Generated with Claude Code' or similar footer in the PR body."
    )
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
        r#""$@"; exec "${SHELL:-/bin/zsh}" -l"#.to_string(),
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
    /// Path to the session-host's Unix socket (None if no task was assigned).
    pub socket_path: Option<PathBuf>,
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
pub fn create_session(
    store: &Store,
    project_id: &str,
    branch_name: &str,
    task: Option<&Task>,
) -> Result<SessionSetup> {
    let project = store.get_project(project_id)?;
    let repo_path = Path::new(&project.repo_path);
    let mut guard = SessionCleanupGuard::new(store, repo_path);

    // 1. Create the worktree
    let worktree_path = create_worktree(
        repo_path,
        &project.name,
        branch_name,
        &project.default_branch,
    )?;
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

    // 8. Build the Claude command and spawn session-host
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
            let instructions = completion_instructions(&project.default_branch);
            let prompt = if let Some(subtask) = store.next_pending_subtask(&task.id)? {
                store.update_subtask_status(&subtask.id, TaskStatus::Working)?;
                format!("{}{instructions}", subtask.description)
            } else {
                format!("{}{instructions}", task.description)
            };
            vec!["claude".to_string(), prompt]
        };

        // Wrap the command so the PTY drops to a shell after Claude exits
        let claude_cmd = wrap_cmd_with_shell_fallback(claude_cmd);

        // Spawn session-host as a detached process and wait for socket
        spawn_session_host(&session.id, &claude_cmd, worktree_str)?;
        let sock = config::session_socket_path(&session.id)?;
        wait_for_socket(&sock, std::time::Duration::from_secs(10))?;
        socket_path = Some(sock);
    }

    // Success — prevent guard from cleaning up
    guard.disarm();

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
    host_cmd.env("CLAUSTRE_SESSION", "1");
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

fn create_worktree(
    repo_path: &Path,
    project_name: &str,
    branch_name: &str,
    default_branch: &str,
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

    let origin_branch = format!("origin/{default_branch}");

    // Fetch latest from origin so the worktree starts from an up-to-date base
    let fetch_output = Command::new("git")
        .args(["-C", repo_str, "fetch", "origin", default_branch])
        .output()
        .context("failed to run git fetch origin")?;

    if !fetch_output.status.success() {
        bail!(
            "git fetch origin failed: {}",
            String::from_utf8_lossy(&fetch_output.stderr)
        );
    }

    // Create worktree branching off origin/<default_branch>
    let output = Command::new("git")
        .args([
            "-C",
            repo_str,
            "worktree",
            "add",
            "-b",
            branch_name,
            wt_str,
            &origin_branch,
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
        .args(["-C", wt_str, "diff", "--stat", &origin_branch])
        .output()
        .context("failed to run git diff --stat")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_git_stat_summary(&stdout))
}

/// Parse the summary line from `git diff --stat` output.
///
/// Expects a line like `" 3 files changed, 10 insertions(+), 5 deletions(-)"`.
/// Returns zeroes if no summary line is found.
fn parse_git_stat_summary(output: &str) -> GitStats {
    let mut files_changed: i64 = 0;
    let mut lines_added: i64 = 0;
    let mut lines_removed: i64 = 0;

    for line in output.lines() {
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
        let instructions = completion_instructions("develop");
        assert!(instructions.contains("develop"));
        assert!(instructions.contains("gh pr create"));
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

    // ── parse_git_stat_summary ──

    #[test]
    fn parse_git_stat_full() {
        let output = " src/main.rs | 10 +++---\n src/lib.rs  |  5 ++--\n 2 files changed, 8 insertions(+), 7 deletions(-)\n";
        let stats = parse_git_stat_summary(output);
        assert_eq!(stats.files_changed, 2);
        assert_eq!(stats.lines_added, 8);
        assert_eq!(stats.lines_removed, 7);
    }

    #[test]
    fn parse_git_stat_insertions_only() {
        let output = " 1 file changed, 15 insertions(+)\n";
        let stats = parse_git_stat_summary(output);
        assert_eq!(stats.files_changed, 1);
        assert_eq!(stats.lines_added, 15);
        assert_eq!(stats.lines_removed, 0);
    }

    #[test]
    fn parse_git_stat_deletions_only() {
        let output = " 3 files changed, 20 deletions(-)\n";
        let stats = parse_git_stat_summary(output);
        assert_eq!(stats.files_changed, 3);
        assert_eq!(stats.lines_added, 0);
        assert_eq!(stats.lines_removed, 20);
    }

    #[test]
    fn parse_git_stat_empty_output() {
        let stats = parse_git_stat_summary("");
        assert_eq!(stats.files_changed, 0);
        assert_eq!(stats.lines_added, 0);
        assert_eq!(stats.lines_removed, 0);
    }
}
