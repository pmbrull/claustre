use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config;
use crate::store::{Session, Store, Task, TaskMode, TaskStatus};

/// Create a full session: worktree, config, Zellij tab, and optionally launch Claude.
pub fn create_session(
    store: &Store,
    project_id: &str,
    branch_name: &str,
    task: Option<&Task>,
) -> Result<Session> {
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

    // 5. Write MCP config with the session ID
    write_mcp_config(&worktree_path, &session.id)?;

    // 6. Launch Claude if autonomous task
    if let Some(task) = task {
        store.assign_task_to_session(&task.id, &session.id)?;
        store.update_task_status(&task.id, TaskStatus::InProgress)?;

        if task.mode == TaskMode::Autonomous {
            launch_claude_in_zellij(&tab_name, &task.description)?;
        }
    }

    Ok(session)
}

/// Tear down a session: close Zellij tab, remove worktree, update DB.
pub fn teardown_session(store: &Store, session_id: &str) -> Result<()> {
    let session = store.get_session(session_id)?;

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
    let _ = remove_worktree(Path::new(&session.worktree_path));

    // Update DB
    store.close_session(session_id)?;

    Ok(())
}

/// Jump to a session's Zellij tab
pub fn goto_session(session: &Session) -> Result<()> {
    Command::new("zellij")
        .args(["action", "go-to-tab-name", &session.zellij_tab_name])
        .status()
        .context("failed to switch Zellij tab")?;
    Ok(())
}

/// Feed the next autonomous task prompt to a session's Zellij pane.
pub fn feed_next_task(store: &Store, session_id: &str) -> Result<bool> {
    if let Some(task) = store.next_pending_task_for_session(session_id)? {
        let session = store.get_session(session_id)?;
        store.update_task_status(&task.id, TaskStatus::InProgress)?;
        launch_claude_in_zellij(&session.zellij_tab_name, &task.description)?;
        Ok(true)
    } else {
        Ok(false)
    }
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

fn remove_worktree(worktree_path: &Path) -> Result<()> {
    let wt_str = worktree_path
        .to_str()
        .context("worktree path contains invalid UTF-8")?;
    Command::new("git")
        .args(["worktree", "remove", "--force", wt_str])
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

fn write_mcp_config(worktree_path: &Path, session_id: &str) -> Result<()> {
    let mcp_config = crate::mcp::mcp_config_json(session_id)?;
    fs::write(
        worktree_path.join(".mcp.json"),
        serde_json::to_string_pretty(&mcp_config)?,
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
    // so we go to the tab first, then close it
    let _ = Command::new("zellij")
        .args(["action", "go-to-tab-name", tab_name])
        .status();
    Command::new("zellij")
        .args(["action", "close-tab"])
        .status()
        .context("failed to close Zellij tab")?;
    Ok(())
}

fn launch_claude_in_zellij(tab_name: &str, prompt: &str) -> Result<()> {
    // First go to the tab
    Command::new("zellij")
        .args(["action", "go-to-tab-name", tab_name])
        .status()
        .context("failed to switch to Zellij tab")?;

    // Write the claude command to the pane
    let cmd = format!("claude --prompt {}\n", shell_escape(prompt));
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
