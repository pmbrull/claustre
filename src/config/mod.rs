//! Configuration loading, path helpers, and notification support.
//!
//! Reads `~/.claustre/config.toml`, provides paths for the database, worktrees,
//! hooks, and sockets, and handles merging global + project `CLAUDE.md` files.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Default, Deserialize, Clone)]
pub struct Config {
    /// Whether to launch Claude sessions with `--remote`. Default: false
    #[serde(default)]
    pub remote_enabled: bool,

    /// Whether to check for updates and auto-update the binary before opening the TUI.
    /// Default: false
    #[serde(default)]
    pub auto_update: bool,

    #[serde(default)]
    pub notifications: NotificationConfig,

    /// Default pane layout for new session tabs.
    /// When absent, uses the default side-by-side layout (shell left, claude right).
    #[serde(default)]
    pub layout: Option<LayoutConfig>,

    /// Custom theme colours. All fields are optional; missing fields keep
    /// their default values.
    #[serde(default)]
    pub theme: crate::tui::theme::ThemeConfig,

    /// Review loop settings (poll interval, prompt template).
    #[serde(default)]
    pub review_loop: ReviewLoopConfig,

    /// Recommended Claude Code permissions for `claustre configure`.
    #[serde(default)]
    pub permissions: RecommendedPermissions,

    /// Claude Code model and effort settings.
    #[serde(default)]
    pub claude: ClaudeConfig,

    /// Sync settings (auto-push on task changes, etc.).
    #[serde(default)]
    pub sync: SyncConfig,

    /// RTK integration settings.
    #[serde(default)]
    pub rtk: RtkConfig,

    /// Sprint board column configuration.
    #[serde(default)]
    pub board: BoardConfig,
}

/// Sprint board column configuration.
///
/// Each column has a name and a list of GitHub issue labels that map
/// issues into that column. The first column acts as the catch-all
/// for open issues without matching labels. The last column catches
/// closed issues.
///
/// ```toml
/// [[board.columns]]
/// name = "Backlog"
/// labels = []
///
/// [[board.columns]]
/// name = "In Progress"
/// labels = ["in progress", "wip"]
///
/// [[board.columns]]
/// name = "In Review"
/// labels = ["in review", "review"]
///
/// [[board.columns]]
/// name = "Done"
/// labels = []
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct BoardConfig {
    /// Column definitions for the sprint board.
    #[serde(default = "default_board_columns")]
    pub columns: Vec<BoardColumn>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BoardColumn {
    /// Display name for the column.
    pub name: String,
    /// GitHub issue labels that place an issue in this column (case-insensitive).
    /// Empty means no label matching — the first column with empty labels is the catch-all.
    #[serde(default)]
    pub labels: Vec<String>,
}

impl Default for BoardConfig {
    fn default() -> Self {
        Self {
            columns: default_board_columns(),
        }
    }
}

impl BoardConfig {
    /// Convert columns to the (name, labels) tuples used by `github::assign_column`.
    pub fn column_labels(&self) -> Vec<(String, Vec<String>)> {
        self.columns
            .iter()
            .map(|c| (c.name.clone(), c.labels.clone()))
            .collect()
    }
}

fn default_board_columns() -> Vec<BoardColumn> {
    vec![
        BoardColumn {
            name: "Backlog".to_string(),
            labels: vec![],
        },
        BoardColumn {
            name: "In Progress".to_string(),
            labels: vec!["in progress".to_string(), "wip".to_string()],
        },
        BoardColumn {
            name: "In Review".to_string(),
            labels: vec!["in review".to_string(), "review".to_string()],
        },
        BoardColumn {
            name: "Done".to_string(),
            labels: vec![],
        },
    ]
}

/// Claude Code model and reasoning effort settings.
///
/// Controls which model and effort level are passed to `claude` via
/// `--model` and `--effort` flags.
///
/// ```toml
/// [claude]
/// model = "claude-opus-4-6"
/// effort = "max"
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct ClaudeConfig {
    /// Model identifier passed to `claude --model`. Default: `"claude-opus-4-6"`
    #[serde(default = "default_claude_model")]
    pub model: String,

    /// Reasoning effort level passed to `claude --effort`. Default: `"max"`
    ///
    /// Valid values: `"min"`, `"low"`, `"medium"`, `"high"`, `"max"`
    #[serde(default = "default_claude_effort")]
    pub effort: String,
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            model: default_claude_model(),
            effort: default_claude_effort(),
        }
    }
}

/// Sync settings for automatic state pushing.
///
/// ```toml
/// [sync]
/// auto_push = true
/// ```
#[derive(Debug, Default, Deserialize, Clone)]
pub struct SyncConfig {
    /// Automatically run `claustre sync push` when tasks are created or updated
    /// via hooks or CLI. Requires `claustre sync init` to have been run first.
    /// Default: false
    #[serde(default)]
    pub auto_push: bool,
}

/// RTK (<https://github.com/rtk-ai/rtk>) integration settings.
///
/// When enabled, `claustre configure` checks that `rtk` is installed and
/// the TUI shows a warning banner if it is missing.
///
/// ```toml
/// [rtk]
/// enabled = true
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct RtkConfig {
    /// Whether RTK integration is enabled. Default: true
    ///
    /// When true, `claustre configure` verifies that the `rtk` CLI is
    /// installed and prompts the user to install it if missing.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for RtkConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

fn default_claude_model() -> String {
    "claude-opus-4-6".to_string()
}

fn default_claude_effort() -> String {
    "max".to_string()
}

/// Recommended Claude Code permission rules applied by `claustre configure`.
///
/// These live in `config.toml` so users can customise what the wizard
/// recommends without touching Rust code.
///
/// ```toml
/// [permissions]
/// allow = ["Bash", "Read(*)"]
/// deny  = ["Bash(git push --force*)"]
/// ask   = ["Bash(rm:*)"]
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct RecommendedPermissions {
    /// Permissions Claude can execute without asking.
    #[serde(default = "default_allow")]
    pub allow: Vec<String>,

    /// Permissions that are always denied.
    #[serde(default = "default_deny")]
    pub deny: Vec<String>,

    /// Permissions that trigger an interactive confirmation.
    #[serde(default = "default_ask")]
    pub ask: Vec<String>,
}

impl Default for RecommendedPermissions {
    fn default() -> Self {
        Self {
            allow: default_allow(),
            deny: default_deny(),
            ask: default_ask(),
        }
    }
}

fn default_allow() -> Vec<String> {
    [
        "Bash",
        "Glob(*)",
        "Grep(*)",
        "Read(*)",
        "Edit(*)",
        "MultiEdit(*)",
        "Write(*)",
        "NotebookEdit(*)",
        "TodoWrite(*)",
        "BashOutput(*)",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

fn default_deny() -> Vec<String> {
    [
        "Bash(git push*main*)",
        "Bash(git push --force*)",
        "Bash(git push*--force*)",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

fn default_ask() -> Vec<String> {
    ["Bash(rm:*)"].iter().map(|s| (*s).to_string()).collect()
}

/// Describes a pane layout tree for session terminals.
///
/// Each leaf is a terminal pane (`"shell"` or `"claude"`).
/// Splits divide space between two children.
///
/// # Example config.toml
///
/// ```toml
/// # Default: shell left, claude right (50/50)
/// [layout]
/// direction = "horizontal"
/// ratio = 50
///
/// [layout.first]
/// pane = "shell"
///
/// [layout.second]
/// pane = "claude"
/// ```
///
/// ```toml
/// # Three panes: shell left, claude top-right, shell bottom-right
/// [layout]
/// direction = "horizontal"
/// ratio = 50
///
/// [layout.first]
/// pane = "shell"
///
/// [layout.second]
/// direction = "vertical"
/// ratio = 70
///
/// [layout.second.first]
/// pane = "claude"
///
/// [layout.second.second]
/// pane = "shell"
/// ```
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum LayoutConfig {
    /// A leaf pane: `{ pane = "shell" }` or `{ pane = "claude" }`.
    Pane { pane: String },
    /// A split between two children.
    Split {
        /// `"horizontal"` (side by side) or `"vertical"` (stacked).
        direction: String,
        /// Percentage of space for the first child (1–99). Default: 50.
        ratio: Option<u16>,
        first: Box<LayoutConfig>,
        second: Box<LayoutConfig>,
    },
}

#[derive(Debug, Deserialize, Clone)]
pub struct NotificationConfig {
    /// Whether voice/sound notifications are enabled. Default: true
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Whether macOS system banner notifications are enabled. Default: true
    #[serde(default = "default_true")]
    pub system: bool,

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

#[derive(Debug, Deserialize, Clone)]
pub struct ReviewLoopConfig {
    /// Poll interval in seconds between review loop checks. Default: 120
    #[serde(default = "default_review_loop_interval")]
    pub poll_interval_secs: u64,

    /// Custom prompt template for the review loop. When set, replaces the
    /// built-in prompt entirely.
    #[serde(default)]
    pub prompt: Option<String>,
}

impl Default for ReviewLoopConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: 120,
            prompt: None,
        }
    }
}

fn default_review_loop_interval() -> u64 {
    120
}

impl Default for NotificationConfig {
    fn default() -> Self {
        NotificationConfig {
            enabled: true,
            system: true,
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

/// Embedded notification icon (keyboard-key "C").
const NOTIFICATION_ICON: &[u8] = include_bytes!("../../assets/claustre-icon.png");

impl NotificationConfig {
    /// Fire a notification for a completed task.
    /// Sends both the voice command and a macOS system banner (if enabled).
    /// If `pr_url` is provided, clicking the system notification opens the PR in a browser.
    /// Otherwise, clicking brings the terminal app to the foreground.
    pub fn notify(&self, task_title: &str, pr_url: Option<&str>) {
        let message = self.template.replace("{task}", task_title);

        if self.enabled {
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

            match cmd.spawn() {
                Ok(mut child) => {
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                }
                Err(e) => {
                    tracing::warn!("notification command failed: {e}");
                }
            }
        }

        if self.system {
            Self::system_notify(task_title, &message, pr_url);
        }
    }

    /// Ensure the notification icon is written to disk and return its path.
    fn ensure_icon() -> Option<PathBuf> {
        let path = base_dir().ok()?.join("claustre-icon.png");
        if !path.exists()
            && let Err(e) = fs::write(&path, NOTIFICATION_ICON)
        {
            tracing::warn!("failed to write notification icon: {e}");
            return None;
        }
        Some(path)
    }

    /// Send a macOS system banner notification.
    /// Tries `terminal-notifier` first (supports custom icons and click actions),
    /// falls back to `osascript`.
    ///
    /// When `pr_url` is provided, clicking the notification opens the PR in a browser.
    /// Otherwise, clicking brings the terminal app to the foreground.
    fn system_notify(task_title: &str, message: &str, pr_url: Option<&str>) {
        let icon_path = Self::ensure_icon();

        // Try terminal-notifier first (supports custom app icon + click actions)
        if let Some(ref icon) = icon_path {
            let icon_str = icon.display().to_string();
            let mut args = vec![
                "-title",
                "claustre",
                "-subtitle",
                task_title,
                "-message",
                message,
                "-appIcon",
                &icon_str,
            ];

            // Click action: open PR URL or bring terminal to foreground
            let bundle_id;
            if let Some(url) = pr_url {
                args.extend(["-open", url]);
            } else {
                bundle_id = Self::detect_terminal_bundle_id();
                args.extend(["-activate", &bundle_id]);
            }

            let result = Command::new("terminal-notifier").args(&args).spawn();

            if result.is_ok() {
                return;
            }
        }

        // Fall back to osascript (no click-action support — banner only)
        let script = format!(
            "display notification \"{}\" with title \"claustre\" subtitle \"{}\"",
            message.replace('\\', "\\\\").replace('"', "\\\""),
            task_title.replace('\\', "\\\\").replace('"', "\\\""),
        );

        if let Err(e) = Command::new("osascript").args(["-e", &script]).spawn() {
            tracing::warn!("system notification failed: {e}");
        }
    }

    /// Detect the bundle identifier of the terminal application.
    /// Uses `TERM_PROGRAM` env var and maps to known bundle IDs.
    /// Falls back to `com.apple.Terminal` if the terminal is unrecognized or unset.
    fn detect_terminal_bundle_id() -> String {
        if let Ok(term) = std::env::var("TERM_PROGRAM") {
            let bundle = match term.as_str() {
                "iTerm.app" => "com.googlecode.iterm2",
                "kitty" => "net.kovidgoyal.kitty",
                "Alacritty" => "org.alacritty",
                "WezTerm" => "com.github.wez.wezterm",
                "ghostty" => "com.mitchellh.ghostty",
                _ => "com.apple.Terminal",
            };
            return bundle.to_string();
        }

        "com.apple.Terminal".to_string()
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

/// Returns the path to the tmp progress directory for a session
pub fn session_progress_dir(session_id: &str) -> Result<PathBuf> {
    Ok(base_dir()?.join("tmp").join(session_id))
}

/// Returns the path to the progress.json file for a session
pub fn session_progress_file(session_id: &str) -> Result<PathBuf> {
    Ok(session_progress_dir(session_id)?.join("progress.json"))
}

/// Returns the directory for session-host Unix sockets
pub fn sockets_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("sockets"))
}

/// Returns the Unix socket path for a session host
pub fn session_socket_path(session_id: &str) -> Result<PathBuf> {
    Ok(sockets_dir()?.join(format!("{session_id}.sock")))
}

/// Returns the directory for session-host PID files
pub fn pids_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("pids"))
}

/// Returns the sync git repo directory: ~/.claustre/sync/
pub fn sync_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("sync"))
}

/// Returns the PID file path for a session host
pub fn session_pid_path(session_id: &str) -> Result<PathBuf> {
    Ok(pids_dir()?.join(format!("{session_id}.pid")))
}

/// Remove stale socket and PID files for sessions whose host process is no longer running.
pub fn cleanup_stale_sockets() -> Result<()> {
    let sockets = sockets_dir()?;
    if !sockets.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(&sockets)?.flatten() {
        let sock_path = entry.path();
        let Some(session_id) = sock_path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        let pid_path = session_pid_path(session_id)?;
        let is_alive = if let Ok(content) = fs::read_to_string(&pid_path) {
            if let Ok(pid) = content.trim().parse::<i32>() {
                // SAFETY: kill(pid, 0) checks if a process exists without sending a signal.
                unsafe { libc::kill(pid, 0) == 0 }
            } else {
                false
            }
        } else {
            // No PID file — check if socket is connectable
            std::os::unix::net::UnixStream::connect(&sock_path).is_ok()
        };

        if !is_alive {
            let _ = fs::remove_file(&sock_path);
            let _ = fs::remove_file(&pid_path);
        }
    }

    Ok(())
}

/// Ensure all required directories exist
pub fn ensure_dirs() -> Result<()> {
    let base = base_dir()?;
    fs::create_dir_all(&base).context("failed to create ~/.claustre/")?;
    fs::create_dir_all(global_hooks_dir()?).context("failed to create ~/.claustre/hooks/")?;
    fs::create_dir_all(worktree_base_dir()?).context("failed to create ~/.claustre/worktrees/")?;
    fs::create_dir_all(base_dir()?.join("tmp")).context("failed to create ~/.claustre/tmp/")?;
    fs::create_dir_all(sockets_dir()?).context("failed to create ~/.claustre/sockets/")?;
    fs::create_dir_all(pids_dir()?).context("failed to create ~/.claustre/pids/")?;
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

/// Auto-detect the default branch of a git repo by querying `origin`.
/// Falls back to `"main"` if detection fails.
pub fn detect_default_branch(repo_path: &str) -> String {
    let output = Command::new("git")
        .args(["-C", repo_path, "remote", "show", "origin"])
        .output();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if let Some(branch) = trimmed.strip_prefix("HEAD branch:") {
                return branch.trim().to_string();
            }
        }
    }

    "main".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn default_config_values() {
        let config = Config::default();
        assert!(!config.remote_enabled);
        assert!(!config.auto_update);
        assert!(config.notifications.enabled);
        assert!(config.notifications.system);
        assert_eq!(config.notifications.command, "say");
        assert_eq!(config.notifications.template, "completed {task}");
        assert!(config.notifications.voice.is_none());
        assert!(config.notifications.rate.is_none());
        assert!(!config.sync.auto_push);
        assert!(config.rtk.enabled);
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
    fn default_layout_is_none() {
        let config = Config::default();
        assert!(config.layout.is_none());
    }

    #[test]
    fn parse_simple_layout() {
        let toml_str = r#"
[layout]
direction = "horizontal"
ratio = 50

[layout.first]
pane = "shell"

[layout.second]
pane = "claude"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let layout = config.layout.unwrap();
        match layout {
            LayoutConfig::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                assert_eq!(direction, "horizontal");
                assert_eq!(ratio, Some(50));
                assert!(matches!(*first, LayoutConfig::Pane { ref pane } if pane == "shell"));
                assert!(matches!(*second, LayoutConfig::Pane { ref pane } if pane == "claude"));
            }
            LayoutConfig::Pane { .. } => panic!("expected Split"),
        }
    }

    #[test]
    fn parse_nested_layout() {
        let toml_str = r#"
[layout]
direction = "horizontal"
ratio = 40

[layout.first]
pane = "shell"

[layout.second]
direction = "vertical"
ratio = 70

[layout.second.first]
pane = "claude"

[layout.second.second]
pane = "shell"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let layout = config.layout.unwrap();
        match layout {
            LayoutConfig::Split {
                direction,
                ratio,
                second,
                ..
            } => {
                assert_eq!(direction, "horizontal");
                assert_eq!(ratio, Some(40));
                match *second {
                    LayoutConfig::Split {
                        direction: inner_dir,
                        ratio: inner_ratio,
                        ..
                    } => {
                        assert_eq!(inner_dir, "vertical");
                        assert_eq!(inner_ratio, Some(70));
                    }
                    LayoutConfig::Pane { .. } => panic!("expected nested Split"),
                }
            }
            LayoutConfig::Pane { .. } => panic!("expected Split"),
        }
    }

    #[test]
    fn parse_layout_default_ratio() {
        let toml_str = r#"
[layout]
direction = "vertical"

[layout.first]
pane = "shell"

[layout.second]
pane = "claude"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let layout = config.layout.unwrap();
        match layout {
            LayoutConfig::Split { ratio, .. } => {
                assert!(ratio.is_none()); // defaults to 50 at runtime
            }
            LayoutConfig::Pane { .. } => panic!("expected Split"),
        }
    }

    #[test]
    fn notification_template_substitution() {
        let config = NotificationConfig {
            enabled: true,
            system: false,
            command: "echo".to_string(),
            template: "task {task} is done".to_string(),
            voice: None,
            rate: None,
        };
        let message = config.template.replace("{task}", "my-task");
        assert_eq!(message, "task my-task is done");
    }

    #[test]
    fn path_helpers_build_expected_paths() {
        let base = base_dir().unwrap();
        let home = dirs::home_dir().unwrap();
        assert_eq!(base, home.join(".claustre"));

        assert_eq!(db_path().unwrap(), base.join("claustre.db"));
        assert_eq!(global_claude_md_path().unwrap(), base.join("claude.md"));
        assert_eq!(global_hooks_dir().unwrap(), base.join("hooks"));
        assert_eq!(worktree_base_dir().unwrap(), base.join("worktrees"));
        assert_eq!(sockets_dir().unwrap(), base.join("sockets"));
        assert_eq!(pids_dir().unwrap(), base.join("pids"));
    }

    #[test]
    fn session_path_helpers() {
        let base = base_dir().unwrap();
        let sid = "test-session-123";

        assert_eq!(
            session_progress_dir(sid).unwrap(),
            base.join("tmp").join(sid)
        );
        assert_eq!(
            session_progress_file(sid).unwrap(),
            base.join("tmp").join(sid).join("progress.json")
        );
        assert_eq!(
            session_socket_path(sid).unwrap(),
            base.join("sockets").join("test-session-123.sock")
        );
        assert_eq!(
            session_pid_path(sid).unwrap(),
            base.join("pids").join("test-session-123.pid")
        );
    }

    #[test]
    fn load_returns_defaults_without_config_file() {
        // This relies on the test environment not having a config file at base_dir,
        // which is fine since base_dir() points to the real ~/.claustre/ and
        // if config.toml doesn't exist it should return defaults.
        // If it does exist, we just verify it parses without error.
        let config = load().unwrap();
        // Regardless of whether file exists, these defaults should be set
        assert!(!config.notifications.command.is_empty());
    }

    #[test]
    fn parse_config_remote_enabled_false() {
        let toml_str = "remote_enabled = false\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.remote_enabled);
    }

    #[test]
    fn parse_config_remote_enabled_defaults_false() {
        let toml_str = "";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.remote_enabled);
    }

    #[test]
    fn default_review_loop_config() {
        let config = Config::default();
        assert_eq!(config.review_loop.poll_interval_secs, 120);
        assert!(config.review_loop.prompt.is_none());
    }

    #[test]
    fn parse_review_loop_config() {
        let toml_str = r#"
[review_loop]
poll_interval_secs = 60
prompt = "Check PR comments and fix issues"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.review_loop.poll_interval_secs, 60);
        assert_eq!(
            config.review_loop.prompt.as_deref(),
            Some("Check PR comments and fix issues")
        );
    }

    #[test]
    fn parse_review_loop_partial_config() {
        let toml_str = r"
[review_loop]
poll_interval_secs = 300
";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.review_loop.poll_interval_secs, 300);
        assert!(config.review_loop.prompt.is_none());
    }

    #[test]
    fn parse_config_auto_update_enabled() {
        let toml_str = "auto_update = true\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.auto_update);
    }

    #[test]
    fn parse_config_auto_update_defaults_false() {
        let toml_str = "";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.auto_update);
    }

    #[test]
    fn parse_config_with_notifications() {
        let toml_str = r#"
[notifications]
enabled = false
system = false
command = "echo"
template = "done: {task}"
voice = "Samantha"
rate = 200
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.notifications.enabled);
        assert!(!config.notifications.system);
        assert_eq!(config.notifications.command, "echo");
        assert_eq!(config.notifications.template, "done: {task}");
        assert_eq!(config.notifications.voice.as_deref(), Some("Samantha"));
        assert_eq!(config.notifications.rate, Some(200));
    }

    // ── Permissions config parsing ──

    #[test]
    fn default_permissions_have_expected_entries() {
        let perms = RecommendedPermissions::default();
        assert!(perms.allow.contains(&"Bash".to_string()));
        assert!(perms.allow.contains(&"Read(*)".to_string()));
        assert!(perms.allow.contains(&"Edit(*)".to_string()));
        assert!(perms.allow.contains(&"Write(*)".to_string()));
        assert!(perms.deny.iter().any(|d| d.contains("push --force")));
        assert!(perms.deny.iter().any(|d| d.contains("push*main")));
        assert!(perms.ask.contains(&"Bash(rm:*)".to_string()));
    }

    #[test]
    fn parse_custom_permissions() {
        let toml_str = r#"
[permissions]
allow = ["Bash", "Read(*)"]
deny = ["Bash(rm -rf /*)"]
ask = ["Bash(git push*)"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.permissions.allow, vec!["Bash", "Read(*)"]);
        assert_eq!(config.permissions.deny, vec!["Bash(rm -rf /*)"]);
        assert_eq!(config.permissions.ask, vec!["Bash(git push*)"]);
    }

    #[test]
    fn parse_empty_config_uses_default_permissions() {
        let config: Config = toml::from_str("").unwrap();
        let defaults = RecommendedPermissions::default();
        assert_eq!(config.permissions.allow, defaults.allow);
        assert_eq!(config.permissions.deny, defaults.deny);
        assert_eq!(config.permissions.ask, defaults.ask);
    }

    // ── Terminal bundle ID detection ──

    #[test]
    fn detect_terminal_bundle_id_known_terminals() {
        // We can't set env vars in a test-safe way without affecting other tests,
        // but we can at least verify the function returns a non-empty string
        let bundle = NotificationConfig::detect_terminal_bundle_id();
        assert!(!bundle.is_empty());
        assert!(
            bundle.contains('.'),
            "bundle ID should have dot-separated segments"
        );
    }

    // ── Full config round-trip ──

    #[test]
    fn parse_full_config() {
        let toml_str = r#"
remote_enabled = true
auto_update = true

[notifications]
enabled = true
system = false
command = "afplay"
template = "task {task} finished"

[review_loop]
poll_interval_secs = 90
prompt = "Review carefully"

[permissions]
allow = ["Bash"]
deny = []
ask = []

[claude]
model = "claude-sonnet-4-6"
effort = "high"

[sync]
auto_push = true

[rtk]
enabled = false

[layout]
direction = "vertical"
ratio = 60

[layout.first]
pane = "claude"

[layout.second]
pane = "shell"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.remote_enabled);
        assert!(config.auto_update);
        assert!(config.notifications.enabled);
        assert!(!config.notifications.system);
        assert_eq!(config.notifications.command, "afplay");
        assert_eq!(config.review_loop.poll_interval_secs, 90);
        assert_eq!(
            config.review_loop.prompt.as_deref(),
            Some("Review carefully")
        );
        assert_eq!(config.permissions.allow, vec!["Bash"]);
        assert!(config.permissions.deny.is_empty());
        assert!(config.layout.is_some());
        assert_eq!(config.claude.model, "claude-sonnet-4-6");
        assert_eq!(config.claude.effort, "high");
        assert!(config.sync.auto_push);
        assert!(!config.rtk.enabled);
    }

    // ── Sync config ──

    #[test]
    fn default_sync_config() {
        let config = Config::default();
        assert!(!config.sync.auto_push);
    }

    #[test]
    fn parse_sync_config_auto_push_enabled() {
        let toml_str = r"
[sync]
auto_push = true
";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.sync.auto_push);
    }

    #[test]
    fn parse_empty_config_uses_default_sync() {
        let config: Config = toml::from_str("").unwrap();
        assert!(!config.sync.auto_push);
    }

    // ── Claude config ──

    #[test]
    fn default_claude_config() {
        let config = Config::default();
        assert_eq!(config.claude.model, "claude-opus-4-6");
        assert_eq!(config.claude.effort, "max");
    }

    #[test]
    fn parse_claude_config_model_only() {
        let toml_str = r#"
[claude]
model = "claude-sonnet-4-6"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.claude.model, "claude-sonnet-4-6");
        assert_eq!(config.claude.effort, "max"); // default
    }

    #[test]
    fn parse_claude_config_effort_only() {
        let toml_str = r#"
[claude]
effort = "low"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.claude.model, "claude-opus-4-6"); // default
        assert_eq!(config.claude.effort, "low");
    }

    #[test]
    fn parse_empty_config_uses_default_claude() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.claude.model, "claude-opus-4-6");
        assert_eq!(config.claude.effort, "max");
    }

    // ── RTK config ──

    #[test]
    fn default_rtk_config_enabled() {
        let config = Config::default();
        assert!(config.rtk.enabled);
    }

    #[test]
    fn parse_rtk_config_disabled() {
        let toml_str = r"
[rtk]
enabled = false
";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.rtk.enabled);
    }

    #[test]
    fn parse_empty_config_uses_default_rtk() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.rtk.enabled);
    }

    // ── Board config ──

    #[test]
    fn default_board_config() {
        let config = Config::default();
        assert_eq!(config.board.columns.len(), 4);
        assert_eq!(config.board.columns[0].name, "Backlog");
        assert_eq!(config.board.columns[1].name, "In Progress");
        assert_eq!(config.board.columns[2].name, "In Review");
        assert_eq!(config.board.columns[3].name, "Done");
    }

    #[test]
    fn parse_custom_board_columns() {
        let toml_str = r#"
[[board.columns]]
name = "Todo"
labels = []

[[board.columns]]
name = "Doing"
labels = ["doing", "started"]

[[board.columns]]
name = "Review"
labels = ["needs review"]

[[board.columns]]
name = "Complete"
labels = []
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.board.columns.len(), 4);
        assert_eq!(config.board.columns[0].name, "Todo");
        assert_eq!(config.board.columns[1].name, "Doing");
        assert_eq!(config.board.columns[1].labels, vec!["doing", "started"]);
    }

    #[test]
    fn parse_empty_config_uses_default_board() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.board.columns.len(), 4);
    }

    #[test]
    fn board_column_labels_helper() {
        let config = Config::default();
        let labels = config.board.column_labels();
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[0].0, "Backlog");
        assert!(labels[0].1.is_empty());
        assert_eq!(labels[1].0, "In Progress");
        assert_eq!(labels[1].1, vec!["in progress", "wip"]);
    }
}
