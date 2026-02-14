use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize, Clone)]
pub struct Config {
    /// Reserved for future config: custom base directory for worktrees.
    #[expect(dead_code, reason = "deserialized from config.toml but not yet used")]
    #[serde(default)]
    pub worktree_base: Option<String>,
    #[serde(default)]
    pub notifications: NotificationConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NotificationConfig {
    /// Whether voice/sound notifications are enabled. Default: true
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Command to run for notifications. Default: "say" (macOS)
    #[serde(default = "default_notification_command")]
    pub command: String,

    /// Template for the notification message.
    /// {task} is replaced with the task title.
    /// Default: "completed {task}"
    #[serde(default = "default_notification_template")]
    pub template: String,

    /// Voice to use with the say command (macOS). Default: none (system default)
    #[serde(default)]
    pub voice: Option<String>,

    /// Speaking rate for the say command (words per minute). Default: none (system default)
    #[serde(default)]
    pub rate: Option<u32>,
}

impl Default for NotificationConfig {
    fn default() -> Self {
        NotificationConfig {
            enabled: true,
            command: default_notification_command(),
            template: default_notification_template(),
            voice: None,
            rate: None,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_notification_command() -> String {
    "say".to_string()
}

fn default_notification_template() -> String {
    "completed {task}".to_string()
}

impl NotificationConfig {
    /// Fire a notification for a completed task.
    pub fn notify(&self, task_title: &str) {
        if !self.enabled {
            return;
        }

        let message = self.template.replace("{task}", task_title);

        let mut cmd = Command::new(&self.command);

        // If using "say", support voice and rate options
        if self.command == "say" {
            if let Some(ref voice) = self.voice {
                cmd.args(["-v", voice]);
            }
            if let Some(rate) = self.rate {
                cmd.args(["-r", &rate.to_string()]);
            }
        }

        cmd.arg(&message);

        // Fire and forget — don't block the caller
        match cmd.spawn() {
            Ok(_) => {}
            Err(e) => {
                tracing::warn!("notification command failed: {}", e);
            }
        }
    }
}

/// Returns the base claustre config directory: ~/.claustre/
pub fn base_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".claustre"))
}

/// Returns the path to the `SQLite` database
pub fn db_path() -> Result<PathBuf> {
    Ok(base_dir()?.join("claustre.db"))
}

/// Returns the path to the MCP socket
pub fn mcp_socket_path() -> Result<PathBuf> {
    Ok(base_dir()?.join("mcp.sock"))
}

/// Returns the path to the global CLAUDE.md
pub fn global_claude_md_path() -> Result<PathBuf> {
    Ok(base_dir()?.join("claude.md"))
}

/// Returns the path to the global hooks directory
pub fn global_hooks_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("hooks"))
}

/// Returns the path to the worktree base directory
pub fn worktree_base_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("worktrees"))
}

/// Ensure all required directories exist
pub fn ensure_dirs() -> Result<()> {
    let base = base_dir()?;
    fs::create_dir_all(&base).context("failed to create ~/.claustre/")?;
    fs::create_dir_all(global_hooks_dir()?).context("failed to create ~/.claustre/hooks/")?;
    fs::create_dir_all(worktree_base_dir()?).context("failed to create ~/.claustre/worktrees/")?;
    Ok(())
}

/// Load config from ~/.claustre/config.toml (or return defaults if it doesn't exist)
pub fn load() -> Result<Config> {
    let path = base_dir()?.join("config.toml");
    if path.exists() {
        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(config)
    } else {
        Ok(Config::default())
    }
}

/// Merge global and project CLAUDE.md content.
/// Global comes first, project-specific appended after.
pub fn merge_claude_md(project_repo_path: &std::path::Path) -> Result<String> {
    let mut content = String::new();

    let global_path = global_claude_md_path()?;
    if global_path.exists() {
        content.push_str(&fs::read_to_string(&global_path)?);
        content.push_str("\n\n");
    }

    let project_path = project_repo_path.join(".claustre").join("claude.md");
    if project_path.exists() {
        content.push_str(&fs::read_to_string(&project_path)?);
        content.push_str("\n\n");
    }

    // Also include the project's own CLAUDE.md if it exists
    let repo_claude_md = project_repo_path.join("CLAUDE.md");
    if repo_claude_md.exists() {
        content.push_str(&fs::read_to_string(&repo_claude_md)?);
    }

    // Append task completion instructions (MOST IMPORTANT — must come first)
    content.push_str("\n\n## Claustre Task Completion (CRITICAL)\n\n");
    content.push_str("When you finish your task, you MUST follow this sequence:\n\n");
    content.push_str("1. Commit all changes with a descriptive commit message\n");
    content.push_str("2. Push the branch to the remote: `git push -u origin HEAD`\n");
    content.push_str("3. Create a pull request against `main` using `gh pr create`\n");
    content.push_str("4. Call `claustre_task_done` with the PR URL\n\n");
    content.push_str("This is NON-NEGOTIABLE. Without this sequence, your work stays in an isolated worktree with no path back to main.\n\n");
    content.push_str("Call `claustre_task_done` with:\n");
    content.push_str("- `summary`: a brief summary of what you accomplished\n");
    content.push_str("- `pr_url`: the URL of the pull request you created\n\n");

    // Append status reporting instructions
    content.push_str("## Claustre Status Reporting\n\n");
    content.push_str("You MUST call the `claustre_status` tool to keep your session status updated in the claustre dashboard:\n");
    content.push_str("- Call with `state: \"working\"` when you start working on a task or subtask\n");
    content.push_str("- Call with `state: \"waiting_for_input\"` when you need user input or approval\n");
    content.push_str("- Call with `state: \"error\"` if you encounter a blocking error\n");
    content.push_str("- Use the `message` field to briefly describe what you're doing (e.g., \"Implementing auth middleware\")\n");
    content.push_str("- Do NOT call with `state: \"done\"` — use `claustre_task_done` instead when finished\n\n");

    // Append rate limit reporting instructions
    content.push_str("## Claustre Rate Limit Reporting\n\n");
    content.push_str(
        "If you hit a rate limit, immediately call the `claustre_rate_limited` tool with:\n",
    );
    content.push_str("- `limit_type`: \"5h\" or \"7d\"\n");
    content.push_str("- `reset_at`: when the limit resets (ISO 8601), if known\n");
    content.push_str(
        "- `usage_5h_pct` and `usage_7d_pct`: current window usage percentages, if known\n\n",
    );
    content.push_str("Periodically call `claustre_usage_windows` to report your current usage window percentages so the claustre dashboard stays updated.\n");

    Ok(content)
}

/// Merge CLAUDE.md content from explicit paths (for testing without ~/.claustre/).
#[cfg(test)]
fn merge_claude_md_from_paths(
    global_path: Option<&std::path::Path>,
    project_path: Option<&std::path::Path>,
    repo_claude_md: Option<&std::path::Path>,
) -> Result<String> {
    let mut content = String::new();

    if let Some(p) = global_path
        && p.exists()
    {
        content.push_str(&fs::read_to_string(p)?);
        content.push_str("\n\n");
    }

    if let Some(p) = project_path
        && p.exists()
    {
        content.push_str(&fs::read_to_string(p)?);
        content.push_str("\n\n");
    }

    if let Some(p) = repo_claude_md
        && p.exists()
    {
        content.push_str(&fs::read_to_string(p)?);
    }

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn default_config_values() {
        let config = Config::default();
        assert!(config.notifications.enabled);
        assert_eq!(config.notifications.command, "say");
        assert_eq!(config.notifications.template, "completed {task}");
        assert!(config.notifications.voice.is_none());
        assert!(config.notifications.rate.is_none());
    }

    #[test]
    fn merge_claude_md_no_files() {
        let result = merge_claude_md_from_paths(None, None, None).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn merge_claude_md_project_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().join("project.md");
        fs::write(&project_path, "project content").unwrap();

        let result = merge_claude_md_from_paths(None, Some(&project_path), None).unwrap();
        assert_eq!(result, "project content\n\n");
    }

    #[test]
    fn merge_claude_md_repo_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("CLAUDE.md");
        fs::write(&repo_path, "repo content").unwrap();

        let result = merge_claude_md_from_paths(None, None, Some(&repo_path)).unwrap();
        assert_eq!(result, "repo content");
    }

    #[test]
    fn merge_claude_md_order() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.md");
        let project = dir.path().join("project.md");
        let repo = dir.path().join("CLAUDE.md");
        fs::write(&global, "GLOBAL").unwrap();
        fs::write(&project, "PROJECT").unwrap();
        fs::write(&repo, "REPO").unwrap();

        let result =
            merge_claude_md_from_paths(Some(&global), Some(&project), Some(&repo)).unwrap();
        assert_eq!(result, "GLOBAL\n\nPROJECT\n\nREPO");
    }

    #[test]
    fn notification_template_substitution() {
        let config = NotificationConfig {
            enabled: true,
            command: "echo".to_string(),
            template: "task {task} is done".to_string(),
            voice: None,
            rate: None,
        };
        let message = config.template.replace("{task}", "my-task");
        assert_eq!(message, "task my-task is done");
    }
}
