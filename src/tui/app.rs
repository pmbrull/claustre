//! TUI application state and event handling.
//!
//! Contains the `App` struct (all mutable state), key/mouse handlers,
//! data refresh logic, and background task coordination.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

/// How long toast notifications remain visible.
const TOAST_DURATION: Duration = Duration::from_secs(4);

/// Tick rate when viewing the dashboard (low refresh, saves CPU).
const DASHBOARD_TICK: Duration = Duration::from_secs(1);
/// Tick rate when viewing a session tab (fast refresh for smooth PTY rendering).
const SESSION_TICK: Duration = Duration::from_millis(16);
/// How often to run the slow-path tick work (DB refresh, PR polling, etc.) from a session tab.
const SESSION_SLOW_TICK: Duration = Duration::from_secs(2);

use crate::pty::{SessionTerminals, SplitDirection};
use crate::store::{Project, ProjectStats, Session, Store, Task, TaskStatus, TaskStatusCounts};

use super::event::{self, AppEvent};
use super::ui;

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

    // Enhanced task form state (field 0=prompt, 1=mode)
    pub new_task_field: u8,
    pub new_task_description: String,
    pub new_task_mode: crate::store::TaskMode,

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

    // Background session operations (create/teardown)
    session_op_tx: mpsc::Sender<SessionOpResult>,
    session_op_rx: mpsc::Receiver<SessionOpResult>,
    session_op_in_progress: bool,

    // Toast notification
    pub toast_message: Option<String>,
    pub toast_style: ToastStyle,
    pub toast_expires: Option<std::time::Instant>,

    // Task status transition detection (for toast notifications)
    prev_task_statuses: HashMap<String, TaskStatus>,
    // Tasks that have already shown an InReview toast (avoid repeats from status cycling)
    notified_in_review: HashSet<String>,

    // Slow-tick tracking for session tabs (DB refresh, PR polling, etc.)
    last_slow_tick: Instant,

    // Last known terminal area for mouse hit-testing
    pub last_terminal_area: Rect,

    // Sessions where Claude is waiting for user permission (detected from PTY screen)
    pub paused_sessions: HashSet<String>,

    // Cached result of visible_tasks() — indices into self.tasks, filtered and sorted.
    // Recomputed by recompute_visible_tasks() after data changes.
    cached_visible_indices: Vec<usize>,
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
}

/// Result from a background git diff --stat check.
struct GitStatsResult {
    session_id: String,
    files_changed: i64,
    lines_added: i64,
    lines_removed: i64,
}

impl App {
    pub fn new(store: Store) -> Result<Self> {
        let projects = store.list_projects()?;

        // Detect stale working sessions (no PTY tab on startup = interrupted).
        // On startup, zero PTY tabs exist, so any Working session is guaranteed stale.
        for project in &projects {
            let proj_sessions = store.list_active_sessions_for_project(&project.id)?;
            let proj_tasks = store.list_tasks_for_project(&project.id)?;
            for session in &proj_sessions {
                if session.claude_status == crate::store::ClaudeStatus::Working {
                    if let Some(task) = proj_tasks.iter().find(|t| {
                        t.session_id.as_deref() == Some(&session.id)
                            && t.status == TaskStatus::Working
                    }) {
                        if task.pr_url.is_some() {
                            // Task was resumed from in_review but has an open PR —
                            // restore to in_review instead of marking interrupted.
                            store.update_task_status(&task.id, TaskStatus::InReview)?;
                            store.update_session_status(
                                &session.id,
                                crate::store::ClaudeStatus::Done,
                                "PR in review",
                            )?;
                        } else {
                            store.update_session_status(
                                &session.id,
                                crate::store::ClaudeStatus::Interrupted,
                                "Session interrupted",
                            )?;
                            store.update_task_status(&task.id, TaskStatus::Interrupted)?;
                        }
                    } else {
                        // No working task — just mark session interrupted
                        store.update_session_status(
                            &session.id,
                            crate::store::ClaudeStatus::Interrupted,
                            "Session interrupted",
                        )?;
                    }
                }
            }
        }

        let (sessions, tasks) = if let Some(project) = projects.first() {
            let sessions = store.list_sessions_for_project(&project.id)?;
            let tasks = store.list_tasks_for_project(&project.id)?;
            (sessions, tasks)
        } else {
            (vec![], vec![])
        };

        let project_stats = projects
            .first()
            .and_then(|p| store.project_stats(&p.id).ok());

        let palette_items = vec![
            PaletteItem {
                label: "New Task".into(),
                action: PaletteAction::NewTask,
            },
            PaletteItem {
                label: "Add Project".into(),
                action: PaletteAction::AddProject,
            },
            PaletteItem {
                label: "Remove Project".into(),
                action: PaletteAction::RemoveProject,
            },
            PaletteItem {
                label: "Focus Projects".into(),
                action: PaletteAction::FocusProjects,
            },
            PaletteItem {
                label: "Focus Tasks".into(),
                action: PaletteAction::FocusTasks,
            },
            PaletteItem {
                label: "Find Skills".into(),
                action: PaletteAction::FindSkills,
            },
            PaletteItem {
                label: "Update Skills".into(),
                action: PaletteAction::UpdateSkills,
            },
            PaletteItem {
                label: "Quit".into(),
                action: PaletteAction::Quit,
            },
        ];
        let palette_filtered: Vec<usize> = (0..palette_items.len()).collect();

        let project_summaries = build_project_summaries(&store, &projects);
        let rate_limit_state = store.get_rate_limit_state().unwrap_or_default();

        // Find pending autonomous tasks without a session to auto-launch on startup
        let startup_auto_launch: VecDeque<(String, Task)> = store
            .pending_autonomous_tasks_unassigned()
            .unwrap_or_default()
            .into_iter()
            .map(|t| (t.project_id.clone(), t))
            .collect();
        let prev_task_statuses: HashMap<String, TaskStatus> =
            tasks.iter().map(|t| (t.id.clone(), t.status)).collect();
        let (tx, rx) = mpsc::channel();
        let (pr_tx, pr_rx) = mpsc::channel();
        let (gs_tx, gs_rx) = mpsc::channel();
        let (so_tx, so_rx) = mpsc::channel();

        let config = crate::config::load().unwrap_or_default();
        let theme = config.theme.build();

        let mut app = App {
            store,
            config,
            theme,
            keymap: super::keymap::KeyMap::default_keymap(),
            should_quit: false,
            focus: Focus::Projects,
            input_mode: InputMode::Normal,
            tabs: vec![Tab::Dashboard],
            active_tab: 0,
            projects,
            sessions,
            tasks,
            project_summaries,
            project_stats,
            project_index: 0,
            task_index: 0,
            task_list_state: ListState::default(),
            input_buffer: String::new(),
            input_cursor: 0,
            new_task_field: 0,
            new_task_description: String::new(),
            new_task_mode: crate::store::TaskMode::Autonomous,
            new_project_field: 0,
            new_project_name: String::new(),
            new_project_path: String::new(),
            path_suggestions: vec![],
            path_suggestion_index: 0,
            show_path_suggestions: false,
            confirm_target: String::new(),
            confirm_entity_id: String::new(),
            confirm_delete_kind: DeleteTarget::Project,
            editing_task_id: None,
            task_filter: String::new(),
            task_filter_cursor: 0,
            subtasks: vec![],
            subtask_index: 0,
            subtask_counts: HashMap::new(),
            new_task_subtasks: vec![],
            new_task_subtask_index: 0,
            editing_subtask_index: None,
            palette_items,
            palette_filtered,
            palette_index: 0,
            installed_skills: vec![],
            search_results: vec![],
            skill_index: 0,
            skill_scope_global: true,
            skill_detail_content: String::new(),
            skill_status_message: String::new(),
            rate_limit_state,
            usage_fetch_in_progress: Arc::new(AtomicBool::new(false)),
            title_tx: tx,
            title_rx: rx,
            pending_titles: HashSet::new(),
            pending_auto_launch: HashMap::new(),
            startup_auto_launch,
            pr_poll_in_progress: Arc::new(AtomicBool::new(false)),
            pr_poll_tx: pr_tx,
            pr_poll_rx: pr_rx,
            last_pr_poll: Instant::now(),
            git_stats_in_progress: Arc::new(AtomicBool::new(false)),
            git_stats_tx: gs_tx,
            git_stats_rx: gs_rx,
            last_git_stats_poll: Instant::now(),
            session_op_tx: so_tx,
            session_op_rx: so_rx,
            session_op_in_progress: false,
            toast_message: None,
            toast_style: ToastStyle::Info,
            toast_expires: None,
            prev_task_statuses,
            notified_in_review: HashSet::new(),
            last_slow_tick: Instant::now(),
            last_terminal_area: Rect::default(),
            paused_sessions: HashSet::new(),
            cached_visible_indices: Vec::new(),
        };

        app.recompute_visible_tasks();

        // Reconnect to any session-host processes that survived a TUI restart
        app.reconnect_running_sessions();

        Ok(app)
    }

    pub fn refresh_data(&mut self) -> Result<()> {
        self.projects = self.store.list_projects()?;

        if let Some(project) = self.projects.get(self.project_index) {
            self.sessions = self.store.list_sessions_for_project(&project.id)?;
            self.tasks = self.store.list_tasks_for_project(&project.id)?;
        } else {
            self.sessions.clear();
            self.tasks.clear();
        }

        // Detect Working → InReview transitions and show a toast (once per task)
        let new_review_title = self.tasks.iter().find_map(|t| {
            (t.status == TaskStatus::InReview
                && self.prev_task_statuses.get(&t.id) == Some(&TaskStatus::Working)
                && !self.notified_in_review.contains(&t.id))
            .then(|| (t.id.clone(), t.title.clone()))
        });
        // Clear notified set for tasks that left InReview (e.g. marked done)
        self.notified_in_review.retain(|id| {
            self.tasks
                .iter()
                .any(|t| t.id == *id && t.status == TaskStatus::InReview)
        });
        self.prev_task_statuses = self
            .tasks
            .iter()
            .map(|t| (t.id.clone(), t.status))
            .collect();
        if let Some((id, title)) = new_review_title {
            self.notified_in_review.insert(id);
            self.show_toast(format!("Ready for review: {title}"), ToastStyle::Success);
        }

        // Pre-fetch sidebar summaries for all projects
        self.project_summaries = build_project_summaries(&self.store, &self.projects);

        // Refresh cached project stats for the selected project
        self.project_stats = self
            .selected_project()
            .and_then(|p| self.store.project_stats(&p.id).ok());

        // Pre-fetch subtask counts for visible tasks
        self.subtask_counts.clear();
        for task in &self.tasks {
            if let Ok(counts) = self.store.subtask_count(&task.id)
                && counts.0 > 0
            {
                self.subtask_counts.insert(task.id.clone(), counts);
            }
        }

        // Recompute visible tasks cache after data changes
        self.recompute_visible_tasks();

        // Clamp indices
        if self.project_index >= self.projects.len() && !self.projects.is_empty() {
            self.project_index = self.projects.len() - 1;
        }
        let visible_count = self.visible_task_count();
        if self.task_index >= visible_count && visible_count > 0 {
            self.task_index = visible_count - 1;
        } else if visible_count == 0 {
            self.task_index = 0;
        }

        // Refresh subtasks for selected task
        if let Some(task) = self.visible_task_at(self.task_index) {
            self.subtasks = self
                .store
                .list_subtasks_for_task(&task.id)
                .unwrap_or_default();
        } else {
            self.subtasks.clear();
        }
        if self.subtask_index >= self.subtasks.len() && !self.subtasks.is_empty() {
            self.subtask_index = self.subtasks.len() - 1;
        } else if self.subtasks.is_empty() {
            self.subtask_index = 0;
        }

        // Refresh rate limit state and auto-clear if expired
        if let Ok(state) = self.store.get_rate_limit_state() {
            if state.is_rate_limited
                && let Some(ref reset_at) = state.reset_at
                && let Ok(reset_time) = chrono::DateTime::parse_from_rfc3339(reset_at)
                && chrono::Utc::now() > reset_time
            {
                let _ = self.store.clear_rate_limit();
                self.rate_limit_state = self.store.get_rate_limit_state().unwrap_or_default();
            } else {
                self.rate_limit_state = state;
            }
        }

        // Read usage percentages from the Claude API cache
        self.refresh_usage_from_api_cache();

        Ok(())
    }

    /// Read usage percentages from ~/.claude/statusline-cache.json (shared with statusline).
    /// Always uses cached data if present. Triggers a background refresh when stale.
    fn refresh_usage_from_api_cache(&mut self) {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let cache_path = home.join(".claude/statusline-cache.json");

        let mut cache_fresh = false;

        if let Ok(content) = std::fs::read_to_string(&cache_path)
            && let Ok(cache) = serde_json::from_str::<serde_json::Value>(&content)
        {
            // Always use cached data regardless of age
            self.rate_limit_state.usage_5h_pct = cache["data"]["pct5h"].as_f64();
            self.rate_limit_state.usage_7d_pct = cache["data"]["pct7d"].as_f64();
            self.rate_limit_state.reset_5h = cache["data"]["reset5h"].as_str().map(String::from);
            self.rate_limit_state.reset_7d = cache["data"]["reset7d"].as_str().map(String::from);

            let timestamp = cache["timestamp"].as_f64().unwrap_or(0.0);
            #[expect(
                clippy::cast_precision_loss,
                reason = "millisecond epoch fits in f64 for decades"
            )]
            let age_ms = (chrono::Utc::now().timestamp_millis() as f64) - timestamp;
            cache_fresh = age_ms < 120_000.0;
        }

        if !cache_fresh && !self.usage_fetch_in_progress.load(Ordering::Relaxed) {
            self.spawn_usage_fetch();
        }
    }

    /// Spawn a background thread to fetch usage from the Anthropic OAuth API
    /// and write the result to the shared cache file.
    fn spawn_usage_fetch(&self) {
        let flag = self.usage_fetch_in_progress.clone();
        flag.store(true, Ordering::Relaxed);

        std::thread::spawn(move || {
            let _result = fetch_and_cache_usage();
            flag.store(false, Ordering::Relaxed);
        });
    }

    /// Spawn a background thread to generate a title for a task via Claude Haiku.
    /// When the title is ready, it's sent through the channel and picked up on the next tick.
    fn spawn_title_generation(&mut self, task_id: String, prompt: String) {
        self.pending_titles.insert(task_id.clone());
        let tx = self.title_tx.clone();
        std::thread::spawn(move || {
            let title = generate_ai_title(&prompt);
            let _ = tx.send((task_id, title));
        });
    }

    /// Drain background title results and update tasks in the DB.
    /// If any completed titles belong to autonomous tasks awaiting launch, launch them now.
    fn poll_title_results(&mut self) -> Result<()> {
        while let Ok((task_id, title)) = self.title_rx.try_recv() {
            self.pending_titles.remove(&task_id);
            self.store.update_task_title(&task_id, &title)?;

            if let Some(project_id) = self.pending_auto_launch.remove(&task_id) {
                let task = self.store.get_task(&task_id)?;
                let branch_name = crate::session::generate_branch_name(&task.title);
                self.spawn_create_session(project_id, branch_name, task);
            }
        }
        Ok(())
    }

    /// Auto-launch pending autonomous tasks found at startup.
    /// Processes one task at a time, waiting for the previous session op to complete.
    fn auto_launch_pending_tasks(&mut self) {
        if self.session_op_in_progress || self.startup_auto_launch.is_empty() {
            return;
        }
        let Some((project_id, task)) = self.startup_auto_launch.pop_front() else {
            return;
        };
        let branch_name = crate::session::generate_branch_name(&task.title);
        self.spawn_create_session(project_id, branch_name, task);
    }

    /// Spawn a background thread to create a session (worktree + config + DB).
    /// The TUI stays responsive while the potentially slow git commands run.
    /// When complete, the main thread spawns PTY terminals and adds the tab.
    fn spawn_create_session(&mut self, project_id: String, branch_name: String, task: Task) {
        self.session_op_in_progress = true;
        self.show_toast("Launching session...", ToastStyle::Info);
        let tx = self.session_op_tx.clone();
        std::thread::spawn(move || {
            let result = match crate::store::Store::open() {
                Ok(store) => {
                    match crate::session::create_session(
                        &store,
                        &project_id,
                        &branch_name,
                        Some(&task),
                    ) {
                        Ok(setup) => {
                            if setup.socket_path.is_none() {
                                SessionOpResult::CreatedNoTask {
                                    message: "Session created (no task)".into(),
                                }
                            } else {
                                SessionOpResult::Created(Box::new(setup))
                            }
                        }
                        Err(e) => SessionOpResult::Error {
                            message: format!("Launch failed: {e}"),
                        },
                    }
                }
                Err(e) => SessionOpResult::Error {
                    message: format!("Launch failed (DB): {e}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    /// Unified entry point for launching a task as a session.
    ///
    /// Handles the full lifecycle: promotes Draft → Pending if needed, ensures a
    /// Haiku-generated title exists (spawning generation + queuing auto-launch if not),
    /// and finally creates the session once the title is ready.
    ///
    /// Called from both the `l` key handler and autonomous task creation.
    fn launch_task(&mut self, task_id: String, project_id: String) -> Result<()> {
        let task = self.store.get_task(&task_id)?;

        // Promote draft → pending
        if task.status == crate::store::TaskStatus::Draft {
            self.store
                .update_task_status(&task_id, crate::store::TaskStatus::Pending)?;
        }

        // Title generation already in progress — just queue for launch when ready
        if self.pending_titles.contains(&task_id) {
            self.pending_auto_launch.insert(task_id, project_id);
            return Ok(());
        }

        // Task still has a fallback title — generate a proper one first, then launch
        if task.title == fallback_title(&task.description) {
            let description = task.description;
            self.spawn_title_generation(task_id.clone(), description);
            self.pending_auto_launch.insert(task_id, project_id);
            return Ok(());
        }

        // Title is ready — launch the session directly
        let branch_name = crate::session::generate_branch_name(&task.title);
        self.spawn_create_session(project_id, branch_name, task);
        Ok(())
    }

    /// Spawn a background thread to tear down a session (worktree cleanup + DB update).
    /// The TUI removes the session tab (dropping PTY handles) before calling this.
    fn spawn_teardown_session(&mut self, session_id: String) {
        // Remove the session tab immediately (drops PTY handles, kills child processes)
        self.remove_session_tab(&session_id);

        self.session_op_in_progress = true;
        let tx = self.session_op_tx.clone();
        std::thread::spawn(move || {
            let result = match crate::store::Store::open() {
                Ok(store) => match crate::session::teardown_session(&store, &session_id) {
                    Ok(()) => SessionOpResult::TornDown {
                        message: "Session torn down".into(),
                    },
                    Err(e) => SessionOpResult::Error {
                        message: format!("Teardown failed: {e}"),
                    },
                },
                Err(e) => SessionOpResult::Error {
                    message: format!("Teardown failed (DB): {e}"),
                },
            };
            let _ = tx.send(result);
        });
    }

    pub fn show_toast(&mut self, message: impl Into<String>, style: ToastStyle) {
        self.toast_message = Some(message.into());
        self.toast_style = style;
        self.toast_expires = Some(Instant::now() + TOAST_DURATION);
    }

    fn tick_toast(&mut self) {
        if let Some(expires) = self.toast_expires
            && std::time::Instant::now() > expires
        {
            self.toast_message = None;
            self.toast_expires = None;
        }
    }

    /// Poll PR status for all `in_review` and `conflict` tasks that have a PR URL.
    /// Detects merges, new conflicts, and conflict resolution.
    /// Spawns a background thread every ~15 seconds.
    fn maybe_poll_pr_merges(&mut self) {
        const PR_POLL_INTERVAL: Duration = Duration::from_secs(15);

        if self.last_pr_poll.elapsed() < PR_POLL_INTERVAL {
            return;
        }
        self.last_pr_poll = Instant::now();

        if self.pr_poll_in_progress.load(Ordering::Relaxed) {
            return;
        }

        let Ok(tasks) = self.store.list_in_review_tasks_with_pr() else {
            return;
        };
        if tasks.is_empty() {
            return;
        }

        // Collect (task_id, session_id, pr_url, title, status) for the background thread
        let check_list: Vec<(String, Option<String>, String, String, TaskStatus)> = tasks
            .into_iter()
            .filter_map(|t| {
                let url = t.pr_url?;
                Some((t.id, t.session_id, url, t.title, t.status))
            })
            .collect();

        if check_list.is_empty() {
            return;
        }

        let flag = self.pr_poll_in_progress.clone();
        flag.store(true, Ordering::Relaxed);
        let tx = self.pr_poll_tx.clone();

        std::thread::spawn(move || {
            for (task_id, session_id, pr_url, title, task_status) in check_list {
                match check_pr_status(&pr_url) {
                    PrStatus::Merged => {
                        let _ = tx.send(PrPollResult::Merged {
                            task_id,
                            session_id,
                            task_title: title,
                        });
                    }
                    PrStatus::Conflicting if task_status != TaskStatus::Conflict => {
                        let _ = tx.send(PrPollResult::Conflict {
                            task_id,
                            task_title: title,
                        });
                    }
                    PrStatus::CiFailed if task_status != TaskStatus::CiFailed => {
                        let _ = tx.send(PrPollResult::CiFailed {
                            task_id,
                            task_title: title,
                        });
                    }
                    PrStatus::Open if task_status == TaskStatus::Conflict => {
                        let _ = tx.send(PrPollResult::ConflictResolved {
                            task_id,
                            task_title: title,
                        });
                    }
                    PrStatus::Open if task_status == TaskStatus::CiFailed => {
                        let _ = tx.send(PrPollResult::CiRecovered {
                            task_id,
                            task_title: title,
                        });
                    }
                    _ => {}
                }
            }
            flag.store(false, Ordering::Relaxed);
        });
    }

    /// Drain PR poll results and handle merges, conflicts, and conflict resolution.
    fn poll_pr_merge_results(&mut self) -> Result<()> {
        while let Ok(result) = self.pr_poll_rx.try_recv() {
            match result {
                PrPollResult::Merged {
                    task_id,
                    session_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::Done)?;
                    if let Some(ref sid) = session_id {
                        self.spawn_teardown_session(sid.clone());
                    }
                    self.show_toast(
                        format!("PR merged — task done: {task_title}"),
                        ToastStyle::Success,
                    );
                }
                PrPollResult::Conflict {
                    task_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::Conflict)?;
                    self.show_toast(format!("PR has conflicts: {task_title}"), ToastStyle::Error);
                }
                PrPollResult::ConflictResolved {
                    task_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::InReview)?;
                    self.show_toast(
                        format!("Conflicts resolved: {task_title}"),
                        ToastStyle::Success,
                    );
                }
                PrPollResult::CiFailed {
                    task_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::CiFailed)?;
                    self.show_toast(format!("CI checks failed: {task_title}"), ToastStyle::Error);
                }
                PrPollResult::CiRecovered {
                    task_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::InReview)?;
                    self.show_toast(
                        format!("CI checks passing: {task_title}"),
                        ToastStyle::Success,
                    );
                }
            }
        }
        Ok(())
    }

    /// Poll git diff stats for all active sessions every ~5 seconds.
    fn maybe_poll_git_stats(&mut self) {
        const GIT_STATS_INTERVAL: Duration = Duration::from_secs(5);

        if self.last_git_stats_poll.elapsed() < GIT_STATS_INTERVAL {
            return;
        }
        self.last_git_stats_poll = Instant::now();

        if self.git_stats_in_progress.load(Ordering::Relaxed) {
            return;
        }

        // Collect all active sessions with their worktree paths and default branches
        let worktrees: Vec<(String, String, String)> = self
            .project_summaries
            .iter()
            .flat_map(|(_, summary)| {
                let branch = summary.default_branch.clone();
                summary
                    .active_sessions
                    .iter()
                    .map(move |s| (s.id.clone(), s.worktree_path.clone(), branch.clone()))
            })
            .collect();

        if worktrees.is_empty() {
            return;
        }

        let flag = self.git_stats_in_progress.clone();
        flag.store(true, Ordering::Relaxed);
        let tx = self.git_stats_tx.clone();

        std::thread::spawn(move || {
            for (session_id, worktree_path, default_branch) in worktrees {
                if let Some(stats) = parse_git_diff_stat(&worktree_path, &default_branch) {
                    let _ = tx.send(GitStatsResult {
                        session_id,
                        files_changed: stats.0,
                        lines_added: stats.1,
                        lines_removed: stats.2,
                    });
                }
            }
            flag.store(false, Ordering::Relaxed);
        });
    }

    /// Drain background session operation results, spawn PTYs for new sessions, and show toasts.
    fn poll_session_ops(&mut self) {
        while let Ok(result) = self.session_op_rx.try_recv() {
            match result {
                SessionOpResult::Created(setup) => {
                    let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                    let cols = term_size.0;
                    let rows = term_size.1.saturating_sub(2);

                    // Claude terminal connects to the session-host socket
                    let claude_result = if let Some(ref socket_path) = setup.socket_path {
                        crate::pty::EmbeddedTerminal::connect(socket_path, rows, cols / 2)
                    } else {
                        // No task: wrap `claude` so the PTY drops to a shell after exit
                        let wrapped = crate::session::wrap_cmd_with_shell_fallback(vec![
                            "claude".to_string(),
                        ]);
                        let mut cmd = portable_pty::CommandBuilder::new(&wrapped[0]);
                        for arg in &wrapped[1..] {
                            cmd.arg(arg);
                        }
                        cmd.cwd(&setup.worktree_path);
                        crate::pty::EmbeddedTerminal::spawn(cmd, rows, cols / 2)
                    };

                    let terminals_result = match claude_result {
                        Ok(claude) => {
                            if let Some(ref layout_config) = self.config.layout {
                                crate::pty::SessionTerminals::from_layout(
                                    claude,
                                    &setup.worktree_path,
                                    layout_config,
                                    rows,
                                    cols,
                                )
                            } else {
                                // Default: spawn shell + use from_parts
                                let shell_path =
                                    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
                                let mut shell_cmd = portable_pty::CommandBuilder::new(&shell_path);
                                shell_cmd.cwd(&setup.worktree_path);
                                crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, cols / 2).map(
                                    |shell| {
                                        crate::pty::SessionTerminals::from_parts(
                                            shell,
                                            claude,
                                            &setup.worktree_path,
                                        )
                                    },
                                )
                            }
                        }
                        Err(e) => Err(e),
                    };

                    match terminals_result {
                        Ok(mut terminals) => {
                            let sizes = compute_pane_sizes_for_resize(
                                &terminals.layout,
                                term_size.0,
                                term_size.1,
                            );
                            let _ = terminals.resize_panes(&sizes);
                            self.add_session_tab(
                                setup.session.id.clone(),
                                Box::new(terminals),
                                setup.tab_label,
                            );
                            self.show_toast("Session launched", ToastStyle::Success);
                        }
                        Err(e) => {
                            self.show_toast(
                                format!("Session launch failed: {e}"),
                                ToastStyle::Error,
                            );
                        }
                    }
                }
                SessionOpResult::CreatedNoTask { message }
                | SessionOpResult::TornDown { message, .. } => {
                    self.show_toast(message, ToastStyle::Success);
                }
                SessionOpResult::Error { message } => {
                    self.show_toast(message, ToastStyle::Error);
                }
            }
            self.session_op_in_progress = false;
            let _ = self.refresh_data();
        }
    }

    /// Drain git stats results and persist to the database.
    fn poll_git_stats_results(&mut self) {
        while let Ok(result) = self.git_stats_rx.try_recv() {
            let _ = self.store.update_session_git_stats(
                &result.session_id,
                result.files_changed,
                result.lines_added,
                result.lines_removed,
            );
        }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_index)
    }

    /// Returns the session linked to the currently selected task, if any.
    pub fn session_for_selected_task(&self) -> Option<&Session> {
        let task = self.visible_tasks().into_iter().nth(self.task_index)?;
        let sid = task.session_id.as_deref()?;
        self.sessions.iter().find(|s| s.id == sid)
    }

    /// Recompute the cached visible task indices. Must be called after any change
    /// to `self.tasks`, `self.task_filter`, or task sort order.
    pub fn recompute_visible_tasks(&mut self) {
        let filter_lower = self.task_filter.to_lowercase();
        let mut indices: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.status != TaskStatus::Done
                    && (filter_lower.is_empty() || t.title.to_lowercase().contains(&filter_lower))
            })
            .map(|(i, _)| i)
            .collect();
        indices.sort_by(|&a, &b| {
            self.tasks[a]
                .status
                .sort_priority()
                .cmp(&self.tasks[b].status.sort_priority())
                .then_with(|| self.tasks[a].sort_order.cmp(&self.tasks[b].sort_order))
        });
        self.cached_visible_indices = indices;
    }

    /// Returns active tasks (excluding Done) for the selected project, optionally filtered
    /// by the current search term (`task_filter`). Uses case-insensitive title matching.
    /// Tasks are sorted by status priority, then by `sort_order` within each status group.
    pub fn visible_tasks(&self) -> Vec<&Task> {
        self.cached_visible_indices
            .iter()
            .map(|&i| &self.tasks[i])
            .collect()
    }

    /// Number of visible tasks (avoids allocating a Vec).
    pub fn visible_task_count(&self) -> usize {
        self.cached_visible_indices.len()
    }

    /// Get a single visible task by display index (avoids allocating a Vec).
    pub fn visible_task_at(&self, index: usize) -> Option<&Task> {
        self.cached_visible_indices
            .get(index)
            .map(|&i| &self.tasks[i])
    }

    /// Add a session tab with its terminals and switch to it.
    pub fn add_session_tab(
        &mut self,
        session_id: String,
        terminals: Box<SessionTerminals>,
        label: String,
    ) {
        self.tabs.push(Tab::Session {
            session_id,
            terminals,
            label,
        });
        // Don't auto-switch to the new tab — stay on dashboard
    }

    /// Remove a session tab by session ID. Returns to dashboard if it was active.
    pub fn remove_session_tab(&mut self, session_id: &str) {
        if let Some(idx) = self
            .tabs
            .iter()
            .position(|t| matches!(t, Tab::Session { session_id: sid, .. } if sid == session_id))
        {
            self.tabs.remove(idx);
            if self.active_tab >= idx && self.active_tab > 0 {
                self.active_tab -= 1;
            }
        }
    }

    /// Switch to the session tab matching the given session ID, if it exists.
    /// Returns `true` if the tab was found and activated.
    pub fn goto_session_tab(&mut self, session_id: &str) -> bool {
        if let Some(idx) = self
            .tabs
            .iter()
            .position(|t| matches!(t, Tab::Session { session_id: sid, .. } if sid == session_id))
        {
            self.active_tab = idx;
            true
        } else {
            false
        }
    }

    /// Restore a session tab for an active session whose PTY was lost (e.g. after
    /// Claustre was closed and reopened). Tries connecting to an existing session-host
    /// socket first, falls back to spawning `claude --continue` in the worktree.
    fn restore_session_tab(&mut self, session: &crate::store::Session) -> Result<()> {
        let worktree = std::path::Path::new(&session.worktree_path);
        if !worktree.exists() {
            self.show_toast("Worktree no longer exists on disk", ToastStyle::Error);
            return Ok(());
        }

        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
        let cols = term_size.0;
        let rows = term_size.1.saturating_sub(2);

        // Try to connect to existing session-host socket
        let socket_path = crate::config::session_socket_path(&session.id)?;
        let claude_terminal = if socket_path.exists() {
            crate::pty::EmbeddedTerminal::connect(&socket_path, rows, cols / 2)?
        } else {
            // Session-host is gone -- fall back to claude --continue
            let mut claude_builder = portable_pty::CommandBuilder::new("claude");
            claude_builder.arg("--continue");
            claude_builder.cwd(&session.worktree_path);
            crate::pty::EmbeddedTerminal::spawn(claude_builder, rows, cols / 2)?
        };

        let mut terminals = if let Some(ref layout_config) = self.config.layout {
            crate::pty::SessionTerminals::from_layout(
                claude_terminal,
                &session.worktree_path,
                layout_config,
                rows,
                cols,
            )?
        } else {
            let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let mut shell_cmd = portable_pty::CommandBuilder::new(&shell_path);
            shell_cmd.cwd(&session.worktree_path);
            let shell_terminal = crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, cols / 2)?;
            crate::pty::SessionTerminals::from_parts(
                shell_terminal,
                claude_terminal,
                &session.worktree_path,
            )
        };

        let sizes = compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
        let _ = terminals.resize_panes(&sizes);
        let label = session.tab_label.clone();
        self.add_session_tab(session.id.clone(), Box::new(terminals), label);
        // Switch to the newly added tab
        self.active_tab = self.tabs.len() - 1;

        // Restore session + task status based on task state
        if let Some(task) = self.tasks.iter().find(|t| {
            t.session_id.as_deref() == Some(&session.id) && t.status == TaskStatus::Interrupted
        }) {
            if task.pr_url.is_some() {
                // Interrupted task has an open PR — restore to in_review
                self.store
                    .update_task_status(&task.id, TaskStatus::InReview)?;
                self.store.update_session_status(
                    &session.id,
                    crate::store::ClaudeStatus::Done,
                    "PR in review",
                )?;
            } else {
                self.store
                    .update_task_status(&task.id, TaskStatus::Working)?;
                self.store.update_session_status(
                    &session.id,
                    crate::store::ClaudeStatus::Working,
                    "Restored",
                )?;
            }
        } else if self.tasks.iter().any(|t| {
            t.session_id.as_deref() == Some(&session.id)
                && matches!(
                    t.status,
                    TaskStatus::InReview | TaskStatus::Conflict | TaskStatus::CiFailed
                )
        }) {
            // Task already in review/conflict/ci_failed — session stays Done
            self.store.update_session_status(
                &session.id,
                crate::store::ClaudeStatus::Done,
                "PR in review",
            )?;
        } else {
            self.store.update_session_status(
                &session.id,
                crate::store::ClaudeStatus::Working,
                "Restored",
            )?;
        }
        self.refresh_data()?;

        self.show_toast("Session tab restored", ToastStyle::Success);
        Ok(())
    }

    /// Reconnect to running session-host processes on TUI startup.
    fn reconnect_running_sessions(&mut self) {
        let Ok(sockets_dir) = crate::config::sockets_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&sockets_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sock") {
                continue;
            }
            let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            // Skip if already have a tab for this session
            if self
                .tabs
                .iter()
                .any(|t| matches!(t, Tab::Session { session_id: sid, .. } if sid == session_id))
            {
                continue;
            }

            // Verify session is active in DB
            let Ok(session) = self.store.get_session(session_id) else {
                continue;
            };
            if session.closed_at.is_some() {
                let _ = std::fs::remove_file(&path);
                continue;
            }

            // Try to restore
            if let Err(e) = self.restore_session_tab(&session) {
                eprintln!("reconnect: failed to restore session {session_id}: {e}");
            }
        }
    }

    /// Switch to the next tab (wrapping around to Dashboard).
    fn next_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
    }

    /// Switch to the previous tab (wrapping around to last session).
    fn prev_tab(&mut self) {
        if self.tabs.len() > 1 {
            if self.active_tab == 0 {
                self.active_tab = self.tabs.len() - 1;
            } else {
                self.active_tab -= 1;
            }
        }
    }

    /// Process PTY output for all session tabs (called on each tick).
    fn process_pty_output(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.process_output();
            }
        }
    }

    /// Detect sessions where Claude is waiting for user permission by scanning PTY screens.
    ///
    /// When Claude Code shows a tool-approval dialog (e.g. "Allow Bash?"), the task appears
    /// as "working" but Claude is actually blocked on user input. This scans each session's
    /// Claude PTY screen for permission prompt patterns and populates `paused_sessions`.
    fn detect_paused_sessions(&mut self) {
        self.paused_sessions.clear();
        for tab in &self.tabs {
            if let Tab::Session {
                session_id,
                terminals,
                ..
            } = tab
            {
                // Only check sessions that have a working task
                let has_working_task = self.tasks.iter().any(|t| {
                    t.status == TaskStatus::Working
                        && t.session_id.as_deref() == Some(session_id.as_str())
                });
                if !has_working_task {
                    continue;
                }

                if let Some(screen) = terminals.claude_screen()
                    && screen_shows_permission_prompt(screen)
                {
                    self.paused_sessions.insert(session_id.clone());
                }
            }
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            // Adaptive tick rate: fast when viewing PTY, slow on dashboard.
            let tick_rate = if self.active_tab > 0 {
                SESSION_TICK
            } else {
                DASHBOARD_TICK
            };

            terminal.draw(|frame| {
                self.last_terminal_area = frame.area();
                ui::draw(frame, self);
            })?;

            match event::poll(tick_rate)? {
                AppEvent::Key(key) => {
                    // When on a session tab, route most keys to the PTY
                    if self.active_tab > 0 {
                        self.handle_session_tab_key(key.code, key.modifiers)?;
                        // Drain any additional queued key/paste events before redrawing
                        while let Ok(extra) = event::poll(Duration::from_millis(0)) {
                            match extra {
                                AppEvent::Key(k) => {
                                    self.handle_session_tab_key(k.code, k.modifiers)?;
                                }
                                AppEvent::Paste(text) => {
                                    self.handle_session_tab_paste(&text)?;
                                }
                                _ => break,
                            }
                        }
                        // Process PTY output immediately so the next frame reflects the keystroke
                        self.process_pty_output();
                    } else {
                        self.handle_dashboard_key(key.code, key.modifiers)?;
                    }
                }
                AppEvent::Paste(text) => {
                    if self.active_tab > 0 {
                        self.handle_session_tab_paste(&text)?;
                        self.process_pty_output();
                    } else {
                        self.handle_dashboard_paste(&text)?;
                    }
                }
                AppEvent::Mouse(mouse) => {
                    self.handle_mouse(mouse)?;
                }
                AppEvent::Tick => {
                    self.process_pty_output();
                    self.detect_paused_sessions();

                    // Fast-path tick work (always runs)
                    self.tick_toast();
                    self.poll_title_results()?;
                    self.poll_session_ops();
                    self.auto_launch_pending_tasks();
                    self.poll_pr_merge_results()?;
                    self.poll_git_stats_results();

                    // Slow-path tick work (DB refresh, background polls)
                    // Always run on dashboard; throttle on session tabs.
                    let run_slow =
                        self.active_tab == 0 || self.last_slow_tick.elapsed() >= SESSION_SLOW_TICK;
                    if run_slow {
                        self.last_slow_tick = Instant::now();
                        self.maybe_poll_pr_merges();
                        self.maybe_poll_git_stats();
                        self.refresh_data()?;
                    }
                }
                AppEvent::Resize(cols, rows) => {
                    self.handle_resize(cols, rows);
                }
            }

            if self.should_quit {
                return Ok(());
            }
        }
    }

    /// Dispatch a key event to the correct dashboard handler based on `input_mode`.
    fn handle_dashboard_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        match self.input_mode {
            InputMode::Normal => self.handle_normal_key(code, modifiers)?,
            InputMode::NewTask => self.handle_input_key(code, modifiers)?,
            InputMode::EditTask => self.handle_edit_task_key(code, modifiers)?,
            InputMode::NewProject => self.handle_new_project_key(code, modifiers)?,
            InputMode::ConfirmDelete => self.handle_confirm_delete_key(code)?,
            InputMode::CommandPalette => self.handle_palette_key(code, modifiers)?,
            InputMode::SkillPanel => self.handle_skill_panel_key(code)?,
            InputMode::SkillSearch => self.handle_skill_search_key(code, modifiers)?,
            InputMode::SkillAdd => self.handle_skill_add_key(code, modifiers)?,
            InputMode::HelpOverlay => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                    self.input_mode = InputMode::Normal;
                }
            }
            InputMode::TaskFilter => self.handle_task_filter_key(code, modifiers)?,
            InputMode::SubtaskPanel => self.handle_subtask_panel_key(code, modifiers)?,
        }
        Ok(())
    }

    /// Handle keys when a session tab is active.
    /// Intercept registered session keys; forward everything else to the PTY.
    fn handle_session_tab_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        if let Some(action) = self.keymap.lookup_session(code, modifiers) {
            self.execute_session_action(action)?;
            return Ok(());
        }

        // Forward to focused PTY, clear selection, and snap back to live screen
        if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
            terminals.selection = None;
            terminals.focused_terminal().reset_scrollback();
            let key_bytes = keycode_to_bytes(code, modifiers);
            if key_bytes.len > 0 {
                let _ = terminals
                    .focused_terminal()
                    .send_bytes(key_bytes.as_bytes());
            }
        }
        Ok(())
    }

    /// Execute a session-mode action (dashboard return, pane focus, splits, close).
    fn execute_session_action(&mut self, action: super::keymap::Action) -> Result<()> {
        use super::keymap::Action;
        match action {
            Action::ReturnToDashboard => {
                self.active_tab = 0;
            }
            Action::FocusPrevPane => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
                    terminals.focus_prev();
                }
            }
            Action::FocusNextPane => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
                    terminals.focus_next();
                }
            }
            Action::PrevTab => self.prev_tab(),
            Action::NextTab => self.next_tab(),
            Action::SplitRight => {
                let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                let rows = term_size.1.saturating_sub(2);
                let cols = term_size.0;
                let split_err = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let err = terminals
                        .split_focused(SplitDirection::Horizontal, rows, cols)
                        .err();
                    let sizes =
                        compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
                    let _ = terminals.resize_panes(&sizes);
                    err
                } else {
                    None
                };
                if let Some(e) = split_err {
                    self.show_toast(format!("Split failed: {e}"), ToastStyle::Error);
                }
            }
            Action::SplitDown => {
                let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                let rows = term_size.1.saturating_sub(2);
                let cols = term_size.0;
                let split_err = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let err = terminals
                        .split_focused(SplitDirection::Vertical, rows, cols)
                        .err();
                    let sizes =
                        compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
                    let _ = terminals.resize_panes(&sizes);
                    err
                } else {
                    None
                };
                if let Some(e) = split_err {
                    self.show_toast(format!("Split failed: {e}"), ToastStyle::Error);
                }
            }
            Action::ClosePane => {
                let close_result = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let closed = terminals.close_focused();
                    if closed {
                        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                        let sizes = compute_pane_sizes_for_resize(
                            &terminals.layout,
                            term_size.0,
                            term_size.1,
                        );
                        let _ = terminals.resize_panes(&sizes);
                    }
                    Some(closed)
                } else {
                    None
                };
                if close_result == Some(false) {
                    self.show_toast("Cannot close this pane", ToastStyle::Info);
                }
            }
            // Normal-mode-only actions are no-ops in session mode
            _ => {}
        }
        Ok(())
    }

    /// Forward pasted text to the focused PTY on a session tab.
    fn handle_session_tab_paste(&mut self, text: &str) -> Result<()> {
        if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
            terminals.selection = None;
            terminals.focused_terminal().reset_scrollback();
            // Send as bracketed paste so the embedded shell/editor handles it correctly
            let bracketed = format!("\x1b[200~{text}\x1b[201~");
            let _ = terminals
                .focused_terminal()
                .send_bytes(bracketed.as_bytes());
        }
        Ok(())
    }

    /// Handle pasted text on the dashboard by inserting at cursor in the active input buffer.
    fn handle_dashboard_paste(&mut self, text: &str) -> Result<()> {
        match self.input_mode {
            InputMode::NewTask | InputMode::EditTask
                if self.new_task_field == 0 || self.new_task_field == 2 =>
            {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
            }
            InputMode::NewProject => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                if self.new_project_field == 1 {
                    self.update_path_suggestions();
                }
            }
            InputMode::CommandPalette => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                self.filter_palette();
                self.palette_index = 0;
            }
            InputMode::SkillSearch => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                self.search_results.clear();
                self.skill_status_message.clear();
            }
            InputMode::SkillAdd | InputMode::SubtaskPanel => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
            }
            InputMode::TaskFilter => {
                self.task_filter
                    .insert_str(self.task_filter_cursor.min(self.task_filter.len()), text);
                self.task_filter_cursor =
                    (self.task_filter_cursor + text.len()).min(self.task_filter.len());
                self.recompute_visible_tasks();
                self.task_index = 0;
            }
            // Normal, ConfirmDelete, SkillPanel, HelpOverlay: no text input
            _ => {}
        }
        Ok(())
    }

    /// Handle terminal resize events — resize all PTYs to match new dimensions.
    ///
    /// Uses ratatui's layout engine to compute exact inner areas for each pane,
    /// ensuring PTY sizes always match the rendered areas.
    fn handle_resize(&mut self, cols: u16, rows: u16) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                let sizes = compute_pane_sizes_for_resize(&terminals.layout, cols, rows);
                let _ = terminals.resize_panes(&sizes);
            }
        }
    }

    /// Compute inner areas (content inside borders) for all panes in the current session tab.
    /// Returns a list of `(PaneId, inner_rect)` in absolute screen coordinates.
    fn session_pane_inner_areas(&self) -> Vec<(crate::pty::PaneId, Rect)> {
        let size = self.last_terminal_area;
        let has_tab_bar = self.tabs.len() > 1;
        let tab_bar_height = u16::from(has_tab_bar);

        let term_area = Rect {
            x: 0,
            y: tab_bar_height,
            width: size.width,
            height: size.height.saturating_sub(tab_bar_height + 1),
        };

        if let Some(Tab::Session { terminals, .. }) = self.tabs.get(self.active_tab) {
            collect_pane_inner_areas(&terminals.layout, term_area)
        } else {
            vec![]
        }
    }

    /// Translate absolute screen coordinates to vt100 terminal coordinates for a pane.
    /// Returns `(PaneId, vt100_row, vt100_col)` or `None` if outside all panes.
    fn screen_to_terminal_coords(
        &self,
        screen_col: u16,
        screen_row: u16,
    ) -> Option<(crate::pty::PaneId, u16, u16)> {
        for (id, inner) in &self.session_pane_inner_areas() {
            if screen_col >= inner.x
                && screen_col < inner.x + inner.width
                && screen_row >= inner.y
                && screen_row < inner.y + inner.height
            {
                return Some((*id, screen_row - inner.y, screen_col - inner.x));
            }
        }
        None
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let col = mouse.column;
        let row = mouse.row;
        let size = self.last_terminal_area;

        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        // --- Session tab: handle selection via Down/Drag/Up ---
        if self.active_tab > 0 {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let has_tab_bar = self.tabs.len() > 1;

                    // Tab bar click
                    if has_tab_bar && row == 0 {
                        let layout =
                            ui::compute_tab_layout(&self.tabs, self.active_tab, size.width);
                        for entry in &layout.entries {
                            if col >= entry.x_start && col < entry.x_start + entry.width {
                                self.active_tab = entry.tab_index;
                                return Ok(());
                            }
                        }
                        return Ok(());
                    }

                    // Click inside a terminal pane: start selection
                    if let Some((pane, vt_row, vt_col)) = self.screen_to_terminal_coords(col, row) {
                        if let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        {
                            terminals.focused = pane;
                            terminals.selection = Some(crate::pty::Selection {
                                pane,
                                start: (vt_row, vt_col),
                                end: (vt_row, vt_col),
                            });
                        }
                    } else {
                        // Click outside panes: clear selection
                        if let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        {
                            terminals.selection = None;
                        }
                    }
                    return Ok(());
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    // Compute pane areas before mutable borrow
                    let pane_areas = self.session_pane_inner_areas();
                    if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                        && let Some(ref mut sel) = terminals.selection
                        && let Some((_, inner)) = pane_areas.iter().find(|(id, _)| *id == sel.pane)
                    {
                        let vt_row = row
                            .saturating_sub(inner.y)
                            .min(inner.height.saturating_sub(1));
                        let vt_col = col
                            .saturating_sub(inner.x)
                            .min(inner.width.saturating_sub(1));
                        sel.end = (vt_row, vt_col);
                    }
                    return Ok(());
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    // Copy selected text to clipboard
                    if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                        && let Some(ref sel) = terminals.selection
                        && let Some(term) = terminals.terminal(sel.pane)
                    {
                        let text = sel.extract_text(term.screen());
                        if !text.is_empty()
                            && let Ok(mut clipboard) = arboard::Clipboard::new()
                        {
                            let _ = clipboard.set_text(&text);
                        }
                    }
                    return Ok(());
                }
                MouseEventKind::ScrollUp => {
                    if let Some((pane_id, _, _)) = self.screen_to_terminal_coords(col, row)
                        && let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        && let Some(term) = terminals.terminal_mut(pane_id)
                    {
                        term.scroll_up(3);
                    }
                    return Ok(());
                }
                MouseEventKind::ScrollDown => {
                    if let Some((pane_id, _, _)) = self.screen_to_terminal_coords(col, row)
                        && let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        && let Some(term) = terminals.terminal_mut(pane_id)
                    {
                        term.scroll_down(3);
                    }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }

        // --- Dashboard events ---
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {}
            MouseEventKind::ScrollUp => {
                if self.input_mode == InputMode::Normal {
                    self.move_up();
                }
                return Ok(());
            }
            MouseEventKind::ScrollDown => {
                if self.input_mode == InputMode::Normal {
                    self.move_down();
                }
                return Ok(());
            }
            _ => return Ok(()),
        }

        let has_tab_bar = self.tabs.len() > 1;
        let tab_bar_height = u16::from(has_tab_bar);

        // --- Tab bar click (top row, only when visible) ---
        if has_tab_bar && row == 0 {
            // Use the same layout computation as draw_tab_bar
            let layout = ui::compute_tab_layout(&self.tabs, self.active_tab, size.width);
            for entry in &layout.entries {
                if col >= entry.x_start && col < entry.x_start + entry.width {
                    self.active_tab = entry.tab_index;
                    return Ok(());
                }
            }
            return Ok(());
        }

        // --- Dashboard: only handle clicks in Normal mode ---
        if self.input_mode != InputMode::Normal {
            return Ok(());
        }

        // Recompute dashboard layout to determine which panel was clicked.
        // Layout mirrors draw_active_impl(): title(1) + main(min) + bottom(2)
        let content_top = tab_bar_height + 1; // +1 for title bar
        let content_bottom = size.height.saturating_sub(2); // -2 for status+hints
        if row < content_top || row >= content_bottom {
            return Ok(());
        }

        let content_height = content_bottom - content_top;

        // Main area: left 30% | right 70%
        let left_width = size.width * 30 / 100;

        if col < left_width {
            // Click in the left column (Projects panel area)
            // Left column: top 60% = Projects, bottom 40% = Stats
            let projects_height = content_height * 60 / 100;
            if row < content_top + projects_height {
                self.focus = Focus::Projects;
                // Try to select the clicked project item.
                // Projects panel has a 1-row border, so inner starts at content_top + 1.
                let inner_top = content_top + 1;
                if row >= inner_top {
                    let clicked_row = (row - inner_top) as usize;
                    // Each project may take multiple lines (name + session statuses).
                    // Walk through projects to find which one this row falls in.
                    let empty_summary = ProjectSummary::default();
                    let mut current_row: usize = 0;
                    for (i, project) in self.projects.iter().enumerate() {
                        let summary = self
                            .project_summaries
                            .get(&project.id)
                            .unwrap_or(&empty_summary);
                        let item_height = 1 + summary.active_sessions.len();
                        if clicked_row >= current_row && clicked_row < current_row + item_height {
                            if i != self.project_index {
                                self.project_index = i;
                                let _ = self.refresh_data();
                                self.task_index = 0;
                            }
                            break;
                        }
                        current_row += item_height;
                    }
                }
            }
            // Stats panel click — no action needed
        } else {
            // Click in the right column
            // Right column: top 60% = Tasks, bottom 40% = Session Detail + Usage
            let tasks_height = content_height * 60 / 100;
            if row < content_top + tasks_height {
                self.focus = Focus::Tasks;
                // Try to select the clicked task item.
                // Tasks panel has a 1-row border, so inner starts at content_top + 1.
                let inner_top = content_top + 1;
                if row >= inner_top {
                    let clicked_row = (row - inner_top) as usize;
                    let visible_count = self.visible_tasks().len();
                    // Account for scroll offset — clicked_row is relative to the viewport
                    let absolute_index = clicked_row + self.task_list_state.offset();
                    if absolute_index < visible_count {
                        self.task_index = absolute_index;
                    }
                }
            }
            // Session Detail / Usage clicks — no action needed
        }

        Ok(())
    }

    fn handle_normal_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        if let Some(action) = self.keymap.lookup_normal(code, modifiers) {
            self.execute_action(action)?;
        }
        Ok(())
    }

    /// Execute a normal-mode action. Context-dependent actions (e.g. `k` = kill
    /// or move-up, `l` = focus or launch) are resolved here based on current state.
    fn execute_action(&mut self, action: super::keymap::Action) -> Result<()> {
        use super::keymap::Action;
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::OpenCommandPalette => {
                self.input_mode = InputMode::CommandPalette;
                self.input_buffer.clear();
                self.palette_index = 0;
                self.filter_palette();
            }
            Action::PrevTab => self.prev_tab(),
            Action::NextTab => self.next_tab(),
            Action::FocusProjects => {
                self.focus = Focus::Projects;
            }
            Action::FocusTasks => self.focus = Focus::Tasks,
            Action::ShowHelp => {
                self.input_mode = InputMode::HelpOverlay;
            }
            Action::FilterTasks => {
                self.task_filter.clear();
                self.recompute_visible_tasks();
                self.input_mode = InputMode::TaskFilter;
                self.focus = Focus::Tasks;
            }
            Action::MoveDown => self.move_down(),
            Action::MoveUp => self.move_up(),
            Action::ReorderTaskDown => {
                if self.focus == Focus::Tasks {
                    let visible = self.visible_tasks();
                    if self.task_index + 1 < visible.len() {
                        let current_id = visible[self.task_index].id.clone();
                        let next_id = visible[self.task_index + 1].id.clone();
                        if self.store.swap_task_order(&current_id, &next_id).is_ok() {
                            self.task_index += 1;
                            let _ = self.refresh_data();
                        }
                    }
                }
            }
            Action::ReorderTaskUp => {
                if self.focus == Focus::Tasks && self.task_index > 0 {
                    let visible = self.visible_tasks();
                    let current_id = visible[self.task_index].id.clone();
                    let prev_id = visible[self.task_index - 1].id.clone();
                    if self.store.swap_task_order(&current_id, &prev_id).is_ok() {
                        self.task_index -= 1;
                        let _ = self.refresh_data();
                    }
                }
            }
            Action::Select => match self.focus {
                Focus::Projects => {
                    self.refresh_data()?;
                    self.task_index = 0;
                }
                Focus::Tasks => {
                    if let Some(task) = self.visible_tasks().get(self.task_index) {
                        if let Some(session_id) = &task.session_id {
                            let session = self.store.get_session(session_id)?;
                            if session.closed_at.is_none() {
                                if !self.goto_session_tab(&session.id) {
                                    self.restore_session_tab(&session)?;
                                }
                            } else {
                                self.show_toast("Session is closed", ToastStyle::Info);
                            }
                        } else if matches!(
                            task.status,
                            crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                        ) {
                            if self.session_op_in_progress {
                                self.show_toast(
                                    "Session operation in progress...",
                                    ToastStyle::Info,
                                );
                            } else if let Some(project_id) =
                                self.selected_project().map(|p| p.id.clone())
                            {
                                let task_id = task.id.clone();
                                self.launch_task(task_id, project_id)?;
                            }
                        }
                    }
                }
            },
            Action::OpenSubtasks => {
                if self.focus == Focus::Tasks && !self.visible_tasks().is_empty() {
                    if let Some(task) = self.visible_tasks().get(self.task_index) {
                        self.subtasks = self
                            .store
                            .list_subtasks_for_task(&task.id)
                            .unwrap_or_default();
                    }
                    self.subtask_index = 0;
                    self.input_buffer.clear();
                    self.input_mode = InputMode::SubtaskPanel;
                }
            }
            Action::NewTask => {
                if self.selected_project().is_some() {
                    self.reset_task_form();
                    self.input_mode = InputMode::NewTask;
                }
            }
            Action::EditTask => {
                if self.focus == Focus::Tasks {
                    let task_data = self.visible_tasks().get(self.task_index).map(|t| {
                        (
                            t.id.clone(),
                            t.title.clone(),
                            t.description.clone(),
                            t.mode,
                            t.status,
                        )
                    });
                    if let Some((id, _title, desc, mode, status)) = task_data
                        && matches!(
                            status,
                            crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                        )
                    {
                        self.editing_task_id = Some(id);
                        self.new_task_description.clone_from(&desc);
                        self.new_task_mode = mode;
                        self.new_task_field = 0;
                        self.input_buffer.clone_from(&desc);
                        self.input_cursor = self.input_buffer.len();
                        self.input_mode = InputMode::EditTask;
                    }
                }
            }
            Action::MarkDone => {
                if self.focus == Focus::Tasks
                    && let Some(task) = self.visible_tasks().get(self.task_index).copied()
                    && matches!(
                        task.status,
                        crate::store::TaskStatus::InReview
                            | crate::store::TaskStatus::Working
                            | crate::store::TaskStatus::Interrupted
                            | crate::store::TaskStatus::CiFailed
                    )
                {
                    self.store
                        .update_task_status(&task.id, crate::store::TaskStatus::Done)?;
                    if let Some(ref sid) = task.session_id {
                        self.spawn_teardown_session(sid.clone());
                    }
                    self.refresh_data()?;
                    self.show_toast("Task marked as done", ToastStyle::Success);
                }
            }
            // `k` = kill session when a running task is focused, otherwise vim-style move up
            Action::KillSession => {
                let mut killed = false;
                if self.focus == Focus::Tasks {
                    if self.session_op_in_progress {
                        self.show_toast("Session operation in progress...", ToastStyle::Info);
                        killed = true;
                    } else if let Some(task) = self.visible_tasks().get(self.task_index).copied()
                        && let Some(ref sid) = task.session_id
                        && matches!(
                            task.status,
                            crate::store::TaskStatus::Working
                                | crate::store::TaskStatus::InReview
                                | crate::store::TaskStatus::CiFailed
                                | crate::store::TaskStatus::Error
                        )
                    {
                        let sid = sid.clone();
                        self.store
                            .update_task_status(&task.id, crate::store::TaskStatus::Pending)?;
                        self.store.unassign_task_from_session(&task.id)?;
                        self.spawn_teardown_session(sid);
                        self.refresh_data()?;
                        self.show_toast("Session killed — press Enter to resume", ToastStyle::Info);
                        killed = true;
                    }
                }
                if !killed {
                    self.move_up();
                }
            }
            Action::OpenPR => {
                if self.focus == Focus::Tasks
                    && let Some(task) = self.visible_tasks().get(self.task_index).copied()
                    && let Some(ref url) = task.pr_url
                {
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let _ = std::process::Command::new(opener).arg(url).spawn();
                    self.show_toast("Opening PR in browser", ToastStyle::Success);
                }
            }
            // `l` = focus tasks when on projects, launch task when on tasks
            Action::LaunchTask => {
                if self.focus == Focus::Projects {
                    self.focus = Focus::Tasks;
                } else if self.session_op_in_progress {
                    self.show_toast("Session operation in progress...", ToastStyle::Info);
                } else {
                    let task_data = self
                        .visible_tasks()
                        .get(self.task_index)
                        .filter(|t| {
                            matches!(
                                t.status,
                                crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                            )
                        })
                        .map(|t| t.id.clone());
                    if let Some(task_id) = task_data
                        && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                    {
                        self.launch_task(task_id, project_id)?;
                    }
                }
            }
            Action::DeleteItem => match self.focus {
                Focus::Projects => {
                    if let Some((name, id)) = self
                        .selected_project()
                        .map(|p| (p.name.clone(), p.id.clone()))
                    {
                        self.confirm_target = name;
                        self.confirm_entity_id = id;
                        self.confirm_delete_kind = DeleteTarget::Project;
                        self.input_mode = InputMode::ConfirmDelete;
                    }
                }
                Focus::Tasks => {
                    let task_data = self
                        .visible_tasks()
                        .get(self.task_index)
                        .map(|t| (t.id.clone(), t.title.clone()));
                    if let Some((id, title)) = task_data {
                        self.confirm_target = title;
                        self.confirm_entity_id = id;
                        self.confirm_delete_kind = DeleteTarget::Task;
                        self.input_mode = InputMode::ConfirmDelete;
                    }
                }
            },
            Action::OpenSkills => {
                self.refresh_skills();
                self.skill_index = 0;
                self.input_mode = InputMode::SkillPanel;
            }
            Action::AddProject => {
                self.input_mode = InputMode::NewProject;
                self.input_buffer.clear();
                self.new_project_name.clear();
                self.new_project_path = String::from(".");
                self.new_project_field = 0;
                self.clear_path_autocomplete();
            }
            // Session-only actions are no-ops in normal mode
            Action::ReturnToDashboard
            | Action::FocusPrevPane
            | Action::FocusNextPane
            | Action::SplitRight
            | Action::SplitDown
            | Action::ClosePane => {}
        }
        Ok(())
    }

    /// Handle keys shared between new-task and edit-task forms (tab, back-tab, mode toggle, typing).
    /// Returns `true` if the key was consumed.
    fn handle_task_form_shared_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let field_count: u8 = 3;
        match code {
            // On subtask field with subtasks: Tab cycles through them
            KeyCode::Tab if self.new_task_field == 2 && !self.new_task_subtasks.is_empty() => {
                // If editing, save the current edit first
                if let Some(idx) = self.editing_subtask_index {
                    let trimmed = self.input_buffer.trim().to_string();
                    if !trimmed.is_empty() {
                        self.new_task_subtasks[idx] = trimmed;
                    }
                    self.editing_subtask_index = None;
                    self.input_buffer.clear();
                }
                self.new_task_subtask_index =
                    (self.new_task_subtask_index + 1) % self.new_task_subtasks.len();
                true
            }
            KeyCode::Tab => {
                self.editing_subtask_index = None;
                self.save_current_task_field();
                self.new_task_field = (self.new_task_field + 1) % field_count;
                self.load_current_task_field();
                true
            }
            KeyCode::BackTab => {
                // Cancel any editing state when leaving field 2
                self.editing_subtask_index = None;
                self.save_current_task_field();
                self.new_task_field = if self.new_task_field == 0 {
                    field_count - 1
                } else {
                    self.new_task_field - 1
                };
                self.load_current_task_field();
                true
            }
            KeyCode::Left | KeyCode::Right if self.new_task_field == 1 => {
                self.new_task_mode = match self.new_task_mode {
                    crate::store::TaskMode::Supervised => crate::store::TaskMode::Autonomous,
                    crate::store::TaskMode::Autonomous => crate::store::TaskMode::Supervised,
                };
                true
            }
            // Subtask input field: typing, add, delete, navigate
            _ if self.new_task_field == 2 => self.handle_subtask_input_key(code, modifiers),
            _ if self.new_task_field == 0 => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
            _ => false,
        }
    }

    /// Handle keys when the subtask input field (field 2) is focused in the task form.
    fn handle_subtask_input_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        match code {
            // Esc while editing a subtask: cancel edit
            KeyCode::Esc if self.editing_subtask_index.is_some() => {
                self.editing_subtask_index = None;
                self.input_buffer.clear();
                true
            }
            // Enter while editing: save edited subtask (trim, reject empty)
            KeyCode::Enter if self.editing_subtask_index.is_some() => {
                let trimmed = self.input_buffer.trim().to_string();
                if let Some(idx) = self.editing_subtask_index
                    && !trimmed.is_empty()
                {
                    self.new_task_subtasks[idx] = trimmed;
                }
                self.editing_subtask_index = None;
                self.input_buffer.clear();
                true
            }
            // Enter with text, not editing: add new subtask (trim, reject empty)
            KeyCode::Enter if !self.input_buffer.is_empty() => {
                let trimmed = self.input_buffer.trim().to_string();
                self.input_buffer.clear();
                if !trimmed.is_empty() {
                    self.new_task_subtasks.push(trimmed);
                }
                true
            }
            // Enter with empty input: start editing selected subtask
            KeyCode::Enter
                if self.input_buffer.is_empty()
                    && !self.new_task_subtasks.is_empty()
                    && self.editing_subtask_index.is_none() =>
            {
                let idx = self.new_task_subtask_index;
                self.editing_subtask_index = Some(idx);
                self.input_buffer.clone_from(&self.new_task_subtasks[idx]);
                self.input_cursor = self.input_buffer.len();
                true
            }
            // 'd' with empty input and not editing: delete selected subtask
            KeyCode::Char('d')
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                if !self.new_task_subtasks.is_empty() {
                    self.new_task_subtasks.remove(self.new_task_subtask_index);
                    if self.new_task_subtask_index >= self.new_task_subtasks.len()
                        && !self.new_task_subtasks.is_empty()
                    {
                        self.new_task_subtask_index = self.new_task_subtasks.len() - 1;
                    }
                }
                true
            }
            // j/k navigation only when not editing
            KeyCode::Char('j') | KeyCode::Down
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                if !self.new_task_subtasks.is_empty() {
                    self.new_task_subtask_index =
                        (self.new_task_subtask_index + 1).min(self.new_task_subtasks.len() - 1);
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                self.new_task_subtask_index = self.new_task_subtask_index.saturating_sub(1);
                true
            }
            _ => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
        }
    }

    fn handle_input_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        if self.handle_task_form_shared_key(code, modifiers) {
            return Ok(());
        }
        match code {
            KeyCode::Enter => {
                self.save_current_task_field();
                if !self.new_task_description.is_empty() {
                    if let Some(project_id) = self.selected_project().map(|p| p.id.clone()) {
                        let fallback = fallback_title(&self.new_task_description);
                        let task = self.store.create_task(
                            &project_id,
                            &fallback,
                            &self.new_task_description,
                            self.new_task_mode,
                        )?;

                        // Create inline subtasks
                        for subtask_desc in &self.new_task_subtasks {
                            let st_title = fallback_title(subtask_desc);
                            self.store
                                .create_subtask(&task.id, &st_title, subtask_desc)?;
                        }

                        // Launch autonomous tasks (generates title + auto-launches),
                        // or just generate the title for supervised tasks.
                        if self.new_task_mode == crate::store::TaskMode::Autonomous {
                            self.launch_task(task.id, project_id)?;
                        } else {
                            let desc = self.new_task_description.clone();
                            self.spawn_title_generation(task.id, desc);
                        }
                    }
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc => {
                self.save_current_task_field();
                if !self.new_task_description.is_empty()
                    && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                {
                    let fallback = fallback_title(&self.new_task_description);
                    let task = self.store.create_task(
                        &project_id,
                        &fallback,
                        &self.new_task_description,
                        self.new_task_mode,
                    )?;
                    self.store
                        .update_task_status(&task.id, crate::store::TaskStatus::Draft)?;

                    // Create inline subtasks
                    for subtask_desc in &self.new_task_subtasks {
                        let st_title = fallback_title(subtask_desc);
                        self.store
                            .create_subtask(&task.id, &st_title, subtask_desc)?;
                    }

                    self.show_toast("Task saved as draft", ToastStyle::Info);
                    self.refresh_data()?;
                }
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn save_current_task_field(&mut self) {
        if self.new_task_field == 0 {
            self.new_task_description.clone_from(&self.input_buffer);
        }
    }

    fn load_current_task_field(&mut self) {
        if self.new_task_field == 0 {
            self.input_buffer.clone_from(&self.new_task_description);
            self.input_cursor = self.input_buffer.len();
        } else {
            self.input_buffer.clear();
            self.input_cursor = 0;
        }
    }

    fn handle_new_project_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        // Path field (field 1) with autocomplete support
        if self.new_project_field == 1 {
            match code {
                KeyCode::Enter => {
                    self.save_current_project_field();
                    self.clear_path_autocomplete();
                    self.submit_new_project()?;
                }
                KeyCode::Tab if self.show_path_suggestions => {
                    self.accept_path_suggestion();
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.save_current_project_field();
                    self.clear_path_autocomplete();
                    self.new_project_field = 1 - self.new_project_field;
                    self.load_current_project_field();
                }
                KeyCode::Down if self.show_path_suggestions => {
                    if !self.path_suggestions.is_empty() {
                        self.path_suggestion_index =
                            (self.path_suggestion_index + 1).min(self.path_suggestions.len() - 1);
                    }
                }
                KeyCode::Up if self.show_path_suggestions => {
                    self.path_suggestion_index = self.path_suggestion_index.saturating_sub(1);
                }
                KeyCode::Esc if self.show_path_suggestions => {
                    self.clear_path_autocomplete();
                }
                KeyCode::Esc => {
                    self.input_buffer.clear();
                    self.new_project_name.clear();
                    self.new_project_path.clear();
                    self.new_project_field = 0;
                    self.clear_path_autocomplete();
                    self.input_mode = InputMode::Normal;
                }
                // Path field uses shared text editing with autocomplete refresh
                _ => {
                    let old_len = self.input_buffer.len();
                    let is_char = matches!(code, KeyCode::Char(_));
                    apply_text_edit(
                        &mut self.input_buffer,
                        &mut self.input_cursor,
                        code,
                        modifiers,
                    );
                    let new_len = self.input_buffer.len();

                    if new_len < old_len {
                        // Something was deleted — refresh autocomplete
                        self.refresh_path_autocomplete_after_delete();
                    } else if is_char && new_len > old_len {
                        // A character was inserted — check if it triggers autocomplete
                        let last_inserted =
                            self.input_buffer[..self.input_cursor].chars().next_back();
                        if last_inserted == Some('/')
                            || (last_inserted == Some('~') && self.input_buffer == "~")
                            || self.show_path_suggestions
                        {
                            self.update_path_suggestions();
                        }
                    }
                }
            }
        } else {
            // Name field (field 0) — use shared text editing
            match code {
                KeyCode::Enter => {
                    self.save_current_project_field();
                    self.submit_new_project()?;
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.save_current_project_field();
                    self.new_project_field = 1 - self.new_project_field;
                    self.load_current_project_field();
                }
                KeyCode::Esc => {
                    self.input_buffer.clear();
                    self.new_project_name.clear();
                    self.new_project_path.clear();
                    self.new_project_field = 0;
                    self.clear_path_autocomplete();
                    self.input_mode = InputMode::Normal;
                }
                _ => {
                    apply_text_edit(
                        &mut self.input_buffer,
                        &mut self.input_cursor,
                        code,
                        modifiers,
                    );
                }
            }
        }
        Ok(())
    }

    fn save_current_project_field(&mut self) {
        match self.new_project_field {
            0 => self.new_project_name.clone_from(&self.input_buffer),
            _ => self.new_project_path.clone_from(&self.input_buffer),
        }
    }

    fn submit_new_project(&mut self) -> Result<()> {
        if !self.new_project_name.is_empty() && !self.new_project_path.is_empty() {
            let name = self.new_project_name.clone();
            let path_to_resolve =
                Self::expand_tilde(&self.new_project_path).unwrap_or(self.new_project_path.clone());
            if let Ok(abs_path) = std::fs::canonicalize(&path_to_resolve)
                && let Some(abs_str) = abs_path.to_str()
            {
                let default_branch = crate::detect_default_branch(abs_str);
                self.store
                    .create_project(&self.new_project_name, abs_str, &default_branch)?;
            }
            self.new_project_name.clear();
            self.new_project_path.clear();
            self.new_project_field = 0;
            self.input_buffer.clear();
            self.clear_path_autocomplete();
            self.input_mode = InputMode::Normal;
            self.refresh_data()?;
            self.show_toast(format!("Project '{name}' created"), ToastStyle::Success);
        }
        Ok(())
    }

    fn load_current_project_field(&mut self) {
        match self.new_project_field {
            0 => self.input_buffer.clone_from(&self.new_project_name),
            _ => self.input_buffer.clone_from(&self.new_project_path),
        }
        self.input_cursor = self.input_buffer.len();
    }

    /// Expand `~` prefix to home directory in the given path string.
    fn expand_tilde(raw: &str) -> Option<String> {
        if let Some(rest) = raw.strip_prefix('~') {
            let home = dirs::home_dir()?;
            Some(home.to_string_lossy().to_string() + rest)
        } else {
            Some(raw.to_string())
        }
    }

    fn update_path_suggestions(&mut self) {
        let Some(expanded) = Self::expand_tilde(&self.input_buffer) else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        // Split into base directory and partial name
        let (base_dir, partial) = if expanded.ends_with('/') {
            (expanded.as_str(), "")
        } else if let Some(pos) = expanded.rfind('/') {
            (&expanded[..=pos], &expanded[pos + 1..])
        } else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        let partial_lower = partial.to_lowercase();

        let Ok(entries) = std::fs::read_dir(base_dir) else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        let mut suggestions: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_type().is_ok_and(|ft| ft.is_dir())
                    && !e.file_name().to_string_lossy().starts_with('.')
            })
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| partial.is_empty() || name.to_lowercase().starts_with(&partial_lower))
            .collect();

        suggestions.sort_unstable();

        self.show_path_suggestions = !suggestions.is_empty();
        self.path_suggestions = suggestions;
        self.path_suggestion_index = 0;
    }

    fn accept_path_suggestion(&mut self) {
        let Some(suggestion) = self
            .path_suggestions
            .get(self.path_suggestion_index)
            .cloned()
        else {
            return;
        };

        let Some(expanded) = Self::expand_tilde(&self.input_buffer) else {
            return;
        };

        let base = if expanded.ends_with('/') {
            expanded
        } else if let Some(pos) = expanded.rfind('/') {
            expanded[..=pos].to_string()
        } else {
            return;
        };

        // Reconstruct with ~ if original started with ~
        let new_path = if self.input_buffer.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                let home_str = home.to_string_lossy().to_string();
                if let Some(rest) = base.strip_prefix(&home_str) {
                    format!("~{rest}{suggestion}/")
                } else {
                    format!("{base}{suggestion}/")
                }
            } else {
                format!("{base}{suggestion}/")
            }
        } else {
            format!("{base}{suggestion}/")
        };

        self.input_buffer = new_path;
        self.input_cursor = self.input_buffer.len();
        self.update_path_suggestions();
    }

    fn clear_path_autocomplete(&mut self) {
        self.path_suggestions.clear();
        self.path_suggestion_index = 0;
        self.show_path_suggestions = false;
    }

    /// Update or clear path autocomplete after a deletion in the path field.
    fn refresh_path_autocomplete_after_delete(&mut self) {
        if self.show_path_suggestions {
            if self.input_buffer.contains('/') || self.input_buffer == "~" {
                self.update_path_suggestions();
            } else {
                self.clear_path_autocomplete();
            }
        }
    }

    fn reset_task_form(&mut self) {
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.new_task_description.clear();
        self.new_task_mode = crate::store::TaskMode::Autonomous;
        self.new_task_field = 0;
        self.new_task_subtasks.clear();
        self.new_task_subtask_index = 0;
        self.editing_subtask_index = None;
    }

    fn handle_confirm_delete_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Char('y') => {
                if !self.confirm_entity_id.is_empty() {
                    let name = self.confirm_target.clone();
                    match self.confirm_delete_kind {
                        DeleteTarget::Project => {
                            self.store.delete_project(&self.confirm_entity_id)?;
                            self.project_index = 0;
                            self.show_toast(
                                format!("Project '{name}' deleted"),
                                ToastStyle::Success,
                            );
                        }
                        DeleteTarget::Task => {
                            // Spawn teardown in background if task has a linked session
                            if let Ok(task) = self.store.get_task(&self.confirm_entity_id)
                                && let Some(ref sid) = task.session_id
                            {
                                self.spawn_teardown_session(sid.clone());
                            }
                            self.store.delete_task(&self.confirm_entity_id)?;
                            self.show_toast(format!("Task '{name}' deleted"), ToastStyle::Success);
                        }
                    }
                    self.confirm_entity_id.clear();
                    self.confirm_target.clear();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.confirm_entity_id.clear();
                self.confirm_target.clear();
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn move_down(&mut self) {
        match self.focus {
            Focus::Projects => {
                if !self.projects.is_empty() {
                    self.project_index = (self.project_index + 1).min(self.projects.len() - 1);
                    // Auto-load sessions/tasks for newly selected project
                    let _ = self.refresh_data();
                    self.task_index = 0;
                }
            }
            Focus::Tasks => {
                let visible_count = self.visible_tasks().len();
                if visible_count > 0 {
                    self.task_index = (self.task_index + 1).min(visible_count - 1);
                }
            }
        }
    }

    fn move_up(&mut self) {
        match self.focus {
            Focus::Projects => {
                self.project_index = self.project_index.saturating_sub(1);
                let _ = self.refresh_data();
                self.task_index = 0;
            }
            Focus::Tasks => {
                self.task_index = self.task_index.saturating_sub(1);
            }
        }
    }

    fn handle_palette_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                if let Some(&idx) = self.palette_filtered.get(self.palette_index) {
                    let action = self.palette_items[idx].action;
                    self.input_buffer.clear();
                    self.input_mode = InputMode::Normal;
                    self.execute_palette_action(action)?;
                }
            }
            KeyCode::Up | KeyCode::Char('k') if self.input_buffer.is_empty() => {
                self.palette_index = self.palette_index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.input_buffer.is_empty() => {
                if !self.palette_filtered.is_empty() {
                    self.palette_index =
                        (self.palette_index + 1).min(self.palette_filtered.len() - 1);
                }
            }
            _ => {
                if apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                ) {
                    self.filter_palette();
                    self.palette_index = 0;
                }
            }
        }
        Ok(())
    }

    fn filter_palette(&mut self) {
        let query = self.input_buffer.to_lowercase();
        if query.is_empty() {
            self.palette_filtered = (0..self.palette_items.len()).collect();
        } else {
            self.palette_filtered = self
                .palette_items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.label.to_lowercase().contains(&query))
                .map(|(i, _)| i)
                .collect();
        }
    }

    fn execute_palette_action(&mut self, action: PaletteAction) -> Result<()> {
        match action {
            PaletteAction::NewTask => {
                if self.selected_project().is_some() {
                    self.reset_task_form();
                    self.input_mode = InputMode::NewTask;
                }
            }
            PaletteAction::AddProject => {
                self.input_mode = InputMode::NewProject;
                self.input_buffer.clear();
                self.new_project_name.clear();
                self.new_project_path = String::from(".");
                self.new_project_field = 0;
                self.clear_path_autocomplete();
            }
            PaletteAction::RemoveProject => {
                if let Some((name, id)) = self
                    .selected_project()
                    .map(|p| (p.name.clone(), p.id.clone()))
                {
                    self.confirm_target = name;
                    self.confirm_entity_id = id;
                    self.confirm_delete_kind = DeleteTarget::Project;
                    self.input_mode = InputMode::ConfirmDelete;
                }
            }
            PaletteAction::FocusProjects => self.focus = Focus::Projects,
            PaletteAction::FocusTasks => self.focus = Focus::Tasks,
            PaletteAction::FindSkills => {
                self.refresh_skills();
                self.input_mode = InputMode::SkillSearch;
                self.input_buffer.clear();
                self.search_results.clear();
            }
            PaletteAction::UpdateSkills => {
                self.skill_status_message = "Updating skills...".to_string();
                match crate::skills::update_skills() {
                    Ok(msg) => {
                        self.skill_status_message = msg;
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.skill_status_message = format!("Update failed: {e}");
                    }
                }
            }
            PaletteAction::Quit => self.should_quit = true,
        }
        Ok(())
    }

    pub fn refresh_skills(&mut self) {
        let mut all_skills = crate::skills::list_skills(true, None).unwrap_or_default();

        if let Some(project) = self.selected_project() {
            let project_skills =
                crate::skills::list_skills(false, Some(&project.repo_path)).unwrap_or_default();
            all_skills.extend(project_skills);
        }

        self.installed_skills = all_skills;

        if self.skill_index >= self.installed_skills.len() && !self.installed_skills.is_empty() {
            self.skill_index = self.installed_skills.len() - 1;
        }

        self.refresh_skill_detail();
    }

    fn refresh_skill_detail(&mut self) {
        if let Some(skill) = self.installed_skills.get(self.skill_index) {
            self.skill_detail_content = crate::skills::read_skill_md(&skill.path)
                .unwrap_or_else(|_| "Could not read SKILL.md".to_string());
        } else {
            self.skill_detail_content.clear();
        }
    }

    fn handle_skill_panel_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.installed_skills.is_empty() {
                    self.skill_index = (self.skill_index + 1).min(self.installed_skills.len() - 1);
                    self.refresh_skill_detail();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.skill_index = self.skill_index.saturating_sub(1);
                self.refresh_skill_detail();
            }
            KeyCode::Char('f') => {
                self.input_mode = InputMode::SkillSearch;
                self.input_buffer.clear();
                self.search_results.clear();
                self.skill_index = 0;
            }
            KeyCode::Char('a') => {
                self.input_mode = InputMode::SkillAdd;
                self.input_buffer.clear();
            }
            KeyCode::Char('x') => {
                if let Some(skill) = self.installed_skills.get(self.skill_index) {
                    let name = skill.name.clone();
                    let global = skill.scope == crate::skills::SkillScope::Global;
                    let project_path =
                        if let crate::skills::SkillScope::Project(ref p) = skill.scope {
                            Some(p.clone())
                        } else {
                            None
                        };
                    match crate::skills::remove_skill(&name, global, project_path.as_deref()) {
                        Ok(_) => {
                            self.show_toast(format!("Removed {name}"), ToastStyle::Success);
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.show_toast(format!("Remove failed: {e}"), ToastStyle::Error);
                        }
                    }
                }
            }
            KeyCode::Char('u') => {
                self.show_toast("Updating skills...", ToastStyle::Info);
                match crate::skills::update_skills() {
                    Ok(msg) => {
                        self.show_toast(msg, ToastStyle::Success);
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.show_toast(format!("Update failed: {e}"), ToastStyle::Error);
                    }
                }
            }
            KeyCode::Char('g') => {
                self.skill_scope_global = !self.skill_scope_global;
                self.refresh_skills();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_skill_search_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty() {
                    if self.search_results.is_empty() {
                        let query = self.input_buffer.clone();
                        self.skill_status_message = format!("Searching for '{query}'...");
                        match crate::skills::find_skills(&query) {
                            Ok(results) => {
                                self.skill_status_message =
                                    format!("Found {} results", results.len());
                                self.search_results = results;
                                self.skill_index = 0;
                            }
                            Err(e) => {
                                self.skill_status_message = format!("Search failed: {e}");
                            }
                        }
                    } else if let Some(result) = self.search_results.get(self.skill_index) {
                        let package = result.package.clone();
                        let global = self.skill_scope_global;
                        let project_path = if global {
                            None
                        } else {
                            self.selected_project().map(|p| p.repo_path.clone())
                        };

                        self.skill_status_message = format!("Installing {package}...");
                        match crate::skills::add_skill(&package, global, project_path.as_deref()) {
                            Ok(_) => {
                                self.skill_status_message = format!("Installed {package}");
                                self.input_mode = InputMode::SkillPanel;
                                self.input_buffer.clear();
                                self.search_results.clear();
                                self.refresh_skills();
                            }
                            Err(e) => {
                                self.skill_status_message = format!("Install failed: {e}");
                            }
                        }
                    }
                }
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.search_results.clear();
                self.input_mode = InputMode::SkillPanel;
                self.skill_status_message.clear();
            }
            KeyCode::Char('j') | KeyCode::Down if !self.search_results.is_empty() => {
                self.skill_index = (self.skill_index + 1).min(self.search_results.len() - 1);
            }
            KeyCode::Char('k') | KeyCode::Up if !self.search_results.is_empty() => {
                self.skill_index = self.skill_index.saturating_sub(1);
            }
            _ => {
                if apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                ) {
                    self.search_results.clear();
                    self.skill_status_message.clear();
                }
            }
        }
        Ok(())
    }

    fn handle_edit_task_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        if self.handle_task_form_shared_key(code, modifiers) {
            return Ok(());
        }
        match code {
            KeyCode::Enter => {
                self.save_current_task_field();
                if !self.new_task_description.is_empty() {
                    if let Some(ref task_id) = self.editing_task_id.clone() {
                        let fallback = fallback_title(&self.new_task_description);
                        self.store.update_task(
                            task_id,
                            &fallback,
                            &self.new_task_description,
                            self.new_task_mode,
                        )?;

                        // Promote draft → pending on submit
                        if let Ok(task) = self.store.get_task(task_id)
                            && task.status == crate::store::TaskStatus::Draft
                        {
                            self.store
                                .update_task_status(task_id, crate::store::TaskStatus::Pending)?;
                        }

                        // Create inline subtasks added during edit
                        for subtask_desc in &self.new_task_subtasks {
                            let st_title = fallback_title(subtask_desc);
                            self.store
                                .create_subtask(task_id, &st_title, subtask_desc)?;
                        }

                        // Launch autonomous tasks (generates title + auto-launches),
                        // or just generate the title for supervised tasks.
                        if self.new_task_mode == crate::store::TaskMode::Autonomous
                            && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                        {
                            self.launch_task(task_id.clone(), project_id)?;
                        } else {
                            self.spawn_title_generation(
                                task_id.clone(),
                                self.new_task_description.clone(),
                            );
                        }
                        self.show_toast("Task updated", ToastStyle::Success);
                    }
                    self.editing_task_id = None;
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc => {
                self.save_current_task_field();
                if !self.new_task_description.is_empty()
                    && let Some(ref task_id) = self.editing_task_id.clone()
                {
                    let fallback = fallback_title(&self.new_task_description);
                    self.store.update_task(
                        task_id,
                        &fallback,
                        &self.new_task_description,
                        self.new_task_mode,
                    )?;

                    // Create inline subtasks added during edit
                    for subtask_desc in &self.new_task_subtasks {
                        let st_title = fallback_title(subtask_desc);
                        self.store
                            .create_subtask(task_id, &st_title, subtask_desc)?;
                    }

                    self.spawn_title_generation(task_id.clone(), self.new_task_description.clone());
                    self.show_toast("Task draft saved", ToastStyle::Info);
                }
                self.editing_task_id = None;
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_task_filter_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        match code {
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                self.task_index = 0;
            }
            KeyCode::Esc => {
                self.task_filter.clear();
                self.recompute_visible_tasks();
                self.input_mode = InputMode::Normal;
                self.task_index = 0;
            }
            _ => {
                if apply_text_edit(
                    &mut self.task_filter,
                    &mut self.task_filter_cursor,
                    code,
                    modifiers,
                ) {
                    self.recompute_visible_tasks();
                    self.task_index = 0;
                }
            }
        }
        Ok(())
    }

    fn handle_subtask_panel_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty()
                    && let Some(task) = self.visible_tasks().get(self.task_index)
                {
                    let task_id = task.id.clone();
                    let desc = std::mem::take(&mut self.input_buffer);
                    let title = fallback_title(&desc);
                    self.store.create_subtask(&task_id, &title, &desc)?;
                    self.subtasks = self
                        .store
                        .list_subtasks_for_task(&task_id)
                        .unwrap_or_default();
                    self.show_toast("Subtask added", ToastStyle::Success);
                }
            }
            KeyCode::Char('d') if self.input_buffer.is_empty() => {
                if let Some(st) = self.subtasks.get(self.subtask_index) {
                    let st_id = st.id.clone();
                    let task_id = st.task_id.clone();
                    self.store.delete_subtask(&st_id)?;
                    self.subtasks = self
                        .store
                        .list_subtasks_for_task(&task_id)
                        .unwrap_or_default();
                    if self.subtask_index >= self.subtasks.len() && !self.subtasks.is_empty() {
                        self.subtask_index = self.subtasks.len() - 1;
                    }
                    self.show_toast("Subtask deleted", ToastStyle::Success);
                }
            }
            KeyCode::Char('j') | KeyCode::Down if self.input_buffer.is_empty() => {
                if !self.subtasks.is_empty() {
                    self.subtask_index = (self.subtask_index + 1).min(self.subtasks.len() - 1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.input_buffer.is_empty() => {
                self.subtask_index = self.subtask_index.saturating_sub(1);
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
            _ => {
                apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
            }
        }
        Ok(())
    }

    fn handle_skill_add_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty() {
                    let package = self.input_buffer.clone();
                    let global = self.skill_scope_global;
                    let project_path = if global {
                        None
                    } else {
                        self.selected_project().map(|p| p.repo_path.clone())
                    };

                    self.skill_status_message = format!("Installing {package}...");
                    match crate::skills::add_skill(&package, global, project_path.as_deref()) {
                        Ok(_) => {
                            self.skill_status_message = format!("Installed {package}");
                            self.input_mode = InputMode::SkillPanel;
                            self.input_buffer.clear();
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.skill_status_message = format!("Install failed: {e}");
                        }
                    }
                }
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = InputMode::SkillPanel;
            }
            _ => {
                apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
            }
        }
        Ok(())
    }
}

// Text editing helpers and format_with_cursor are in super::form.
use super::form::apply_text_edit;

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

    // Check CI status — any completed check with FAILURE/ERROR conclusion means CI failed
    if let Some(checks) = json["statusCheckRollup"].as_array() {
        let has_failure = checks.iter().any(|check| {
            let conclusion = check["conclusion"].as_str().unwrap_or("");
            conclusion.eq_ignore_ascii_case("FAILURE") || conclusion.eq_ignore_ascii_case("ERROR")
        });
        if has_failure {
            return PrStatus::CiFailed;
        }
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

    // Fetch usage from API
    let output = std::process::Command::new("curl")
        .args([
            "-s",
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
            "pct5h": usage["five_hour"]["utilization"],
            "pct7d": usage["seven_day"]["utilization"]
        }
    });

    let home = dirs::home_dir()?;
    let cache_path = home.join(".claude/statusline-cache.json");
    std::fs::write(cache_path, serde_json::to_string(&cache).ok()?).ok()?;

    Some(())
}

/// Quick fallback title by truncating the first line at a word boundary.
/// Used immediately when creating a task so the UI stays responsive.
fn fallback_title(prompt: &str) -> String {
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

/// Convert a crossterm key event into the raw bytes a terminal would send.
/// Stack-allocated key byte buffer (avoids heap allocation per keystroke).
/// Maximum escape sequence is 4 bytes (e.g. `\x1b[3~`), and max UTF-8 char is 4 bytes.
struct KeyBytes {
    buf: [u8; 8],
    len: usize,
}

impl KeyBytes {
    const fn empty() -> Self {
        Self {
            buf: [0; 8],
            len: 0,
        }
    }

    fn from_slice(s: &[u8]) -> Self {
        let mut buf = [0u8; 8];
        let len = s.len().min(8);
        buf[..len].copy_from_slice(&s[..len]);
        Self { buf, len }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

fn keycode_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> KeyBytes {
    // Ctrl modifier — map Ctrl+letter to the control character
    if modifiers.contains(KeyModifiers::CONTROL)
        && let KeyCode::Char(c) = code
    {
        let ctrl = (c.to_ascii_lowercase() as u8)
            .wrapping_sub(b'a')
            .wrapping_add(1);
        return KeyBytes {
            buf: [ctrl, 0, 0, 0, 0, 0, 0, 0],
            len: 1,
        };
    }

    // Alt modifier — prefix the key's normal bytes with ESC (\x1b).
    // This is the standard terminal convention for Alt/Option key combos
    // (e.g. Alt+Backspace → \x1b\x7f = backward-kill-word in readline/zsh).
    if modifiers.contains(KeyModifiers::ALT) {
        let base = keycode_to_bytes_base(code);
        if base.len > 0 {
            let mut buf = [0u8; 8];
            buf[0] = 0x1b;
            let copy_len = base.len.min(7);
            buf[1..=copy_len].copy_from_slice(&base.buf[..copy_len]);
            return KeyBytes {
                buf,
                len: 1 + copy_len,
            };
        }
        return KeyBytes::empty();
    }

    keycode_to_bytes_base(code)
}

/// Map a keycode (without modifiers) to its raw terminal bytes.
///
/// Philosophy: forward ALL byte-producing keys to the PTY by default.
/// Only keys intercepted earlier in `handle_session_tab_key` are excluded.
/// Non-byte-producing keys (modifier-only, media, etc.) return empty.
fn keycode_to_bytes_base(code: KeyCode) -> KeyBytes {
    match code {
        KeyCode::Char(c) => {
            let mut buf = [0u8; 8];
            let s = c.encode_utf8(&mut buf[..4]);
            let len = s.len();
            KeyBytes { buf, len }
        }
        KeyCode::Esc => KeyBytes::from_slice(b"\x1b"),
        KeyCode::Enter => KeyBytes::from_slice(b"\r"),
        KeyCode::Backspace => KeyBytes::from_slice(&[0x7f]),
        KeyCode::Tab => KeyBytes::from_slice(b"\t"),
        KeyCode::BackTab => KeyBytes::from_slice(b"\x1b[Z"),
        KeyCode::Up => KeyBytes::from_slice(b"\x1b[A"),
        KeyCode::Down => KeyBytes::from_slice(b"\x1b[B"),
        KeyCode::Right => KeyBytes::from_slice(b"\x1b[C"),
        KeyCode::Left => KeyBytes::from_slice(b"\x1b[D"),
        KeyCode::Home => KeyBytes::from_slice(b"\x1b[H"),
        KeyCode::End => KeyBytes::from_slice(b"\x1b[F"),
        KeyCode::Insert => KeyBytes::from_slice(b"\x1b[2~"),
        KeyCode::Delete => KeyBytes::from_slice(b"\x1b[3~"),
        KeyCode::PageUp => KeyBytes::from_slice(b"\x1b[5~"),
        KeyCode::PageDown => KeyBytes::from_slice(b"\x1b[6~"),
        KeyCode::Null => KeyBytes::from_slice(&[0x00]),
        KeyCode::F(n) => match n {
            1 => KeyBytes::from_slice(b"\x1bOP"),
            2 => KeyBytes::from_slice(b"\x1bOQ"),
            3 => KeyBytes::from_slice(b"\x1bOR"),
            4 => KeyBytes::from_slice(b"\x1bOS"),
            5 => KeyBytes::from_slice(b"\x1b[15~"),
            6 => KeyBytes::from_slice(b"\x1b[17~"),
            7 => KeyBytes::from_slice(b"\x1b[18~"),
            8 => KeyBytes::from_slice(b"\x1b[19~"),
            9 => KeyBytes::from_slice(b"\x1b[20~"),
            10 => KeyBytes::from_slice(b"\x1b[21~"),
            11 => KeyBytes::from_slice(b"\x1b[23~"),
            12 => KeyBytes::from_slice(b"\x1b[24~"),
            _ => KeyBytes::empty(),
        },
        // Modifier-only keys, media keys, etc. don't produce terminal bytes
        _ => KeyBytes::empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Store, TaskMode, TaskStatus};
    use crossterm::event::{KeyCode, KeyModifiers};

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
            )
            .unwrap();
        store
            .create_task(
                &project.id,
                "Task Beta",
                "Second task",
                TaskMode::Autonomous,
            )
            .unwrap();
        store
            .create_task(
                &project.id,
                "Task Gamma",
                "Third task",
                TaskMode::Supervised,
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
        terminal.draw(|frame| super::ui::draw(frame, app)).unwrap();

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
            .create_task(&p1.id, "alpha-task", "", TaskMode::Supervised)
            .unwrap();
        store
            .create_task(&p2.id, "beta-task", "", TaskMode::Supervised)
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
        // Toggle mode
        press(&mut app, KeyCode::Left);
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
    fn visible_tasks_excludes_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::Done)
            .unwrap();
        app.refresh_data().unwrap();

        let visible = app.visible_tasks();
        assert!(!visible.iter().any(|t| t.status == TaskStatus::Done));
        // Done task should be excluded
        assert_eq!(visible.len(), app.tasks.len() - 1);
    }

    #[test]
    fn visible_tasks_sorted_by_status_priority() {
        let mut app = test_app_with_tasks();
        // Alpha=Pending, Beta=Pending, Gamma=Pending initially.
        // Set each to a different status.
        let alpha_id = app.tasks[0].id.clone();
        let beta_id = app.tasks[1].id.clone();

        app.store
            .update_task_status(&alpha_id, TaskStatus::Error)
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

        // BackTab wraps to field 2 (subtasks)
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
        assert_eq!(app.new_task_field, 0);
    }

    #[test]
    fn task_form_mode_toggle() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);

        press(&mut app, KeyCode::Left);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);

        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);
    }

    #[test]
    fn edit_task_form_cycling() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('2'));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::EditTask);

        // Tab cycles through prompt (0), mode (1), subtasks (2)
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 2);
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
        // Set varied task statuses
        let t0 = app.tasks[0].id.clone();
        let t1 = app.tasks[1].id.clone();
        app.store
            .update_task_status(&t0, TaskStatus::Working)
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
}
