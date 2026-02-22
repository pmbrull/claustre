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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn default_config_values() {
        let config = Config::default();
        assert!(config.notifications.enabled);
        assert!(config.notifications.system);
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
}
