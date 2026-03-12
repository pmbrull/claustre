//! TUI application state and event handling.
//!
//! Contains the `App` struct (all mutable state), key/mouse handlers,
//! data refresh logic, and background task coordination.

mod data_refresh;
mod event_loop;
mod initialization;
mod input;
mod polling;
mod pty_management;
mod session_lifecycle;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::layout::Rect;
use ratatui::widgets::ListState;

use crate::pty::SessionTerminals;
use crate::store::{Project, ProjectStats, Session, Store, Task, TaskStatus, TaskStatusCounts};

/// How long toast notifications remain visible.
const TOAST_DURATION: Duration = Duration::from_secs(4);

/// Tick rate when viewing the dashboard.
///
/// 200 ms keeps background session PTY output reasonably current (5× per
/// second) while staying light on CPU.  The old 1 s rate meant sessions
/// could accumulate up to 1 second of unprocessed output, causing a visible
/// catch-up lag when the user switched to a session tab.
const DASHBOARD_TICK: Duration = Duration::from_millis(200);
/// Tick rate when viewing a session tab (fast refresh for smooth PTY rendering).
const SESSION_TICK: Duration = Duration::from_millis(16);
/// How often to run the slow-path tick work (DB refresh, PR polling, etc.).
/// Applies on all tabs since the dashboard tick rate (200 ms) is now faster
/// than the desired refresh interval.
const SLOW_TICK: Duration = Duration::from_secs(1);

/// A tab in the TUI — either the main dashboard or a session terminal.
pub(crate) enum Tab {
    Dashboard,
    Session {
        session_id: String,
        terminals: Box<SessionTerminals>,
        label: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Projects,
    Tasks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToastStyle {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Normal,
    NewTask,
    EditTask,
    NewProject,
    ConfirmDelete,
    CommandPalette,
    SkillPanel,
    SkillSearch,
    SkillAdd,
    HelpOverlay,
    TaskFilter,
    SubtaskPanel,
    TaskDetails,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeleteTarget {
    Project,
    Task,
}

#[derive(Debug, Clone)]
pub(crate) struct PaletteItem {
    pub label: String,
    pub action: PaletteAction,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum PaletteAction {
    NewTask,
    AddProject,
    RemoveProject,
    FocusProjects,
    FocusTasks,
    FindSkills,
    UpdateSkills,
    Quit,
}

/// Pre-fetched per-project summary for the sidebar (avoids DB queries during rendering).
#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectSummary {
    pub active_sessions: Vec<Session>,
    pub task_counts: TaskStatusCounts,
    pub default_branch: String,
}

/// Result from a background session create/teardown.
enum SessionOpResult {
    /// Session created successfully — carry the setup info for PTY spawning.
    Created(Box<crate::session::SessionSetup>),
    /// Session created but no task to launch (e.g. bare session).
    CreatedNoTask { message: String },
    /// Teardown completed.
    TornDown { message: String },
    /// An operation failed.
    Error { message: String },
}

/// What the GitHub API reports about a PR's state.
enum PrStatus {
    Merged,
    Conflicting,
    CiFailed,
    CiRunning,
    CiPassed,
    Open,
}

/// Result from a background PR status check.
enum PrPollResult {
    /// PR was merged — task should be marked done.
    Merged {
        task_id: String,
        session_id: Option<String>,
        task_title: String,
    },
    /// PR has merge conflicts — task should transition to conflict.
    Conflict { task_id: String, task_title: String },
    /// Previously conflicting PR is now mergeable — task goes back to `in_review`.
    ConflictResolved { task_id: String, task_title: String },
    /// PR has failed CI checks — task should transition to `ci_failed`.
    CiFailed { task_id: String, task_title: String },
    /// Previously failed CI checks are now passing — task goes back to `in_review`.
    CiRecovered { task_id: String, task_title: String },
    /// CI status changed (running or passed) — update the `ci_status` field without changing task status.
    CiStatusChanged {
        task_id: String,
        ci_status: crate::store::CiStatus,
    },
}

/// Result from a background git diff --stat check.
struct GitStatsResult {
    session_id: String,
    files_changed: i64,
    lines_added: i64,
    lines_removed: i64,
}

pub(crate) struct App {
    pub store: Store,
    pub config: crate::config::Config,
    pub theme: super::theme::Theme,
    pub keymap: super::keymap::KeyMap,
    pub should_quit: bool,
    pub focus: Focus,
    pub input_mode: InputMode,

    // Tab system (tab 0 = Dashboard, additional tabs = session terminals)
    pub tabs: Vec<Tab>,
    pub active_tab: usize,

    // Data
    pub projects: Vec<Project>,
    pub sessions: Vec<Session>,
    pub tasks: Vec<Task>,

    // Pre-fetched sidebar data (project_id -> summary)
    pub project_summaries: HashMap<String, ProjectSummary>,

    // Cached stats for the selected project (avoids DB queries during rendering)
    pub project_stats: Option<ProjectStats>,

    // Selection indices
    pub project_index: usize,
    pub task_index: usize,

    // Scroll state for task list (used by ratatui's stateful List widget)
    pub task_list_state: ListState,

    // Input buffer for new task creation
    pub input_buffer: String,
    // Cursor byte-offset within input_buffer (clamped to buf.len())
    pub input_cursor: usize,

    // Enhanced task form state (field 0=prompt, 1=mode, 2=base, 3=branch, 4=push_mode, 5=review_loop, 6=subtasks)
    pub new_task_field: u8,
    pub new_task_description: String,
    pub new_task_mode: crate::store::TaskMode,
    pub new_task_base: String,
    pub new_task_branch: String,
    pub new_task_push_mode: crate::store::PushMode,
    pub new_task_review_loop: bool,

    // Add Project form state
    pub new_project_field: u8,
    pub new_project_name: String,
    pub new_project_path: String,

    // Path autocomplete state
    pub path_suggestions: Vec<String>,
    pub path_suggestion_index: usize,
    pub show_path_suggestions: bool,

    // Confirm delete state
    pub confirm_target: String,
    pub confirm_entity_id: String,
    pub confirm_delete_kind: DeleteTarget,

    // Editing task state
    pub editing_task_id: Option<String>,

    // Task filter state
    pub task_filter: String,
    pub task_filter_cursor: usize,

    // Subtask state
    pub subtasks: Vec<crate::store::Subtask>,
    pub subtask_index: usize,
    pub subtask_counts: HashMap<String, (i64, i64)>,

    // Task details panel scroll offset
    pub task_details_scroll: u16,

    // Inline subtasks for new-task form
    pub new_task_subtasks: Vec<String>,
    pub new_task_subtask_index: usize,
    pub editing_subtask_index: Option<usize>,

    // Command palette state
    pub palette_items: Vec<PaletteItem>,
    pub palette_filtered: Vec<usize>,
    pub palette_index: usize,

    // Skills state
    pub installed_skills: Vec<crate::skills::InstalledSkill>,
    pub search_results: Vec<crate::skills::SearchResult>,
    pub skill_index: usize,
    pub skill_scope_global: bool,
    pub skill_detail_content: String,
    pub skill_status_message: String,
    pub selected_search_indices: HashSet<usize>,

    // Rate limit state
    pub rate_limit_state: crate::store::RateLimitState,

    // Background API usage fetch coordination
    usage_fetch_in_progress: Arc<AtomicBool>,

    // Background title generation
    title_tx: mpsc::Sender<(String, String)>,
    title_rx: mpsc::Receiver<(String, String)>,
    pub pending_titles: HashSet<String>,
    // Tasks waiting for title generation before auto-launching (task_id → project_id)
    pending_auto_launch: HashMap<String, String>,
    // Pending autonomous tasks to auto-launch on startup (project_id, task)
    startup_auto_launch: VecDeque<(String, Task)>,

    // PR status polling (merge + conflict detection)
    pr_poll_in_progress: Arc<AtomicBool>,
    pr_poll_tx: mpsc::Sender<PrPollResult>,
    pr_poll_rx: mpsc::Receiver<PrPollResult>,
    last_pr_poll: Instant,

    // Git stats polling
    git_stats_in_progress: Arc<AtomicBool>,
    git_stats_tx: mpsc::Sender<GitStatsResult>,
    git_stats_rx: mpsc::Receiver<GitStatsResult>,
    last_git_stats_poll: Instant,

    // External session scanner
    scanner_in_progress: Arc<AtomicBool>,
    scanner_tx: mpsc::Sender<crate::scanner::ScanResult>,
    scanner_rx: mpsc::Receiver<crate::scanner::ScanResult>,
    last_scan: Instant,
    pub external_sessions: Vec<crate::store::ExternalSession>,

    // Background session operations (create/teardown)
    session_op_tx: mpsc::Sender<SessionOpResult>,
    session_op_rx: mpsc::Receiver<SessionOpResult>,
    session_op_in_progress: bool,

    // Pending relaunch: when relaunching a stuck task, teardown fires first,
    // then this queues the task for auto-launch once teardown completes.
    // (task_id, project_id)
    pending_relaunch: Option<(String, String)>,

    // Toast notification
    pub toast_message: Option<String>,
    pub toast_style: ToastStyle,
    pub toast_expires: Option<std::time::Instant>,

    // Task status transition detection (for toast notifications)
    prev_task_statuses: HashMap<String, TaskStatus>,
    // Tasks that have already shown an InReview toast (avoid repeats from status cycling)
    notified_in_review: HashSet<String>,
    // Tasks that have already had a review loop spawned
    review_loop_spawned: HashSet<String>,

    // Slow-tick tracking for session tabs (DB refresh, PR polling, etc.)
    last_slow_tick: Instant,

    // Last known terminal area for mouse hit-testing
    pub last_terminal_area: Rect,

    // Sessions where Claude is waiting for user permission (detected from PTY screen)
    pub paused_sessions: HashSet<String>,

    // Sessions where Claude asked a question and is waiting for user answer (detected from PTY screen)
    pub waiting_sessions: HashSet<String>,

    // Cached result of visible_tasks() — indices into self.tasks, filtered and sorted.
    // Recomputed by recompute_visible_tasks() after data changes.
    cached_visible_indices: Vec<usize>,

    // Auto-update state
    update_check_in_progress: Arc<AtomicBool>,
    update_tx: mpsc::Sender<crate::update::UpdateCheckResult>,
    update_rx: mpsc::Receiver<crate::update::UpdateCheckResult>,
    last_update_check: Instant,
    /// Stores the version string after a successful auto-update (shown in title bar).
    pub updated_version: Option<String>,
    /// Stores a newer version string when one exists but installation failed.
    pub available_version: Option<String>,
}

/// Quick fallback title by truncating the first line at a word boundary.
/// Used immediately when creating a task so the UI stays responsive.
pub(super) fn fallback_title(prompt: &str) -> String {
    let first_line = prompt.lines().next().unwrap_or(prompt);
    if first_line.len() <= 60 {
        first_line.to_string()
    } else {
        // Find a char boundary at or before byte 60 to avoid panicking on multi-byte UTF-8
        let boundary = first_line
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= 60)
            .last()
            .unwrap_or(0);
        let truncated = &first_line[..boundary];
        if let Some(last_space) = truncated.rfind(' ') {
            format!("{}...", &truncated[..last_space])
        } else {
            format!("{truncated}...")
        }
    }
}

/// Generate a short title from a task prompt using Claude Haiku.
/// Called in a background thread. Falls back to the truncated title on failure.
fn generate_ai_title(prompt: &str) -> String {
    let system = "Output ONLY a concise title (max 8 words) for the given task. No quotes, no punctuation at the end, no preamble, no explanation. Just the title, nothing else.";
    let msg = format!("Title this task:\n{prompt}");

    if let Ok(output) = std::process::Command::new("claude")
        .args(["-p", "--model", "haiku", "--system-prompt", system, &msg])
        .output()
        && output.status.success()
    {
        // Take the last non-empty line to skip any preamble the model might add
        let raw = String::from_utf8_lossy(&output.stdout);
        let title = raw
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string();
        if !title.is_empty() {
            return title;
        }
    }

    fallback_title(prompt)
}

/// Check if a vt100 terminal screen shows a Claude Code permission prompt.
///
/// Claude Code renders tool-approval dialogs with patterns like:
///   "Allow Bash", "Allow `WebFetch`", etc.
/// Recursively compute the inner area (content inside border) for every leaf pane
/// in a layout tree, given the total outer area.
fn collect_pane_inner_areas(
    node: &crate::pty::LayoutNode,
    area: Rect,
) -> Vec<(crate::pty::PaneId, Rect)> {
    use ratatui::layout::{Constraint, Direction, Layout};
    use ratatui::widgets::{Block, Borders};

    let mut result = Vec::new();

    match node {
        crate::pty::LayoutNode::Pane(id) => {
            let block = Block::default().borders(Borders::ALL);
            let inner = block.inner(area);
            result.push((*id, inner));
        }
        crate::pty::LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let dir = match direction {
                crate::pty::SplitDirection::Horizontal => Direction::Horizontal,
                crate::pty::SplitDirection::Vertical => Direction::Vertical,
            };
            let chunks = Layout::default()
                .direction(dir)
                .constraints([
                    Constraint::Percentage(*ratio),
                    Constraint::Percentage(100 - *ratio),
                ])
                .split(area);
            result.extend(collect_pane_inner_areas(first, chunks[0]));
            result.extend(collect_pane_inner_areas(second, chunks[1]));
        }
    }

    result
}

/// Compute exact PTY inner dimensions for each pane using ratatui's layout engine.
///
/// Uses the same `Layout` and `Block::inner()` logic as the rendering path
/// (`draw_session_tab` + `render_layout_node` + `render_single_pane`) so PTY sizes
/// always match the actual rendered areas — no off-by-one edge clipping.
fn compute_pane_sizes_for_resize(
    layout: &crate::pty::LayoutNode,
    total_cols: u16,
    total_rows: u16,
) -> Vec<(crate::pty::PaneId, u16, u16)> {
    // Terminal content area matches draw_session_tab layout:
    // tab bar (1) + terminal area (remaining) + hint bar (1)
    let term_area = Rect {
        x: 0,
        y: 0,
        width: total_cols,
        height: total_rows.saturating_sub(2),
    };
    collect_pane_inner_areas(layout, term_area)
        .into_iter()
        .map(|(id, r)| (id, r.height, r.width))
        .collect()
}

/// followed by interactive options ("Yes", "No", "Always").
///
/// Only checks the bottom 20 rows of the screen to avoid false positives
/// from Claude's text output that might mention "Allow" in discussion.
fn screen_shows_permission_prompt(screen: &vt100::Screen) -> bool {
    let contents = screen.contents();
    let lines: Vec<&str> = contents.lines().collect();
    let total = lines.len();

    // Only check the bottom portion of the screen where prompts appear
    let start = total.saturating_sub(20);
    let bottom_lines = &lines[start..];

    // Look for "Allow <ToolName>" pattern — the tool name starts with an uppercase letter.
    // This matches Claude Code's permission dialog for any tool (Bash, WebFetch, Read, etc.)
    let has_allow = bottom_lines.iter().any(|line| {
        // Find "Allow " anywhere in the line (may be preceded by box-drawing chars or symbols)
        if let Some(pos) = line.find("Allow ") {
            let after = &line[pos + 6..];
            after.starts_with(|c: char| c.is_ascii_uppercase())
        } else {
            false
        }
    });

    if !has_allow {
        return false;
    }

    // Confirm with yes/no options nearby — Claude Code shows interactive choices
    // like "Yes  No  Always" on the same line
    bottom_lines.iter().any(|line| {
        (line.contains("Yes") || line.contains("yes"))
            && (line.contains("No") || line.contains("no"))
    })
}

/// Detect Claude Code's `AskUserQuestion` interactive selector in the PTY screen.
///
/// When Claude uses `AskUserQuestion`, the terminal shows a question with selectable
/// options. The selector always includes "Other" as a choice, and uses `❯` (U+276F) as
/// the cursor on the currently focused option. We detect this pattern in the bottom 25
/// lines of the screen.
fn screen_shows_question_prompt(screen: &vt100::Screen) -> bool {
    let contents = screen.contents();
    let lines: Vec<&str> = contents.lines().collect();
    let total = lines.len();

    let start = total.saturating_sub(25);
    let bottom_lines = &lines[start..];

    // Look for "❯" (selection cursor) on an option line — not the bare input prompt.
    // The input prompt is just "❯" possibly followed by typed text at the very bottom,
    // but question options have "❯" followed by a label among other option lines.
    let has_selection_cursor = bottom_lines.iter().any(|line| {
        let trimmed = line.trim();
        // Selection cursor followed by a space and option text
        trimmed.starts_with('\u{276f}')
    });

    if !has_selection_cursor {
        return false;
    }

    // AskUserQuestion always appends "Other" as a selectable option.
    // Check that "Other" appears as a standalone option line (trimmed).
    bottom_lines
        .iter()
        .any(|line| line.trim().starts_with("Other"))
}

fn build_project_summaries(store: &Store, projects: &[Project]) -> HashMap<String, ProjectSummary> {
    let mut summaries = HashMap::with_capacity(projects.len());
    for project in projects {
        let active_sessions = store
            .list_active_sessions_for_project(&project.id)
            .unwrap_or_default();
        let task_counts = store.count_tasks_by_status(&project.id).unwrap_or_default();
        summaries.insert(
            project.id.clone(),
            ProjectSummary {
                active_sessions,
                task_counts,
                default_branch: project.default_branch.clone(),
            },
        );
    }
    summaries
}

/// Run `git diff --stat` in a worktree and parse the summary line.
/// Returns (files changed, lines added, lines removed).
fn parse_git_diff_stat(worktree_path: &str, default_branch: &str) -> Option<(i64, i64, i64)> {
    let origin_branch = format!("origin/{default_branch}");
    let output = std::process::Command::new("git")
        .args(["diff", "--stat", &origin_branch])
        .current_dir(worktree_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The summary line looks like:
    //  3 files changed, 10 insertions(+), 2 deletions(-)
    // or just "1 file changed, 5 insertions(+)" etc.
    let last_line = stdout.lines().last()?;

    if !last_line.contains("changed") {
        // No changes — empty diff
        return Some((0, 0, 0));
    }

    let mut files = 0i64;
    let mut added = 0i64;
    let mut removed = 0i64;

    for part in last_line.split(',') {
        let part = part.trim();
        if part.contains("file") {
            files = part
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if part.contains("insertion") {
            added = part
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        } else if part.contains("deletion") {
            removed = part
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        }
    }

    Some((files, added, removed))
}

fn check_pr_status(pr_url: &str) -> PrStatus {
    let Ok(output) = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            pr_url,
            "--json",
            "state,mergeable,statusCheckRollup",
        ])
        .output()
    else {
        return PrStatus::Open;
    };
    if !output.status.success() {
        return PrStatus::Open;
    }
    let raw = String::from_utf8_lossy(&output.stdout);

    // Parse JSON: {"state":"OPEN","mergeable":"CONFLICTING","statusCheckRollup":[...]}
    let Ok(json) = serde_json::from_str::<serde_json::Value>(raw.trim()) else {
        return PrStatus::Open;
    };

    let state = json["state"].as_str().unwrap_or("");
    if state.eq_ignore_ascii_case("MERGED") {
        return PrStatus::Merged;
    }

    let mergeable = json["mergeable"].as_str().unwrap_or("");
    if mergeable.eq_ignore_ascii_case("CONFLICTING") {
        return PrStatus::Conflicting;
    }

    // Check CI status from statusCheckRollup
    if let Some(checks) = json["statusCheckRollup"].as_array()
        && !checks.is_empty()
    {
        // Any completed check with FAILURE/ERROR conclusion means CI failed
        let has_failure = checks.iter().any(|check| {
            let conclusion = check["conclusion"].as_str().unwrap_or("");
            conclusion.eq_ignore_ascii_case("FAILURE") || conclusion.eq_ignore_ascii_case("ERROR")
        });
        if has_failure {
            return PrStatus::CiFailed;
        }

        // Check if all checks have completed (non-empty conclusion)
        let all_done = checks.iter().all(|check| {
            let conclusion = check["conclusion"].as_str().unwrap_or("");
            !conclusion.is_empty()
        });

        return if all_done {
            PrStatus::CiPassed
        } else {
            PrStatus::CiRunning
        };
    }

    PrStatus::Open
}

/// Fetch usage from the Anthropic OAuth API and write to the shared cache file.
#[expect(
    clippy::similar_names,
    reason = "5h and 7d are distinct domain-specific window labels"
)]
fn fetch_and_cache_usage() -> Option<()> {
    // Get OAuth token from macOS Keychain
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let token_json = String::from_utf8(output.stdout).ok()?;
    let creds: serde_json::Value = serde_json::from_str(token_json.trim()).ok()?;
    let access_token = creds["claudeAiOauth"]["accessToken"].as_str()?;

    // Fetch usage from API. Use --fail so HTTP errors (401, 500) produce a
    // non-zero exit code instead of silently returning an error JSON body.
    let output = std::process::Command::new("curl")
        .args([
            "-sf",
            "--max-time",
            "10",
            "https://api.anthropic.com/api/oauth/usage",
            "-H",
            &format!("Authorization: Bearer {access_token}"),
            "-H",
            "anthropic-beta: oauth-2025-04-20",
            "-H",
            "Content-Type: application/json",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let usage: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;

    // Bail out if the API didn't return utilization data — don't overwrite
    // the cache with incomplete data that would blank the usage bars.
    let pct_5h = usage["five_hour"]["utilization"].as_f64();
    let pct_7d = usage["seven_day"]["utilization"].as_f64();
    if pct_5h.is_none() && pct_7d.is_none() {
        return None;
    }

    let now = chrono::Utc::now().timestamp_millis();

    // Format time-until-reset strings (matching statusline cache format)
    let format_time_left = |reset_at_str: &str| -> Option<String> {
        let reset_at = chrono::DateTime::parse_from_rfc3339(reset_at_str).ok()?;
        let time_left = reset_at.timestamp_millis() - now;
        if time_left <= 0 {
            return None;
        }
        let hours = time_left / (1000 * 60 * 60);
        let minutes = (time_left % (1000 * 60 * 60)) / (1000 * 60);
        if hours >= 24 {
            let days = hours / 24;
            let rem_hours = hours % 24;
            Some(format!("{days}d{rem_hours}h"))
        } else if hours > 0 {
            Some(format!("{hours}h{minutes}m"))
        } else {
            Some(format!("{minutes}m"))
        }
    };

    let reset_5h = usage["five_hour"]["resets_at"]
        .as_str()
        .and_then(format_time_left);
    let reset_7d = usage["seven_day"]["resets_at"]
        .as_str()
        .and_then(format_time_left);

    let cache = serde_json::json!({
        "timestamp": now,
        "data": {
            "reset5h": reset_5h,
            "reset7d": reset_7d,
            "pct5h": pct_5h.unwrap_or(0.0),
            "pct7d": pct_7d.unwrap_or(0.0)
        }
    });

    let home = dirs::home_dir()?;
    let cache_path = home.join(".claude/statusline-cache.json");
    std::fs::write(cache_path, serde_json::to_string(&cache).ok()?).ok()?;

    Some(())
}

#[cfg(test)]
mod tests {
    use super::input::{encode_mouse_event, keycode_to_bytes};
    use super::*;
    use crate::store::{Store, TaskMode, TaskStatus};
    use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};

    // ── Test Helpers ──

    fn test_app() -> App {
        let store = Store::open_in_memory().unwrap();
        App::new(store).unwrap()
    }

    fn test_app_with_project() -> App {
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("test-project", "/tmp/test-repo", "main")
            .unwrap();
        App::new(store).unwrap()
    }

    fn test_app_with_tasks() -> App {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("test-project", "/tmp/test-repo", "main")
            .unwrap();
        store
            .create_task(
                &project.id,
                "Task Alpha",
                "First task",
                TaskMode::Supervised,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &project.id,
                "Task Beta",
                "Second task",
                TaskMode::Autonomous,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &project.id,
                "Task Gamma",
                "Third task",
                TaskMode::Supervised,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        App::new(store).unwrap()
    }

    /// Simulate a key press, routing through the correct handler based on current `InputMode`.
    fn press(app: &mut App, code: KeyCode) {
        press_mod(app, code, KeyModifiers::NONE);
    }

    fn press_mod(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        match app.input_mode {
            InputMode::Normal => {
                app.handle_normal_key(code, modifiers).unwrap();
            }
            InputMode::NewTask => app.handle_input_key(code, modifiers).unwrap(),
            InputMode::EditTask => app.handle_edit_task_key(code, modifiers).unwrap(),
            InputMode::NewProject => app.handle_new_project_key(code, modifiers).unwrap(),
            InputMode::ConfirmDelete => app.handle_confirm_delete_key(code).unwrap(),
            InputMode::CommandPalette => app.handle_palette_key(code, modifiers).unwrap(),
            InputMode::SkillPanel => app.handle_skill_panel_key(code).unwrap(),
            InputMode::SkillSearch => app.handle_skill_search_key(code, modifiers).unwrap(),
            InputMode::SkillAdd => app.handle_skill_add_key(code, modifiers).unwrap(),
            InputMode::HelpOverlay => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                    app.input_mode = InputMode::Normal;
                }
            }
            InputMode::TaskDetails => match code {
                KeyCode::Esc | KeyCode::Char('v' | 'q') => {
                    app.input_mode = InputMode::Normal;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    app.task_details_scroll = app.task_details_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.task_details_scroll = app.task_details_scroll.saturating_sub(1);
                }
                _ => {}
            },
            InputMode::TaskFilter => app.handle_task_filter_key(code, modifiers).unwrap(),
            InputMode::SubtaskPanel => app.handle_subtask_panel_key(code, modifiers).unwrap(),
        }
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// Render the app to a test buffer and return the content as a string.
    #[allow(deprecated)]
    fn render_to_string(app: &mut App, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| super::super::ui::draw(frame, app))
            .unwrap();

        let buf = terminal.backend().buffer();
        let area = buf.area;
        let mut lines = Vec::new();
        for y in area.y..area.y + area.height {
            let mut line = String::new();
            for x in area.x..area.x + area.width {
                let cell = buf.get(x, y);
                line.push_str(cell.symbol());
            }
            lines.push(line.trim_end().to_string());
        }
        lines.join("\n")
    }

    // ═══════════════════════════════════════════════════════════════
    // 1. NAVIGATION TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn focus_switching_with_numbers() {
        let mut app = test_app();
        assert_eq!(app.focus, Focus::Projects);

        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.focus, Focus::Tasks);

        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.focus, Focus::Projects);
    }

    #[test]
    fn navigate_projects_jk() {
        let store = Store::open_in_memory().unwrap();
        store.create_project("alpha", "/tmp/alpha", "main").unwrap();
        store.create_project("beta", "/tmp/beta", "main").unwrap();
        store.create_project("gamma", "/tmp/gamma", "main").unwrap();
        let mut app = App::new(store).unwrap();

        assert_eq!(app.project_index, 0);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.project_index, 1);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.project_index, 2);
        // Clamp at end
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.project_index, 2);

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.project_index, 1);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.project_index, 0);
        // Clamp at start
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.project_index, 0);
    }

    #[test]
    fn navigate_tasks_jk() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.task_index, 0);

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.task_index, 1);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.task_index, 2);
        // Clamp
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.task_index, 2);

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.task_index, 1);
    }

    #[test]
    fn navigate_with_arrow_keys() {
        let store = Store::open_in_memory().unwrap();
        store.create_project("a", "/tmp/a", "main").unwrap();
        store.create_project("b", "/tmp/b", "main").unwrap();
        let mut app = App::new(store).unwrap();

        press(&mut app, KeyCode::Down);
        assert_eq!(app.project_index, 1);

        press(&mut app, KeyCode::Up);
        assert_eq!(app.project_index, 0);
    }

    #[test]
    fn quit_with_q() {
        let mut app = test_app();
        assert!(!app.should_quit);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn quit_with_ctrl_c() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(app.should_quit);
    }

    // ═══════════════════════════════════════════════════════════════
    // 2. PROJECT MANAGEMENT TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn add_project_opens_form() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.input_mode, InputMode::NewProject);
        assert_eq!(app.new_project_field, 0);
    }

    #[test]
    fn add_project_form_field_cycling() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.new_project_field, 0);

        type_str(&mut app, "my-proj");
        assert_eq!(app.input_buffer, "my-proj");

        // Tab to path field
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_project_field, 1);
        assert_eq!(app.new_project_name, "my-proj");

        // BackTab back to name
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_project_field, 0);
        assert_eq!(app.input_buffer, "my-proj");
    }

    #[test]
    fn add_project_cancel_with_esc() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('a'));
        type_str(&mut app, "will-cancel");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.new_project_name.is_empty());
    }

    #[test]
    fn add_project_submit() {
        let mut app = test_app();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_str().unwrap();

        press(&mut app, KeyCode::Char('a'));
        type_str(&mut app, "new-proj");
        press(&mut app, KeyCode::Tab);

        // Clear default "." in path field
        press(&mut app, KeyCode::Backspace);
        type_str(&mut app, path);
        press(&mut app, KeyCode::Enter);

        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.projects.len(), 1);
        assert_eq!(app.projects[0].name, "new-proj");
    }

    #[test]
    fn remove_project_with_confirm() {
        let mut app = test_app_with_project();
        assert_eq!(app.projects.len(), 1);

        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.input_mode, InputMode::ConfirmDelete);
        assert!(matches!(app.confirm_delete_kind, DeleteTarget::Project));
        assert_eq!(app.confirm_target, "test-project");

        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.projects.is_empty());
    }

    #[test]
    fn remove_project_cancel_n() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.projects.len(), 1);
    }

    #[test]
    fn remove_project_cancel_esc() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.projects.len(), 1);
    }

    #[test]
    fn select_project_loads_its_data() {
        let store = Store::open_in_memory().unwrap();
        let p1 = store.create_project("alpha", "/tmp/alpha", "main").unwrap();
        let p2 = store.create_project("beta", "/tmp/beta", "main").unwrap();
        store
            .create_task(
                &p1.id,
                "alpha-task",
                "",
                TaskMode::Supervised,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &p2.id,
                "beta-task",
                "",
                TaskMode::Supervised,
                None,
                None,
                crate::store::PushMode::Pr,
                false,
            )
            .unwrap();
        let mut app = App::new(store).unwrap();

        // First project selected by default
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].title, "alpha-task");

        // Navigate to second project
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.project_index, 1);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].title, "beta-task");
    }

    // ═══════════════════════════════════════════════════════════════
    // 3. TASK LIFECYCLE TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn create_task_full_flow() {
        let mut app = test_app_with_project();
        assert!(app.tasks.is_empty());

        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.input_mode, InputMode::NewTask);
        assert_eq!(app.new_task_field, 0);

        // Type prompt
        type_str(&mut app, "Fix the login bug");
        // Tab to mode
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_description, "Fix the login bug");
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);
        // Toggle mode: Autonomous → Supervised (Right)
        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);
        // Submit
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);

        let tasks = app
            .store
            .list_tasks_for_project(&app.projects[0].id)
            .unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Fix the login bug"); // auto-generated from prompt
        assert_eq!(tasks[0].description, "Fix the login bug");
        assert_eq!(tasks[0].mode, TaskMode::Supervised);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn create_task_cancel_empty_discards() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        // Esc with empty description discards (no draft created)
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.tasks.is_empty());
    }

    #[test]
    fn create_task_esc_saves_draft() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "Draft task");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].status, TaskStatus::Draft);
        assert_eq!(app.tasks[0].description, "Draft task");
    }

    #[test]
    fn create_task_requires_project() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn create_task_empty_prompt_does_not_submit() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Enter);
        // Should stay in NewTask mode
        assert_eq!(app.input_mode, InputMode::NewTask);
    }

    #[test]
    fn edit_task_flow() {
        let mut app = test_app_with_tasks();
        let original_id = app.tasks[0].id.clone();

        press(&mut app, KeyCode::Char('2')); // Focus tasks
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::EditTask);
        assert_eq!(app.input_buffer, "First task"); // loads description
        assert_eq!(app.editing_task_id.as_deref(), Some(original_id.as_str()));

        // Clear and retype prompt
        for _ in 0.."First task".len() {
            press(&mut app, KeyCode::Backspace);
        }
        type_str(&mut app, "Updated prompt");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);

        let task = app.store.get_task(&original_id).unwrap();
        assert_eq!(task.title, "Updated prompt"); // auto-generated from prompt
        assert_eq!(task.description, "Updated prompt");
    }

    #[test]
    fn edit_task_esc_saves_draft() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        for _ in 0..20 {
            press(&mut app, KeyCode::Backspace);
        }
        type_str(&mut app, "Changed");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.description, "Changed");
    }

    #[test]
    fn edit_task_only_works_on_pending_or_draft() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        // Navigate to the Working task (sorted after Pending tasks)
        let working_idx = app
            .visible_tasks()
            .iter()
            .position(|t| t.id == task_id)
            .unwrap();
        for _ in 0..working_idx {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn edit_draft_task_promotes_to_pending() {
        let mut app = test_app_with_project();
        // Create a draft by pressing Esc with content
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "My draft");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.tasks.len(), 1);
        assert_eq!(app.tasks[0].status, TaskStatus::Draft);

        // Focus tasks and edit the draft
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::EditTask);

        // Submit the edit — draft should become pending
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn delete_task_flow() {
        let mut app = test_app_with_tasks();
        assert_eq!(app.visible_tasks().len(), 3);

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.input_mode, InputMode::ConfirmDelete);
        assert!(matches!(app.confirm_delete_kind, DeleteTarget::Task));
        assert_eq!(app.confirm_target, "Task Alpha");

        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.input_mode, InputMode::Normal);
        let tasks = app
            .store
            .list_tasks_for_project(&app.projects[0].id)
            .unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn delete_task_any_status() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('d'));
        // Any task status should allow deletion
        assert_eq!(app.input_mode, InputMode::ConfirmDelete);
    }

    #[test]
    fn reorder_tasks_shift_j() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));

        assert_eq!(app.visible_tasks()[0].title, "Task Alpha");
        assert_eq!(app.visible_tasks()[1].title, "Task Beta");

        press(&mut app, KeyCode::Char('J'));
        assert_eq!(app.task_index, 1);
        assert_eq!(app.visible_tasks()[0].title, "Task Beta");
        assert_eq!(app.visible_tasks()[1].title, "Task Alpha");
    }

    #[test]
    fn reorder_tasks_shift_k() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('j')); // Move to second task
        assert_eq!(app.task_index, 1);

        press(&mut app, KeyCode::Char('K'));
        assert_eq!(app.task_index, 0);
        assert_eq!(app.visible_tasks()[0].title, "Task Beta");
        assert_eq!(app.visible_tasks()[1].title, "Task Alpha");
    }

    #[test]
    fn filter_tasks_enter_applies() {
        let mut app = test_app_with_tasks();
        assert_eq!(app.visible_tasks().len(), 3);

        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.input_mode, InputMode::TaskFilter);
        assert_eq!(app.focus, Focus::Tasks);

        type_str(&mut app, "alpha");
        assert_eq!(app.visible_tasks().len(), 1);
        assert_eq!(app.visible_tasks()[0].title, "Task Alpha");

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert_eq!(app.task_filter, "alpha");
        assert_eq!(app.visible_tasks().len(), 1);
    }

    #[test]
    fn filter_tasks_esc_clears() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('/'));
        type_str(&mut app, "beta");
        assert_eq!(app.visible_tasks().len(), 1);

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.task_filter.is_empty());
        assert_eq!(app.visible_tasks().len(), 3);
    }

    #[test]
    fn filter_is_case_insensitive() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('/'));
        type_str(&mut app, "GAMMA");
        assert_eq!(app.visible_tasks().len(), 1);
        assert_eq!(app.visible_tasks()[0].title, "Task Gamma");
    }

    #[test]
    fn visible_tasks_includes_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        // Transition through valid path: Pending → Working → InReview → Done
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::InReview)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::Done)
            .unwrap();
        app.refresh_data().unwrap();

        let visible = app.visible_tasks();
        assert!(visible.iter().any(|t| t.status == TaskStatus::Done));
        // Done tasks should be included
        assert_eq!(visible.len(), app.tasks.len());
    }

    #[test]
    fn visible_tasks_sorted_by_status_priority() {
        let mut app = test_app_with_tasks();
        // Alpha=Pending, Beta=Pending, Gamma=Pending initially.
        // Set each to a different status via valid transitions.
        let alpha_id = app.tasks[0].id.clone();
        let beta_id = app.tasks[1].id.clone();

        // Alpha: Pending → Working → Error
        app.store
            .update_task_status(&alpha_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&alpha_id, TaskStatus::Error)
            .unwrap();
        // Beta: Pending → Working → InReview
        app.store
            .update_task_status(&beta_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&beta_id, TaskStatus::InReview)
            .unwrap();
        // Gamma stays Pending
        app.refresh_data().unwrap();

        let visible = app.visible_tasks();
        // Expected order: InReview (Beta) → Error (Alpha) → Pending (Gamma)
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].title, "Task Beta");
        assert_eq!(visible[0].status, TaskStatus::InReview);
        assert_eq!(visible[1].title, "Task Alpha");
        assert_eq!(visible[1].status, TaskStatus::Error);
        assert_eq!(visible[2].title, "Task Gamma");
        assert_eq!(visible[2].status, TaskStatus::Pending);
    }

    // ═══════════════════════════════════════════════════════════════
    // 4. TASK REVIEW FLOW
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn review_task_marks_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        // Transition through valid path: Pending → Working → InReview
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::InReview)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('r'));

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(app.toast_message.as_deref(), Some("Task marked as done"));
    }

    #[test]
    fn review_working_task_marks_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        // Navigate to the Working task (sorted after Pending tasks)
        let working_idx = app
            .visible_tasks()
            .iter()
            .position(|t| t.id == task_id)
            .unwrap();
        for _ in 0..working_idx {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('r'));

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(app.toast_message.as_deref(), Some("Task marked as done"));
    }

    #[test]
    fn review_interrupted_task_marks_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        // Transition through valid path: Pending → Working → Interrupted
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::Interrupted)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        // Navigate to the Interrupted task (sorted before Pending)
        let idx = app
            .visible_tasks()
            .iter()
            .position(|t| t.id == task_id)
            .unwrap();
        for _ in 0..idx {
            press(&mut app, KeyCode::Char('j'));
        }
        press(&mut app, KeyCode::Char('r'));

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(app.toast_message.as_deref(), Some("Task marked as done"));
    }

    #[test]
    fn review_only_works_on_in_review_tasks() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('r'));

        // Pending task: r should do nothing
        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
    }

    // ═══════════════════════════════════════════════════════════════
    // 5. COMMAND PALETTE
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn command_palette_opens_with_ctrl_p() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(app.input_mode, InputMode::CommandPalette);
        assert!(app.input_buffer.is_empty());
        assert!(!app.palette_filtered.is_empty());
    }

    #[test]
    fn command_palette_filters_items() {
        let mut app = test_app_with_project();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        let initial_count = app.palette_filtered.len();

        type_str(&mut app, "quit");
        assert!(app.palette_filtered.len() < initial_count);
        assert!(!app.palette_filtered.is_empty());
    }

    #[test]
    fn command_palette_navigate() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(app.palette_index, 0);

        press(&mut app, KeyCode::Down);
        assert_eq!(app.palette_index, 1);

        press(&mut app, KeyCode::Up);
        assert_eq!(app.palette_index, 0);
    }

    #[test]
    fn command_palette_cancel() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "test");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn command_palette_execute_quit() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "quit");
        press(&mut app, KeyCode::Enter);
        assert!(app.should_quit);
    }

    #[test]
    fn command_palette_execute_focus_tasks() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "focus tasks");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Tasks);
    }

    #[test]
    fn command_palette_execute_new_task() {
        let mut app = test_app_with_project();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "new task");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::NewTask);
    }

    #[test]
    fn command_palette_execute_add_project() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "add project");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_mode, InputMode::NewProject);
    }

    #[test]
    fn command_palette_backspace_refilters() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "quit");
        let narrow_count = app.palette_filtered.len();

        press(&mut app, KeyCode::Backspace);
        // After removing a character, more items should match
        assert!(app.palette_filtered.len() >= narrow_count);
    }

    // ═══════════════════════════════════════════════════════════════
    // 7. HELP OVERLAY
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn help_overlay_open_close_question_mark() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.input_mode, InputMode::HelpOverlay);

        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn help_overlay_close_with_esc() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn help_overlay_close_with_q() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    // ═══════════════════════════════════════════════════════════════
    // 8. TASK FORM DETAILS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn task_form_backtab_cycles() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.new_task_field, 0);

        // BackTab wraps to field 6 (subtasks)
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 6);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 5);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 4);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 3);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 2);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 1);

        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.new_task_field, 0);
    }

    #[test]
    fn task_form_tab_forward_cycles() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 2);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 3);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 4);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 5);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 6);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 0);
    }

    #[test]
    fn task_form_mode_toggle() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);

        // Right cycles: Autonomous → Supervised → Exploration → Autonomous
        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);

        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Exploration);

        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);

        // Left cycles in reverse: Autonomous → Exploration → Supervised → Autonomous
        press(&mut app, KeyCode::Left);
        assert_eq!(app.new_task_mode, TaskMode::Exploration);

        press(&mut app, KeyCode::Left);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);

        press(&mut app, KeyCode::Left);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);
    }

    #[test]
    fn edit_task_form_cycling() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::EditTask);

        // Tab cycles through prompt (0), mode (1), base (2), branch (3), push_mode (4), loop (5), subtasks (6)
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 2);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 3);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 4);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 5);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 6);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 0);
    }

    // ═══════════════════════════════════════════════════════════════
    // 10. TOAST NOTIFICATION
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn toast_shows_on_success_actions() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        // Transition through valid path: Pending → Working → InReview
        app.store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&task_id, TaskStatus::InReview)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('r'));

        assert!(app.toast_message.is_some());
        assert!(matches!(app.toast_style, ToastStyle::Success));
    }

    #[test]
    fn toast_on_project_delete() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        assert!(app.toast_message.is_some());
        assert!(matches!(app.toast_style, ToastStyle::Success));
    }

    #[test]
    fn toast_on_task_delete() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        assert!(
            app.toast_message
                .as_deref()
                .is_some_and(|m| m.contains("deleted"))
        );
    }

    #[test]
    fn toast_on_task_edit() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        // Just submit with existing title
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.toast_message.as_deref(), Some("Task updated"));
    }

    // ═══════════════════════════════════════════════════════════════
    // 11. SNAPSHOT RENDER TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn snapshot_active_view_empty() {
        let mut app = test_app();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("claustre"));
        assert!(output.contains("Projects"));
        assert!(output.contains("No projects yet"));
    }

    #[test]
    fn snapshot_active_view_with_data() {
        let mut app = test_app_with_tasks();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("claustre"));
        assert!(output.contains("Projects"));
        assert!(output.contains("test-project"));
        assert!(output.contains("Task Queue"));
        assert!(output.contains("Task Alpha"));
        assert!(output.contains("Task Beta"));
        assert!(output.contains("Task Gamma"));
    }

    #[test]
    fn snapshot_active_view_session_detail() {
        let mut app = test_app_with_project();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Session Detail"));
        assert!(output.contains("No tasks"));
    }

    #[test]
    fn snapshot_help_overlay() {
        let mut app = test_app();
        app.input_mode = InputMode::HelpOverlay;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Help"));
        assert!(output.contains("Ctrl+P"));
        assert!(output.contains("Quit"));
    }

    #[test]
    fn snapshot_command_palette() {
        let mut app = test_app();
        app.input_mode = InputMode::CommandPalette;
        app.filter_palette();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Command Palette"));
        assert!(output.contains("New Task"));
        assert!(output.contains("Quit"));
    }

    #[test]
    fn snapshot_task_form() {
        let mut app = test_app_with_project();
        app.input_mode = InputMode::NewTask;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("New Task"));
        assert!(output.contains("Prompt"));
        assert!(output.contains("Mode"));
    }

    #[test]
    fn snapshot_edit_task_form() {
        let mut app = test_app_with_tasks();
        app.editing_task_id = Some(app.tasks[0].id.clone());
        app.new_task_description = "First task".to_string();
        app.input_buffer = "First task".to_string();
        app.input_mode = InputMode::EditTask;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Edit Task"));
        assert!(output.contains("Prompt"));
    }

    #[test]
    fn snapshot_new_project_panel() {
        let mut app = test_app();
        app.input_mode = InputMode::NewProject;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Add Project"));
        assert!(output.contains("Name"));
        assert!(output.contains("Path"));
    }

    #[test]
    fn snapshot_confirm_delete() {
        let mut app = test_app_with_project();
        app.input_mode = InputMode::ConfirmDelete;
        app.confirm_target = "test-project".to_string();
        app.confirm_delete_kind = DeleteTarget::Project;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Delete"));
        assert!(output.contains("test-project"));
    }

    #[test]
    fn snapshot_task_filter_active() {
        let mut app = test_app_with_tasks();
        app.input_mode = InputMode::TaskFilter;
        app.task_filter = "alpha".to_string();
        app.recompute_visible_tasks();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("/alpha"));
    }

    #[test]
    fn snapshot_usage_bars() {
        let mut app = test_app_with_project();
        app.rate_limit_state.usage_5h_pct = Some(42.0);
        app.rate_limit_state.usage_7d_pct = Some(15.0);
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Usage"));
        assert!(output.contains("5h"));
        assert!(output.contains("7d"));
        assert!(output.contains("42%"));
        assert!(output.contains("15%"));
    }

    #[test]
    fn snapshot_rate_limited_banner() {
        let mut app = test_app_with_project();
        app.rate_limit_state.is_rate_limited = true;
        app.rate_limit_state.limit_type = Some("5h".to_string());
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("RATE LIMITED"));
    }

    #[test]
    fn snapshot_toast_visible() {
        let mut app = test_app_with_project();
        app.show_toast("Test notification", ToastStyle::Success);
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Test notification"));
    }

    #[test]
    fn snapshot_task_status_indicators() {
        let mut app = test_app_with_tasks();
        // Set varied task statuses via valid transitions
        let t0 = app.tasks[0].id.clone();
        let t1 = app.tasks[1].id.clone();
        app.store
            .update_task_status(&t0, TaskStatus::Working)
            .unwrap();
        // t1: Pending → Working → InReview
        app.store
            .update_task_status(&t1, TaskStatus::Working)
            .unwrap();
        app.store
            .update_task_status(&t1, TaskStatus::InReview)
            .unwrap();
        app.refresh_data().unwrap();
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("working"));
        assert!(output.contains("in_review"));
        assert!(output.contains("pending"));
    }

    // ═══════════════════════════════════════════════════════════════
    // 12. SUBTASK PANEL
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn subtask_panel_opens() {
        let mut app = test_app_with_tasks();
        // Focus tasks
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.input_mode, InputMode::SubtaskPanel);
    }

    #[test]
    fn subtask_panel_add_and_close() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.input_mode, InputMode::SubtaskPanel);

        // Type a subtask description
        for c in "implement login".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);

        // Should have added a subtask
        let subtasks = app.store.list_subtasks_for_task(&task_id).unwrap();
        assert_eq!(subtasks.len(), 1);
        assert_eq!(subtasks[0].description, "implement login");

        // Close panel
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn subtask_panel_delete() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .create_subtask(&task_id, "step 1", "first step")
            .unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.subtasks.len(), 1);

        // Delete the subtask (d only works when input is empty)
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.subtasks.len(), 0);
        assert_eq!(app.toast_message.as_deref(), Some("Subtask deleted"));
    }

    #[test]
    fn subtask_panel_navigate() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .create_subtask(&task_id, "step 1", "first")
            .unwrap();
        app.store
            .create_subtask(&task_id, "step 2", "second")
            .unwrap();

        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.subtask_index, 0);

        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.subtask_index, 1);

        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.subtask_index, 0);
    }

    #[test]
    fn subtask_panel_requires_tasks_focus() {
        let mut app = test_app_with_tasks();
        // Focus is Projects by default
        assert_eq!(app.focus, Focus::Projects);
        press(&mut app, KeyCode::Char('s'));
        // Should be no-op since focus is not Tasks
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn subtask_counts_populated() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .create_subtask(&task_id, "step 1", "first")
            .unwrap();
        app.store
            .create_subtask(&task_id, "step 2", "second")
            .unwrap();
        app.refresh_data().unwrap();

        assert!(app.subtask_counts.contains_key(&task_id));
        let &(total, done) = app.subtask_counts.get(&task_id).unwrap();
        assert_eq!(total, 2);
        assert_eq!(done, 0);
    }

    #[test]
    fn snapshot_subtask_panel() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .create_subtask(&task_id, "step 1", "first step")
            .unwrap();
        app.subtasks = app.store.list_subtasks_for_task(&task_id).unwrap();
        app.input_mode = InputMode::SubtaskPanel;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Subtasks"));
        assert!(output.contains("step 1"));
    }

    // ═══════════════════════════════════════════════════════════════
    // 13. SKILL PANEL
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn skill_panel_opens_with_i() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.input_mode, InputMode::SkillPanel);
    }

    #[test]
    fn skill_panel_closes_with_esc() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn skill_panel_find_opens_search() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.input_mode, InputMode::SkillSearch);
        assert!(app.input_buffer.is_empty());
    }

    #[test]
    fn skill_panel_add_opens_add() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.input_mode, InputMode::SkillAdd);
        assert!(app.input_buffer.is_empty());
    }

    #[test]
    fn skill_search_esc_returns_to_panel() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.input_mode, InputMode::SkillSearch);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::SkillPanel);
    }

    #[test]
    fn skill_add_esc_returns_to_panel() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.input_mode, InputMode::SkillAdd);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::SkillPanel);
    }

    #[test]
    fn skill_panel_scope_toggle() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('i'));
        assert!(app.skill_scope_global);
        press(&mut app, KeyCode::Char('g'));
        assert!(!app.skill_scope_global);
        press(&mut app, KeyCode::Char('g'));
        assert!(app.skill_scope_global);
    }

    #[test]
    fn snapshot_skill_panel() {
        let mut app = test_app();
        app.input_mode = InputMode::SkillPanel;
        let output = render_to_string(&mut app, 100, 30);
        assert!(output.contains("Skills"));
        assert!(output.contains("global"));
        assert!(output.contains("No skills installed"));
    }

    // Text-editing unit tests (word boundary, apply_text_edit, format_with_cursor)
    // are in form.rs. Integration tests exercising them through the App follow.

    #[test]
    fn task_form_alt_backspace_deletes_word() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "hello world");
        press_mod(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(app.input_buffer, "hello ");
    }

    #[test]
    fn task_form_alt_b_f_word_jump() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "hello world test");
        // Alt+b (macOS Option+Left) jumps word left
        press_mod(&mut app, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 12); // before "test"
        press_mod(&mut app, KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 6); // before "world"
        // Alt+f (macOS Option+Right) jumps word right
        press_mod(&mut app, KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 12); // start of "test"
    }

    #[test]
    fn task_form_alt_arrow_word_jump() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "hello world test");
        // Alt+Left jumps word left
        press_mod(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 12); // before "test"
        press_mod(&mut app, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 6); // before "world"
        // Alt+Right jumps word right
        press_mod(&mut app, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(app.input_cursor, 12); // start of "test"
    }

    #[test]
    fn task_form_super_backspace_clears_line() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "hello world");
        press_mod(&mut app, KeyCode::Backspace, KeyModifiers::SUPER);
        assert_eq!(app.input_buffer, "");
    }

    #[test]
    fn palette_ctrl_w_deletes_word() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(app.input_mode, InputMode::CommandPalette);
        type_str(&mut app, "new task");
        press_mod(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(app.input_buffer, "new ");
    }

    #[test]
    fn filter_alt_backspace_deletes_word() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('/'));
        assert_eq!(app.input_mode, InputMode::TaskFilter);
        type_str(&mut app, "hello world");
        press_mod(&mut app, KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(app.task_filter, "hello ");
    }

    // ═══════════════════════════════════════════════════════════════
    // TAB NAVIGATION TESTS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn next_tab_wraps_around() {
        let mut app = test_app();
        // Only dashboard — no-op
        assert_eq!(app.active_tab, 0);
        app.next_tab();
        assert_eq!(app.active_tab, 0);

        // Add two fake session tabs (push Tab::Dashboard as placeholders since
        // we can't construct SessionTerminals in tests)
        app.tabs.push(Tab::Dashboard);
        app.tabs.push(Tab::Dashboard);
        assert_eq!(app.tabs.len(), 3);

        app.next_tab();
        assert_eq!(app.active_tab, 1);
        app.next_tab();
        assert_eq!(app.active_tab, 2);
        // Wraps back to 0
        app.next_tab();
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn prev_tab_wraps_around() {
        let mut app = test_app();
        // Only dashboard — no-op
        app.prev_tab();
        assert_eq!(app.active_tab, 0);

        // Add two fake session tabs
        app.tabs.push(Tab::Dashboard);
        app.tabs.push(Tab::Dashboard);

        // From dashboard (0), prev wraps to last tab (2)
        app.prev_tab();
        assert_eq!(app.active_tab, 2);
        app.prev_tab();
        assert_eq!(app.active_tab, 1);
        app.prev_tab();
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn ctrl_j_k_navigates_tabs_from_dashboard() {
        let mut app = test_app();
        // Add fake session tabs
        app.tabs.push(Tab::Dashboard);
        app.tabs.push(Tab::Dashboard);

        assert_eq!(app.active_tab, 0);

        // Ctrl+J moves to next tab
        press_mod(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 1);

        // Return to dashboard for next test
        app.active_tab = 0;

        // Ctrl+K wraps to last tab
        press_mod(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 2);
    }

    #[test]
    fn ctrl_j_k_noop_with_single_tab() {
        let mut app = test_app();
        assert_eq!(app.tabs.len(), 1);

        press_mod(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 0);

        press_mod(&mut app, KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert_eq!(app.active_tab, 0);
    }

    // ── keycode_to_bytes tests ──

    #[test]
    fn alt_backspace_sends_esc_del() {
        let bytes = keycode_to_bytes(KeyCode::Backspace, KeyModifiers::ALT);
        assert_eq!(bytes.as_bytes(), b"\x1b\x7f");
    }

    #[test]
    fn alt_char_sends_esc_prefix() {
        let bytes = keycode_to_bytes(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(bytes.as_bytes(), b"\x1bb");

        let bytes = keycode_to_bytes(KeyCode::Char('d'), KeyModifiers::ALT);
        assert_eq!(bytes.as_bytes(), b"\x1bd");

        let bytes = keycode_to_bytes(KeyCode::Char('f'), KeyModifiers::ALT);
        assert_eq!(bytes.as_bytes(), b"\x1bf");
    }

    #[test]
    fn plain_backspace_unchanged() {
        let bytes = keycode_to_bytes(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(bytes.as_bytes(), &[0x7f]);
    }

    #[test]
    fn ctrl_char_sends_control_code() {
        let bytes = keycode_to_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(bytes.as_bytes(), &[0x03]);
    }

    // ── Permission prompt detection tests ──

    #[test]
    fn permission_prompt_detected() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n\r\n");
        parser.process(b"  Allow Bash\r\n");
        parser.process(b"  ls -la\r\n");
        parser.process(b"  Yes  No  Always\r\n");
        assert!(screen_shows_permission_prompt(parser.screen()));
    }

    #[test]
    fn permission_prompt_not_detected_without_allow() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n");
        parser.process(b"  Running command: ls -la\r\n");
        parser.process(b"  Yes  No  Always\r\n");
        assert!(!screen_shows_permission_prompt(parser.screen()));
    }

    #[test]
    fn permission_prompt_not_detected_without_options() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"  Allow Bash\r\n");
        parser.process(b"  ls -la\r\n");
        assert!(!screen_shows_permission_prompt(parser.screen()));
    }

    #[test]
    fn permission_prompt_detected_webfetch() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"  Allow WebFetch\r\n");
        parser.process(b"  https://example.com\r\n");
        parser.process(b"  Yes  No\r\n");
        assert!(screen_shows_permission_prompt(parser.screen()));
    }

    #[test]
    fn permission_prompt_not_detected_lowercase_tool() {
        // "Allow something" where something is not capitalized (unlikely to be a tool)
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"  Allow me to explain\r\n");
        parser.process(b"  Yes  No\r\n");
        // "me" starts lowercase — should not match
        assert!(!screen_shows_permission_prompt(parser.screen()));
    }

    // ── Question prompt detection tests ──

    #[test]
    fn question_prompt_detected_with_options() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n\r\n");
        parser.process(b"  Which approach should we take?\r\n\r\n");
        parser.process(b"  \xe2\x9d\xaf Option A (Recommended)\r\n");
        parser.process(b"    Option B\r\n");
        parser.process(b"    Other\r\n");
        assert!(screen_shows_question_prompt(parser.screen()));
    }

    #[test]
    fn question_prompt_not_detected_without_cursor() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n");
        parser.process(b"  Option A\r\n");
        parser.process(b"  Other\r\n");
        assert!(!screen_shows_question_prompt(parser.screen()));
    }

    #[test]
    fn question_prompt_not_detected_without_other() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"Working on your task...\r\n");
        parser.process(b"  \xe2\x9d\xaf Option A\r\n");
        parser.process(b"  Option B\r\n");
        assert!(!screen_shows_question_prompt(parser.screen()));
    }

    #[test]
    fn question_prompt_not_confused_with_permission_text() {
        // Regular text mentioning "Other" and containing a right-pointing character shouldn't match
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"There are Other options available.\r\n");
        assert!(!screen_shows_question_prompt(parser.screen()));
    }

    #[test]
    fn question_prompt_detected_multiselect() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"  Which features do you want?\r\n\r\n");
        parser.process(b"  \xe2\x9d\xaf Feature A\r\n");
        parser.process(b"    Feature B\r\n");
        parser.process(b"    Feature C\r\n");
        parser.process(b"    Other\r\n");
        assert!(screen_shows_question_prompt(parser.screen()));
    }

    // ── Modified special key encoding tests ──

    #[test]
    fn shift_up_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Up, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2A");
    }

    #[test]
    fn shift_down_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Down, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2B");
    }

    #[test]
    fn ctrl_right_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Right, KeyModifiers::CONTROL);
        // Ctrl = 5 (1 + 4)
        assert_eq!(kb.as_bytes(), b"\x1b[1;5C");
    }

    #[test]
    fn shift_ctrl_left_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Left, KeyModifiers::SHIFT | KeyModifiers::CONTROL);
        // Shift+Ctrl = 6 (1 + 1 + 4)
        assert_eq!(kb.as_bytes(), b"\x1b[1;6D");
    }

    #[test]
    fn alt_arrow_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Up, KeyModifiers::ALT);
        // Alt = 3 (1 + 2)
        assert_eq!(kb.as_bytes(), b"\x1b[1;3A");
    }

    #[test]
    fn shift_home_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::Home, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2H");
    }

    #[test]
    fn shift_end_encodes_xterm_modifier() {
        let kb = keycode_to_bytes(KeyCode::End, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2F");
    }

    #[test]
    fn shift_pageup_encodes_tilde_modifier() {
        let kb = keycode_to_bytes(KeyCode::PageUp, KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[5;2~");
    }

    #[test]
    fn ctrl_delete_encodes_tilde_modifier() {
        let kb = keycode_to_bytes(KeyCode::Delete, KeyModifiers::CONTROL);
        assert_eq!(kb.as_bytes(), b"\x1b[3;5~");
    }

    #[test]
    fn shift_f1_encodes_modifier() {
        let kb = keycode_to_bytes(KeyCode::F(1), KeyModifiers::SHIFT);
        assert_eq!(kb.as_bytes(), b"\x1b[1;2P");
    }

    #[test]
    fn ctrl_f5_encodes_modifier() {
        let kb = keycode_to_bytes(KeyCode::F(5), KeyModifiers::CONTROL);
        assert_eq!(kb.as_bytes(), b"\x1b[15;5~");
    }

    #[test]
    fn unmodified_arrow_unchanged() {
        let kb = keycode_to_bytes(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(kb.as_bytes(), b"\x1b[A");
    }

    #[test]
    fn ctrl_char_still_works() {
        let kb = keycode_to_bytes(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(kb.as_bytes(), &[3]); // ETX
    }

    #[test]
    fn alt_char_still_prefixes_esc() {
        let kb = keycode_to_bytes(KeyCode::Char('b'), KeyModifiers::ALT);
        assert_eq!(kb.as_bytes(), b"\x1bb");
    }

    // ── Mouse event encoding tests ──

    #[test]
    fn sgr_scroll_up_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::ScrollUp,
            10,
            5,
            vt100::MouseProtocolEncoding::Sgr,
        );
        assert_eq!(bytes, Some(b"\x1b[<64;11;6M".to_vec()));
    }

    #[test]
    fn sgr_scroll_down_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::ScrollDown,
            10,
            5,
            vt100::MouseProtocolEncoding::Sgr,
        );
        assert_eq!(bytes, Some(b"\x1b[<65;11;6M".to_vec()));
    }

    #[test]
    fn sgr_left_press_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::Down(MouseButton::Left),
            0,
            0,
            vt100::MouseProtocolEncoding::Sgr,
        );
        assert_eq!(bytes, Some(b"\x1b[<0;1;1M".to_vec()));
    }

    #[test]
    fn sgr_left_release_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::Up(MouseButton::Left),
            0,
            0,
            vt100::MouseProtocolEncoding::Sgr,
        );
        // Release uses 'm' suffix instead of 'M'
        assert_eq!(bytes, Some(b"\x1b[<0;1;1m".to_vec()));
    }

    #[test]
    fn default_scroll_up_encoding() {
        let bytes = encode_mouse_event(
            &MouseEventKind::ScrollUp,
            10,
            5,
            vt100::MouseProtocolEncoding::Default,
        );
        // button=64+32=96, x=11+32=43, y=6+32=38
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 96, 43, 38]));
    }

    #[test]
    fn default_left_release_sends_button3() {
        let bytes = encode_mouse_event(
            &MouseEventKind::Up(MouseButton::Left),
            0,
            0,
            vt100::MouseProtocolEncoding::Default,
        );
        // Release: button=3+32=35, x=1+32=33, y=1+32=33
        assert_eq!(bytes, Some(vec![0x1b, b'[', b'M', 35, 33, 33]));
    }
}
