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
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::InReview => "in_review",
            TaskStatus::Done => "done",
            TaskStatus::Error => "error",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "in_progress" => TaskStatus::InProgress,
            "in_review" => TaskStatus::InReview,
            "done" => TaskStatus::Done,
            "error" => TaskStatus::Error,
            _ => TaskStatus::Pending,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            TaskStatus::Pending => "☐",
            TaskStatus::InProgress => "●",
            TaskStatus::InReview => "◐",
            TaskStatus::Done => "✓",
            TaskStatus::Error => "✗",
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
            TaskMode::Autonomous => "autonomous",
            TaskMode::Supervised => "supervised",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "autonomous" => TaskMode::Autonomous,
            _ => TaskMode::Supervised,
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
    pub cost: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeStatus {
    Idle,
    Working,
    WaitingForInput,
    Done,
    Error,
}

impl ClaudeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClaudeStatus::Idle => "idle",
            ClaudeStatus::Working => "working",
            ClaudeStatus::WaitingForInput => "waiting_for_input",
            ClaudeStatus::Done => "done",
            ClaudeStatus::Error => "error",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "working" => ClaudeStatus::Working,
            "waiting_for_input" => ClaudeStatus::WaitingForInput,
            "done" => ClaudeStatus::Done,
            "error" => ClaudeStatus::Error,
            _ => ClaudeStatus::Idle,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            ClaudeStatus::Idle => "○",
            ClaudeStatus::Working => "●",
            ClaudeStatus::WaitingForInput => "◐",
            ClaudeStatus::Done => "✓",
            ClaudeStatus::Error => "✗",
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
}

#[derive(Debug, Clone)]
pub struct RateLimitState {
    pub is_rate_limited: bool,
    pub limit_type: Option<String>,
    #[expect(dead_code, reason = "stored for diagnostics/future display")]
    pub rate_limited_at: Option<String>,
    pub reset_at: Option<String>,
    pub usage_5h_pct: f64,
    pub usage_7d_pct: f64,
    #[expect(dead_code, reason = "stored for diagnostics/future display")]
    pub updated_at: String,
}

impl Default for RateLimitState {
    fn default() -> Self {
        RateLimitState {
            is_rate_limited: false,
            limit_type: None,
            rate_limited_at: None,
            reset_at: None,
            usage_5h_pct: 0.0,
            usage_7d_pct: 0.0,
            updated_at: String::new(),
        }
    }
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
            assert_eq!(TaskStatus::from_str(status.as_str()), status);
        }
    }

    #[test]
    fn task_status_unknown_defaults_to_pending() {
        assert_eq!(TaskStatus::from_str("nonsense"), TaskStatus::Pending);
        assert_eq!(TaskStatus::from_str(""), TaskStatus::Pending);
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
            assert_eq!(TaskMode::from_str(mode.as_str()), mode);
        }
    }

    #[test]
    fn task_mode_unknown_defaults_to_supervised() {
        assert_eq!(TaskMode::from_str("nonsense"), TaskMode::Supervised);
        assert_eq!(TaskMode::from_str(""), TaskMode::Supervised);
    }

    #[test]
    fn claude_status_round_trip() {
        for status in [
            ClaudeStatus::Idle,
            ClaudeStatus::Working,
            ClaudeStatus::WaitingForInput,
            ClaudeStatus::Done,
            ClaudeStatus::Error,
        ] {
            assert_eq!(ClaudeStatus::from_str(status.as_str()), status);
        }
    }

    #[test]
    fn claude_status_unknown_defaults_to_idle() {
        assert_eq!(ClaudeStatus::from_str("nonsense"), ClaudeStatus::Idle);
        assert_eq!(ClaudeStatus::from_str(""), ClaudeStatus::Idle);
    }

    #[test]
    fn claude_status_symbols() {
        assert_eq!(ClaudeStatus::Idle.symbol(), "\u{25cb}");
        assert_eq!(ClaudeStatus::Working.symbol(), "\u{25cf}");
        assert_eq!(ClaudeStatus::WaitingForInput.symbol(), "\u{25d0}");
        assert_eq!(ClaudeStatus::Done.symbol(), "\u{2713}");
        assert_eq!(ClaudeStatus::Error.symbol(), "\u{2717}");
    }
}
