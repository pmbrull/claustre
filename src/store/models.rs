use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub repo_path: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    InReview,
    Done,
    Error,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::InReview => "in_review",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Pending => "☐",
            Self::InProgress => "●",
            Self::InReview => "◐",
            Self::Done => "✓",
            Self::Error => "✗",
        }
    }

    /// Sort priority for the task queue panel display.
    /// Lower values appear first: `in_review` → error → pending → `in_progress` → done.
    pub fn sort_priority(&self) -> u8 {
        match self {
            Self::InReview => 0,
            Self::Error => 1,
            Self::Pending => 2,
            Self::InProgress => 3,
            Self::Done => 4,
        }
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_progress" => Ok(Self::InProgress),
            "in_review" => Ok(Self::InReview),
            "done" => Ok(Self::Done),
            "error" => Ok(Self::Error),
            _ => Err(format!("unknown task status: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskMode {
    Autonomous,
    Supervised,
}

impl TaskMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Autonomous => "autonomous",
            Self::Supervised => "supervised",
        }
    }
}

impl fmt::Display for TaskMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "autonomous" => Ok(Self::Autonomous),
            "supervised" => Ok(Self::Supervised),
            _ => Err(format!("unknown task mode: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub mode: TaskMode,
    pub session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub sort_order: i64,
    pub pr_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub sort_order: i64,
    pub created_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeProgressItem {
    pub subject: String,
    pub status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeStatus {
    Idle,
    Working,
    Done,
    Error,
}

impl ClaudeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Idle => "○",
            Self::Working => "●",
            Self::Done => "✓",
            Self::Error => "✗",
        }
    }
}

impl fmt::Display for ClaudeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ClaudeStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "idle" => Ok(Self::Idle),
            "working" => Ok(Self::Working),
            "done" => Ok(Self::Done),
            "error" => Ok(Self::Error),
            _ => Err(format!("unknown claude status: {s}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub project_id: String,
    pub branch_name: String,
    pub worktree_path: String,
    pub zellij_tab_name: String,
    pub claude_status: ClaudeStatus,
    pub status_message: String,
    pub last_activity_at: String,
    pub files_changed: i64,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub created_at: String,
    pub closed_at: Option<String>,
    pub claude_progress: Vec<ClaudeProgressItem>,
}

/// Tracks rate limit state. DB-backed fields are loaded from the `rate_limit_state` table.
/// `reset_5h` and `reset_7d` are populated from the API cache at runtime, not stored in DB.
#[derive(Debug, Clone, Default)]
pub struct RateLimitState {
    pub is_rate_limited: bool,
    pub limit_type: Option<String>,
    #[expect(dead_code, reason = "stored for diagnostics/future display")]
    pub rate_limited_at: Option<String>,
    pub reset_at: Option<String>,
    pub usage_5h_pct: Option<f64>,
    pub usage_7d_pct: Option<f64>,
    /// Time until 5h window resets (e.g. "2h30m"), from API
    pub reset_5h: Option<String>,
    /// Time until 7d window resets (e.g. "3d12h"), from API
    pub reset_7d: Option<String>,
    #[expect(dead_code, reason = "stored for diagnostics/future display")]
    pub updated_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_status_round_trip() {
        for status in [
            TaskStatus::Pending,
            TaskStatus::InProgress,
            TaskStatus::InReview,
            TaskStatus::Done,
            TaskStatus::Error,
        ] {
            assert_eq!(status.as_str().parse::<TaskStatus>().unwrap(), status);
            assert_eq!(status.to_string(), status.as_str());
        }
    }

    #[test]
    fn task_status_unknown_returns_error() {
        assert!("nonsense".parse::<TaskStatus>().is_err());
        assert!("".parse::<TaskStatus>().is_err());
    }

    #[test]
    fn task_status_symbols() {
        assert_eq!(TaskStatus::Pending.symbol(), "\u{2610}");
        assert_eq!(TaskStatus::InProgress.symbol(), "\u{25cf}");
        assert_eq!(TaskStatus::InReview.symbol(), "\u{25d0}");
        assert_eq!(TaskStatus::Done.symbol(), "\u{2713}");
        assert_eq!(TaskStatus::Error.symbol(), "\u{2717}");
    }

    #[test]
    fn task_mode_round_trip() {
        for mode in [TaskMode::Autonomous, TaskMode::Supervised] {
            assert_eq!(mode.as_str().parse::<TaskMode>().unwrap(), mode);
            assert_eq!(mode.to_string(), mode.as_str());
        }
    }

    #[test]
    fn task_mode_unknown_returns_error() {
        assert!("nonsense".parse::<TaskMode>().is_err());
        assert!("".parse::<TaskMode>().is_err());
    }

    #[test]
    fn claude_status_round_trip() {
        for status in [
            ClaudeStatus::Idle,
            ClaudeStatus::Working,
            ClaudeStatus::Done,
            ClaudeStatus::Error,
        ] {
            assert_eq!(status.as_str().parse::<ClaudeStatus>().unwrap(), status);
            assert_eq!(status.to_string(), status.as_str());
        }
    }

    #[test]
    fn claude_status_unknown_returns_error() {
        assert!("nonsense".parse::<ClaudeStatus>().is_err());
        assert!("".parse::<ClaudeStatus>().is_err());
    }

    #[test]
    fn claude_status_symbols() {
        assert_eq!(ClaudeStatus::Idle.symbol(), "\u{25cb}");
        assert_eq!(ClaudeStatus::Working.symbol(), "\u{25cf}");
        assert_eq!(ClaudeStatus::Done.symbol(), "\u{2713}");
        assert_eq!(ClaudeStatus::Error.symbol(), "\u{2717}");
    }
}
