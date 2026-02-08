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
        let config: Config =
            toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
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

    Ok(content)
}
