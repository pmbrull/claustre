use std::time::Duration;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::DefaultTerminal;

use std::collections::HashMap;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use crate::store::{Project, Session, Store, Task};

use super::event::{self, AppEvent};
use super::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Active,
    History,
    Skills,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Sessions,
    Tasks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastStyle {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    NewTask,
    EditTask,
    NewSession,
    NewProject,
    ConfirmDelete,
    CommandPalette,
    SkillSearch,
    SkillAdd,
    HelpOverlay,
    TaskFilter,
    SubtaskPanel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteTarget {
    Project,
    Session,
    Task,
}

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub label: String,
    pub action: PaletteAction,
}

#[derive(Debug, Clone, Copy)]
pub enum PaletteAction {
    NewTask,
    NewSession,
    AddProject,
    RemoveProject,
    ToggleView,
    FocusProjects,
    FocusSessions,
    FocusTasks,
    FindSkills,
    UpdateSkills,
    Quit,
}

/// Pre-fetched per-project summary for the sidebar (avoids DB queries during rendering).
#[derive(Debug, Clone, Default)]
pub struct ProjectSummary {
    pub active_sessions: Vec<Session>,
    pub has_review: bool,
}

pub struct App {
    pub store: Store,
    pub should_quit: bool,
    pub view: View,
    pub focus: Focus,
    pub input_mode: InputMode,

    // Data
    pub projects: Vec<Project>,
    pub sessions: Vec<Session>,
    pub tasks: Vec<Task>,

    // Pre-fetched sidebar data (project_id -> summary)
    pub project_summaries: HashMap<String, ProjectSummary>,

    // Selection indices
    pub project_index: usize,
    pub session_index: usize,
    pub task_index: usize,

    // Input buffer for new task creation
    pub input_buffer: String,

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

    // Subtask state
    pub subtasks: Vec<crate::store::Subtask>,
    pub subtask_index: usize,
    pub subtask_counts: HashMap<String, (i64, i64)>,

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

    // Toast notification
    pub toast_message: Option<String>,
    pub toast_style: ToastStyle,
    pub toast_expires: Option<std::time::Instant>,
}

impl App {
    pub fn new(store: Store) -> Result<Self> {
        let projects = store.list_projects()?;

        let (sessions, tasks) = if let Some(project) = projects.first() {
            let sessions = store.list_active_sessions_for_project(&project.id)?;
            let tasks = store.list_tasks_for_project(&project.id)?;
            (sessions, tasks)
        } else {
            (vec![], vec![])
        };

        let palette_items = vec![
            PaletteItem {
                label: "New Task".into(),
                action: PaletteAction::NewTask,
            },
            PaletteItem {
                label: "New Session".into(),
                action: PaletteAction::NewSession,
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
                label: "Toggle View (Active/History)".into(),
                action: PaletteAction::ToggleView,
            },
            PaletteItem {
                label: "Focus Projects".into(),
                action: PaletteAction::FocusProjects,
            },
            PaletteItem {
                label: "Focus Sessions".into(),
                action: PaletteAction::FocusSessions,
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
        let (tx, rx) = mpsc::channel();

        Ok(App {
            store,
            should_quit: false,
            view: View::Active,
            focus: Focus::Projects,
            input_mode: InputMode::Normal,
            projects,
            sessions,
            tasks,
            project_summaries,
            project_index: 0,
            session_index: 0,
            task_index: 0,
            input_buffer: String::new(),
            new_task_field: 0,
            new_task_description: String::new(),
            new_task_mode: crate::store::TaskMode::Supervised,
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
            subtasks: vec![],
            subtask_index: 0,
            subtask_counts: HashMap::new(),
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
            toast_message: None,
            toast_style: ToastStyle::Info,
            toast_expires: None,
        })
    }

    pub fn refresh_data(&mut self) -> Result<()> {
        self.projects = self.store.list_projects()?;

        if let Some(project) = self.projects.get(self.project_index) {
            self.sessions = self.store.list_active_sessions_for_project(&project.id)?;
            self.tasks = self.store.list_tasks_for_project(&project.id)?;
        } else {
            self.sessions.clear();
            self.tasks.clear();
        }

        // Pre-fetch sidebar summaries for all projects
        self.project_summaries = build_project_summaries(&self.store, &self.projects);

        // Pre-fetch subtask counts for visible tasks
        self.subtask_counts.clear();
        for task in &self.tasks {
            if let Ok(counts) = self.store.subtask_count(&task.id)
                && counts.0 > 0
            {
                self.subtask_counts.insert(task.id.clone(), counts);
            }
        }

        // Clamp indices
        if self.project_index >= self.projects.len() && !self.projects.is_empty() {
            self.project_index = self.projects.len() - 1;
        }
        if self.session_index >= self.sessions.len() && !self.sessions.is_empty() {
            self.session_index = self.sessions.len() - 1;
        }
        let visible_count = self.visible_tasks().len();
        if self.task_index >= visible_count && visible_count > 0 {
            self.task_index = visible_count - 1;
        } else if visible_count == 0 {
            self.task_index = 0;
        }

        // Refresh subtasks for selected task
        let visible = self.visible_tasks();
        if let Some(task) = visible.get(self.task_index) {
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
    fn poll_title_results(&mut self) -> Result<()> {
        while let Ok((task_id, title)) = self.title_rx.try_recv() {
            self.pending_titles.remove(&task_id);
            self.store.update_task_title(&task_id, &title)?;
        }
        Ok(())
    }

    pub fn show_toast(&mut self, message: impl Into<String>, style: ToastStyle) {
        self.toast_message = Some(message.into());
        self.toast_style = style;
        self.toast_expires = Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
    }

    fn tick_toast(&mut self) {
        if let Some(expires) = self.toast_expires
            && std::time::Instant::now() > expires
        {
            self.toast_message = None;
            self.toast_expires = None;
        }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_index)
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.session_index)
    }

    /// Returns the tasks visible in the current view.
    /// Active view filters out Done tasks; other views show all.
    /// Also applies the `task_filter` if non-empty.
    pub fn visible_tasks(&self) -> Vec<&Task> {
        let filter_lower = self.task_filter.to_lowercase();
        self.tasks
            .iter()
            .filter(|t| {
                if self.view == View::Active && t.status == crate::store::TaskStatus::Done {
                    return false;
                }
                if !filter_lower.is_empty() && !t.title.to_lowercase().contains(&filter_lower) {
                    return false;
                }
                true
            })
            .collect()
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let tick_rate = Duration::from_millis(250);

        loop {
            terminal.draw(|frame| ui::draw(frame, self))?;

            match event::poll(tick_rate)? {
                AppEvent::Key(key) => match self.input_mode {
                    InputMode::Normal => {
                        if self.view == View::Skills {
                            self.handle_skills_key(key.code, key.modifiers)?;
                        } else {
                            self.handle_normal_key(key.code, key.modifiers)?;
                        }
                    }
                    InputMode::NewTask => self.handle_input_key(key.code)?,
                    InputMode::EditTask => self.handle_edit_task_key(key.code)?,
                    InputMode::NewSession => self.handle_session_input_key(key.code)?,
                    InputMode::NewProject => self.handle_new_project_key(key.code)?,
                    InputMode::ConfirmDelete => self.handle_confirm_delete_key(key.code)?,
                    InputMode::CommandPalette => self.handle_palette_key(key.code)?,
                    InputMode::SkillSearch => self.handle_skill_search_key(key.code)?,
                    InputMode::SkillAdd => self.handle_skill_add_key(key.code)?,
                    InputMode::HelpOverlay => {
                        if matches!(key.code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                            self.input_mode = InputMode::Normal;
                        }
                    }
                    InputMode::TaskFilter => self.handle_task_filter_key(key.code)?,
                    InputMode::SubtaskPanel => self.handle_subtask_panel_key(key.code)?,
                },
                AppEvent::Tick => {
                    self.tick_toast();
                    self.poll_title_results()?;
                    // Periodic refresh for MCP updates
                    self.refresh_data()?;
                }
            }

            if self.should_quit {
                return Ok(());
            }
        }
    }

    fn handle_normal_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        match (code, modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }

            // Command palette
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                self.input_mode = InputMode::CommandPalette;
                self.input_buffer.clear();
                self.palette_index = 0;
                self.filter_palette();
            }

            // View toggle
            (KeyCode::Tab, _) => {
                self.view = match self.view {
                    View::Active => View::History,
                    View::History => View::Skills,
                    View::Skills => View::Active,
                };
                if self.view == View::Skills {
                    self.refresh_skills();
                }
            }

            // Focus switching
            (KeyCode::Char('1'), _) => self.focus = Focus::Projects,
            (KeyCode::Char('2'), _) => self.focus = Focus::Sessions,
            (KeyCode::Char('3'), _) => self.focus = Focus::Tasks,

            // Help overlay
            (KeyCode::Char('?'), _) => {
                self.input_mode = InputMode::HelpOverlay;
            }

            // Task filter
            (KeyCode::Char('/'), _) => {
                self.task_filter.clear();
                self.input_mode = InputMode::TaskFilter;
                self.focus = Focus::Tasks;
            }

            // Navigation
            (KeyCode::Char('j') | KeyCode::Down, _) => self.move_down(),
            (KeyCode::Char('k') | KeyCode::Up, _) => self.move_up(),

            // Task reorder (Shift+J/K)
            (KeyCode::Char('J'), _) => {
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
            (KeyCode::Char('K'), _) => {
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

            // Enter: context-dependent
            (KeyCode::Enter, _) => {
                match self.focus {
                    Focus::Projects => {
                        self.refresh_data()?;
                        self.session_index = 0;
                        self.task_index = 0;
                    }
                    Focus::Sessions => {
                        // Jump to the Zellij tab for this session
                        if let Some(session) = self.selected_session()
                            && let Err(e) = crate::session::goto_session(session)
                        {
                            self.show_toast(
                                format!("Failed to switch session: {e}"),
                                ToastStyle::Error,
                            );
                        }
                    }
                    Focus::Tasks => {}
                }
            }

            // Subtask panel (when Tasks focused) or New session (otherwise)
            (KeyCode::Char('s'), _) => {
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
                } else if self.selected_project().is_some() {
                    self.input_mode = InputMode::NewSession;
                    self.input_buffer.clear();
                }
            }

            // New task
            (KeyCode::Char('n'), _) => {
                if self.selected_project().is_some() {
                    self.reset_task_form();
                    self.input_mode = InputMode::NewTask;
                }
            }

            // Edit task
            (KeyCode::Char('e'), _) => {
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
                        && status == crate::store::TaskStatus::Pending
                    {
                        self.editing_task_id = Some(id);
                        self.new_task_description.clone_from(&desc);
                        self.new_task_mode = mode;
                        self.new_task_field = 0;
                        self.input_buffer.clone_from(&desc);
                        self.input_mode = InputMode::EditTask;
                    }
                }
            }

            // Review task (mark in_review → done)
            (KeyCode::Char('r'), _) => {
                if self.focus == Focus::Tasks
                    && let Some(task) = self.visible_tasks().get(self.task_index).copied()
                    && matches!(
                        task.status,
                        crate::store::TaskStatus::InReview | crate::store::TaskStatus::InProgress
                    )
                {
                    // Teardown the linked session (worktree + Zellij tab) if one exists
                    if let Some(ref sid) = task.session_id {
                        let _ = crate::session::teardown_session(&self.store, sid);
                    }
                    self.store
                        .update_task_status(&task.id, crate::store::TaskStatus::Done)?;
                    self.refresh_data()?;
                    self.show_toast("Task marked as done", ToastStyle::Success);
                }
            }

            // Open PR URL in browser
            (KeyCode::Char('o'), _) => {
                if self.focus == Focus::Tasks
                    && let Some(task) = self.visible_tasks().get(self.task_index).copied()
                    && let Some(ref url) = task.pr_url
                {
                    let _ = std::process::Command::new("open").arg(url).spawn();
                    self.show_toast("Opening PR in browser", ToastStyle::Success);
                }
            }

            // Launch task (auto-create session with generated branch)
            (KeyCode::Char('l'), _) => {
                let task_id = if self.focus == Focus::Tasks {
                    self.visible_tasks()
                        .get(self.task_index)
                        .filter(|t| t.status == crate::store::TaskStatus::Pending)
                        .map(|t| t.id.clone())
                } else {
                    None
                };
                if let Some(task_id) = task_id
                    && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                {
                    let task = self.store.get_task(&task_id)?;
                    let branch_name = crate::session::generate_branch_name(&task.title);
                    match crate::session::create_session(
                        &self.store,
                        &project_id,
                        &branch_name,
                        Some(&task),
                    ) {
                        Ok(_session) => {
                            self.refresh_data()?;
                            self.show_toast("Session launched", ToastStyle::Success);
                        }
                        Err(e) => {
                            self.show_toast(format!("Launch failed: {e}"), ToastStyle::Error);
                        }
                    }
                }
            }

            // Delete (with confirmation) — universal across all panels
            (KeyCode::Char('d'), _) => match self.focus {
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
                Focus::Sessions => {
                    if let Some((name, id)) = self
                        .selected_session()
                        .map(|s| (s.zellij_tab_name.clone(), s.id.clone()))
                    {
                        self.confirm_target = name;
                        self.confirm_entity_id = id;
                        self.confirm_delete_kind = DeleteTarget::Session;
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

            // Add project
            (KeyCode::Char('a'), _) => {
                self.input_mode = InputMode::NewProject;
                self.input_buffer.clear();
                self.new_project_name.clear();
                self.new_project_path = String::from(".");
                self.new_project_field = 0;
                self.clear_path_autocomplete();
            }

            _ => {}
        }
        Ok(())
    }

    /// Handle keys shared between new-task and edit-task forms (tab, back-tab, mode toggle, typing).
    /// Returns `true` if the key was consumed.
    fn handle_task_form_shared_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Tab => {
                self.save_current_task_field();
                self.new_task_field = (self.new_task_field + 1) % 2;
                self.load_current_task_field();
                true
            }
            KeyCode::BackTab => {
                self.save_current_task_field();
                self.new_task_field = u8::from(self.new_task_field == 0);
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
            KeyCode::Char(c) if self.new_task_field == 0 => {
                self.input_buffer.push(c);
                true
            }
            KeyCode::Backspace if self.new_task_field == 0 => {
                self.input_buffer.pop();
                true
            }
            _ => false,
        }
    }

    fn handle_input_key(&mut self, code: KeyCode) -> Result<()> {
        if self.handle_task_form_shared_key(code) {
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

                        // Spawn background AI title generation
                        self.spawn_title_generation(
                            task.id.clone(),
                            self.new_task_description.clone(),
                        );

                        // Auto-launch autonomous tasks immediately
                        if self.new_task_mode == crate::store::TaskMode::Autonomous {
                            let branch_name = crate::session::generate_branch_name(&task.title);
                            match crate::session::create_session(
                                &self.store,
                                &project_id,
                                &branch_name,
                                Some(&task),
                            ) {
                                Ok(_) => {
                                    self.show_toast(
                                        "Autonomous task launched",
                                        ToastStyle::Success,
                                    );
                                }
                                Err(e) => {
                                    self.show_toast(
                                        format!("Auto-launch failed: {e}"),
                                        ToastStyle::Error,
                                    );
                                }
                            }
                        }
                    }
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc => {
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
        } else {
            self.input_buffer.clear();
        }
    }

    fn handle_session_input_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty()
                    && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                {
                    let branch_name = std::mem::take(&mut self.input_buffer);
                    self.input_mode = InputMode::Normal;

                    crate::session::create_session(&self.store, &project_id, &branch_name, None)?;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_new_project_key(&mut self, code: KeyCode) -> Result<()> {
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
                KeyCode::Char(c) => {
                    self.input_buffer.push(c);
                    if c == '/'
                        || (c == '~' && self.input_buffer == "~")
                        || self.show_path_suggestions
                    {
                        self.update_path_suggestions();
                    }
                }
                KeyCode::Backspace => {
                    self.input_buffer.pop();
                    if self.show_path_suggestions {
                        if self.input_buffer.contains('/') || self.input_buffer == "~" {
                            self.update_path_suggestions();
                        } else {
                            self.clear_path_autocomplete();
                        }
                    }
                }
                _ => {}
            }
        } else {
            // Name field (field 0) — original behavior
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
                KeyCode::Char(c) => {
                    self.input_buffer.push(c);
                }
                KeyCode::Backspace => {
                    self.input_buffer.pop();
                }
                _ => {}
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
                self.store.create_project(&self.new_project_name, abs_str)?;
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
        self.update_path_suggestions();
    }

    fn clear_path_autocomplete(&mut self) {
        self.path_suggestions.clear();
        self.path_suggestion_index = 0;
        self.show_path_suggestions = false;
    }

    fn reset_task_form(&mut self) {
        self.input_buffer.clear();
        self.new_task_description.clear();
        self.new_task_mode = crate::store::TaskMode::Supervised;
        self.new_task_field = 0;
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
                        DeleteTarget::Session => {
                            match crate::session::teardown_session(
                                &self.store,
                                &self.confirm_entity_id,
                            ) {
                                Ok(()) => {
                                    self.show_toast(
                                        format!("Session '{name}' torn down"),
                                        ToastStyle::Success,
                                    );
                                }
                                Err(e) => {
                                    self.show_toast(
                                        format!("Teardown failed: {e}"),
                                        ToastStyle::Error,
                                    );
                                }
                            }
                        }
                        DeleteTarget::Task => {
                            // Teardown the linked session (worktree + Zellij tab) if one exists
                            if let Ok(task) = self.store.get_task(&self.confirm_entity_id)
                                && let Some(ref sid) = task.session_id
                            {
                                let _ = crate::session::teardown_session(&self.store, sid);
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
                    self.session_index = 0;
                    self.task_index = 0;
                }
            }
            Focus::Sessions => {
                if !self.sessions.is_empty() {
                    self.session_index = (self.session_index + 1).min(self.sessions.len() - 1);
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
                self.session_index = 0;
                self.task_index = 0;
            }
            Focus::Sessions => {
                self.session_index = self.session_index.saturating_sub(1);
            }
            Focus::Tasks => {
                self.task_index = self.task_index.saturating_sub(1);
            }
        }
    }

    fn handle_palette_key(&mut self, code: KeyCode) -> Result<()> {
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
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
                self.filter_palette();
                self.palette_index = 0;
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                self.filter_palette();
                self.palette_index = 0;
            }
            _ => {}
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
            PaletteAction::NewSession => {
                if self.selected_project().is_some() {
                    self.input_mode = InputMode::NewSession;
                    self.input_buffer.clear();
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
            PaletteAction::ToggleView => {
                self.view = match self.view {
                    View::Active => View::History,
                    View::History => View::Skills,
                    View::Skills => View::Active,
                };
                if self.view == View::Skills {
                    self.refresh_skills();
                }
            }
            PaletteAction::FocusProjects => self.focus = Focus::Projects,
            PaletteAction::FocusSessions => self.focus = Focus::Sessions,
            PaletteAction::FocusTasks => self.focus = Focus::Tasks,
            PaletteAction::FindSkills => {
                self.view = View::Skills;
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

    fn handle_skills_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
        match (code, modifiers) {
            (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }

            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                self.input_mode = InputMode::CommandPalette;
                self.input_buffer.clear();
                self.palette_index = 0;
                self.filter_palette();
            }

            (KeyCode::Tab, _) => {
                self.view = View::Active;
            }

            (KeyCode::Char('j') | KeyCode::Down, _) => {
                if !self.installed_skills.is_empty() {
                    self.skill_index = (self.skill_index + 1).min(self.installed_skills.len() - 1);
                    self.refresh_skill_detail();
                }
            }
            (KeyCode::Char('k') | KeyCode::Up, _) => {
                self.skill_index = self.skill_index.saturating_sub(1);
                self.refresh_skill_detail();
            }

            (KeyCode::Char('f'), _) => {
                self.input_mode = InputMode::SkillSearch;
                self.input_buffer.clear();
                self.search_results.clear();
                self.skill_index = 0;
            }

            (KeyCode::Char('a'), _) => {
                self.input_mode = InputMode::SkillAdd;
                self.input_buffer.clear();
            }

            (KeyCode::Char('x'), _) => {
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
                            self.skill_status_message = format!("Removed {name}");
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.skill_status_message = format!("Remove failed: {e}");
                        }
                    }
                }
            }

            (KeyCode::Char('u'), _) => {
                self.skill_status_message = "Updating skills...".to_string();
                match crate::skills::update_skills() {
                    Ok(_) => {
                        self.skill_status_message = "Skills updated".to_string();
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.skill_status_message = format!("Update failed: {e}");
                    }
                }
            }

            (KeyCode::Char('g'), _) => {
                self.skill_scope_global = !self.skill_scope_global;
            }

            (KeyCode::Char('?'), _) => {
                self.input_mode = InputMode::HelpOverlay;
            }

            _ => {}
        }
        Ok(())
    }

    fn handle_skill_search_key(&mut self, code: KeyCode) -> Result<()> {
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
                                self.input_mode = InputMode::Normal;
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
                self.input_mode = InputMode::Normal;
                self.skill_status_message.clear();
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
                self.search_results.clear();
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
                self.search_results.clear();
            }
            KeyCode::Down => {
                if !self.search_results.is_empty() {
                    self.skill_index = (self.skill_index + 1).min(self.search_results.len() - 1);
                }
            }
            KeyCode::Up => {
                self.skill_index = self.skill_index.saturating_sub(1);
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_edit_task_key(&mut self, code: KeyCode) -> Result<()> {
        if self.handle_task_form_shared_key(code) {
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
                        // Spawn background AI title generation
                        self.spawn_title_generation(
                            task_id.clone(),
                            self.new_task_description.clone(),
                        );
                        self.show_toast("Task updated", ToastStyle::Success);
                    }
                    self.editing_task_id = None;
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc => {
                self.editing_task_id = None;
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_task_filter_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                self.task_index = 0;
            }
            KeyCode::Esc => {
                self.task_filter.clear();
                self.input_mode = InputMode::Normal;
                self.task_index = 0;
            }
            KeyCode::Char(c) => {
                self.task_filter.push(c);
                self.task_index = 0;
            }
            KeyCode::Backspace => {
                self.task_filter.pop();
                self.task_index = 0;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_subtask_panel_key(&mut self, code: KeyCode) -> Result<()> {
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
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_skill_add_key(&mut self, code: KeyCode) -> Result<()> {
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
                            self.input_mode = InputMode::Normal;
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
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            _ => {}
        }
        Ok(())
    }
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
        let truncated = &first_line[..60];
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
    let system = "You are a task title generator. Given a task prompt, output ONLY a concise title (max 8 words). No quotes, no punctuation at the end, no explanation.";
    let msg = format!("Generate a title for this task:\n{prompt}");

    if let Ok(output) = std::process::Command::new("claude")
        .args(["-p", "--model", "haiku", "--system-prompt", system, &msg])
        .output()
        && output.status.success()
    {
        let title = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !title.is_empty() {
            return title;
        }
    }

    fallback_title(prompt)
}

fn build_project_summaries(store: &Store, projects: &[Project]) -> HashMap<String, ProjectSummary> {
    let mut summaries = HashMap::with_capacity(projects.len());
    for project in projects {
        let active_sessions = store
            .list_active_sessions_for_project(&project.id)
            .unwrap_or_default();
        let has_review = store.has_review_tasks(&project.id).unwrap_or(false);
        summaries.insert(
            project.id.clone(),
            ProjectSummary {
                active_sessions,
                has_review,
            },
        );
    }
    summaries
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
            .create_project("test-project", "/tmp/test-repo")
            .unwrap();
        App::new(store).unwrap()
    }

    fn test_app_with_tasks() -> App {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("test-project", "/tmp/test-repo")
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
                if app.view == View::Skills {
                    app.handle_skills_key(code, modifiers).unwrap();
                } else {
                    app.handle_normal_key(code, modifiers).unwrap();
                }
            }
            InputMode::NewTask => app.handle_input_key(code).unwrap(),
            InputMode::EditTask => app.handle_edit_task_key(code).unwrap(),
            InputMode::NewSession => app.handle_session_input_key(code).unwrap(),
            InputMode::NewProject => app.handle_new_project_key(code).unwrap(),
            InputMode::ConfirmDelete => app.handle_confirm_delete_key(code).unwrap(),
            InputMode::CommandPalette => app.handle_palette_key(code).unwrap(),
            InputMode::SkillSearch => app.handle_skill_search_key(code).unwrap(),
            InputMode::SkillAdd => app.handle_skill_add_key(code).unwrap(),
            InputMode::HelpOverlay => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                    app.input_mode = InputMode::Normal;
                }
            }
            InputMode::TaskFilter => app.handle_task_filter_key(code).unwrap(),
            InputMode::SubtaskPanel => app.handle_subtask_panel_key(code).unwrap(),
        }
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    /// Render the app to a test buffer and return the content as a string.
    #[allow(deprecated)]
    fn render_to_string(app: &App, width: u16, height: u16) -> String {
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
    fn view_cycling_with_tab() {
        let mut app = test_app();
        assert_eq!(app.view, View::Active);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.view, View::History);

        press(&mut app, KeyCode::Tab);
        assert_eq!(app.view, View::Skills);

        // In Skills view, Tab goes back to Active
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.view, View::Active);
    }

    #[test]
    fn focus_switching_with_numbers() {
        let mut app = test_app();
        assert_eq!(app.focus, Focus::Projects);

        press(&mut app, KeyCode::Char('2'));
        assert_eq!(app.focus, Focus::Sessions);

        press(&mut app, KeyCode::Char('3'));
        assert_eq!(app.focus, Focus::Tasks);

        press(&mut app, KeyCode::Char('1'));
        assert_eq!(app.focus, Focus::Projects);
    }

    #[test]
    fn navigate_projects_jk() {
        let store = Store::open_in_memory().unwrap();
        store.create_project("alpha", "/tmp/alpha").unwrap();
        store.create_project("beta", "/tmp/beta").unwrap();
        store.create_project("gamma", "/tmp/gamma").unwrap();
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
        press(&mut app, KeyCode::Char('3'));
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
        store.create_project("a", "/tmp/a").unwrap();
        store.create_project("b", "/tmp/b").unwrap();
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
        let p1 = store.create_project("alpha", "/tmp/alpha").unwrap();
        let p2 = store.create_project("beta", "/tmp/beta").unwrap();
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
        assert_eq!(app.new_task_mode, TaskMode::Supervised);
        // Toggle mode
        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);
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
        assert_eq!(tasks[0].mode, TaskMode::Autonomous);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn create_task_cancel() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        type_str(&mut app, "Will cancel");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.tasks.is_empty());
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

        press(&mut app, KeyCode::Char('3')); // Focus tasks
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
    fn edit_task_cancel() {
        let mut app = test_app_with_tasks();
        let original_desc = app.tasks[0].description.clone();
        let task_id = app.tasks[0].id.clone();

        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('e'));
        for _ in 0..20 {
            press(&mut app, KeyCode::Backspace);
        }
        type_str(&mut app, "Changed");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.description, original_desc);
    }

    #[test]
    fn edit_task_only_works_on_pending() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::InProgress)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn delete_task_flow() {
        let mut app = test_app_with_tasks();
        assert_eq!(app.visible_tasks().len(), 3);

        press(&mut app, KeyCode::Char('3'));
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
            .update_task_status(&task_id, TaskStatus::InProgress)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('d'));
        // Any task status should allow deletion
        assert_eq!(app.input_mode, InputMode::ConfirmDelete);
    }

    #[test]
    fn reorder_tasks_shift_j() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('3'));

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
        press(&mut app, KeyCode::Char('3'));
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
    fn visible_tasks_excludes_done_in_active_view() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::Done)
            .unwrap();
        app.refresh_data().unwrap();

        assert_eq!(app.view, View::Active);
        assert_eq!(app.visible_tasks().len(), 2);
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

        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('r'));

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(app.toast_message.as_deref(), Some("Task marked as done"));
    }

    #[test]
    fn review_in_progress_task_marks_done() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        app.store
            .update_task_status(&task_id, TaskStatus::InProgress)
            .unwrap();
        app.refresh_data().unwrap();

        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('r'));

        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Done);
        assert_eq!(app.toast_message.as_deref(), Some("Task marked as done"));
    }

    #[test]
    fn review_only_works_on_in_review_tasks() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();

        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('r'));

        // Pending task: r should do nothing
        let task = app.store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Pending);
    }

    // ═══════════════════════════════════════════════════════════════
    // 5. SESSION INPUT MODE
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn new_session_opens_form() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.input_mode, InputMode::NewSession);
        assert!(app.input_buffer.is_empty());
    }

    #[test]
    fn new_session_requires_project() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn new_session_cancel() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('s'));
        type_str(&mut app, "feat/something");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.input_buffer.is_empty());
    }

    #[test]
    fn new_session_typing_and_backspace() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('s'));
        type_str(&mut app, "feat/my-branch");
        assert_eq!(app.input_buffer, "feat/my-branch");

        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.input_buffer, "feat/my-branc");
    }

    // ═══════════════════════════════════════════════════════════════
    // 6. COMMAND PALETTE
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
    fn command_palette_execute_toggle_view() {
        let mut app = test_app();
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        type_str(&mut app, "toggle");
        press(&mut app, KeyCode::Enter);
        assert_ne!(app.view, View::Active);
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
    // 8. SKILLS VIEW
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn skills_view_toggle_scope() {
        let mut app = test_app();
        app.view = View::Skills;
        assert!(app.skill_scope_global);

        press(&mut app, KeyCode::Char('g'));
        assert!(!app.skill_scope_global);

        press(&mut app, KeyCode::Char('g'));
        assert!(app.skill_scope_global);
    }

    #[test]
    fn skills_view_search_mode() {
        let mut app = test_app();
        app.view = View::Skills;

        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.input_mode, InputMode::SkillSearch);
        assert!(app.input_buffer.is_empty());

        type_str(&mut app, "test-skill");
        assert_eq!(app.input_buffer, "test-skill");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
        assert!(app.input_buffer.is_empty());
    }

    #[test]
    fn skills_view_add_mode() {
        let mut app = test_app();
        app.view = View::Skills;

        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.input_mode, InputMode::SkillAdd);

        type_str(&mut app, "owner/repo@skill");
        assert_eq!(app.input_buffer, "owner/repo@skill");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.input_mode, InputMode::Normal);
    }

    #[test]
    fn skills_view_quit() {
        let mut app = test_app();
        app.view = View::Skills;
        press(&mut app, KeyCode::Char('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn skills_view_help() {
        let mut app = test_app();
        app.view = View::Skills;
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.input_mode, InputMode::HelpOverlay);
    }

    #[test]
    fn skills_view_tab_returns_to_active() {
        let mut app = test_app();
        app.view = View::Skills;
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.view, View::Active);
    }

    #[test]
    fn skills_view_ctrl_p_opens_palette() {
        let mut app = test_app();
        app.view = View::Skills;
        press_mod(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(app.input_mode, InputMode::CommandPalette);
    }

    // ═══════════════════════════════════════════════════════════════
    // 9. TASK FORM DETAILS
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn task_form_backtab_cycles() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.new_task_field, 0);

        // BackTab wraps to field 1 (mode)
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
        assert_eq!(app.new_task_field, 0);
    }

    #[test]
    fn task_form_mode_toggle() {
        let mut app = test_app_with_project();
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);

        press(&mut app, KeyCode::Right);
        assert_eq!(app.new_task_mode, TaskMode::Autonomous);

        press(&mut app, KeyCode::Left);
        assert_eq!(app.new_task_mode, TaskMode::Supervised);
    }

    #[test]
    fn edit_task_form_cycling() {
        let mut app = test_app_with_tasks();
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.input_mode, InputMode::EditTask);

        // Tab cycles between prompt (0) and mode (1)
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.new_task_field, 1);
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

        press(&mut app, KeyCode::Char('3'));
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
        press(&mut app, KeyCode::Char('3'));
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
        press(&mut app, KeyCode::Char('3'));
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
        let app = test_app();
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("claustre"));
        assert!(output.contains("Projects"));
        assert!(output.contains("No projects yet"));
    }

    #[test]
    fn snapshot_active_view_with_data() {
        let app = test_app_with_tasks();
        let output = render_to_string(&app, 100, 30);
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
        let app = test_app_with_project();
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("Session Detail"));
        assert!(output.contains("No active sessions"));
    }

    #[test]
    fn snapshot_history_view() {
        let mut app = test_app_with_tasks();
        app.view = View::History;
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("history"));
        assert!(output.contains("Project Stats"));
        assert!(output.contains("Completed Tasks"));
        assert!(output.contains("test-project"));
    }

    #[test]
    fn snapshot_skills_view() {
        let mut app = test_app();
        app.view = View::Skills;
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("skills"));
        assert!(output.contains("Installed Skills"));
        assert!(output.contains("Skill Detail"));
    }

    #[test]
    fn snapshot_help_overlay() {
        let mut app = test_app();
        app.input_mode = InputMode::HelpOverlay;
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("Help"));
        assert!(output.contains("Tab"));
        assert!(output.contains("Ctrl+P"));
        assert!(output.contains("Quit"));
    }

    #[test]
    fn snapshot_command_palette() {
        let mut app = test_app();
        app.input_mode = InputMode::CommandPalette;
        app.filter_palette();
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("Command Palette"));
        assert!(output.contains("New Task"));
        assert!(output.contains("Quit"));
    }

    #[test]
    fn snapshot_task_form() {
        let mut app = test_app_with_project();
        app.input_mode = InputMode::NewTask;
        let output = render_to_string(&app, 100, 30);
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
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("Edit Task"));
        assert!(output.contains("Prompt"));
    }

    #[test]
    fn snapshot_new_session_panel() {
        let mut app = test_app_with_project();
        app.input_mode = InputMode::NewSession;
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("New Session"));
        assert!(output.contains("Branch"));
    }

    #[test]
    fn snapshot_new_project_panel() {
        let mut app = test_app();
        app.input_mode = InputMode::NewProject;
        let output = render_to_string(&app, 100, 30);
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
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("Delete"));
        assert!(output.contains("test-project"));
    }

    #[test]
    fn snapshot_task_filter_active() {
        let mut app = test_app_with_tasks();
        app.input_mode = InputMode::TaskFilter;
        app.task_filter = "alpha".to_string();
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("/alpha"));
    }

    #[test]
    fn snapshot_usage_bars() {
        let mut app = test_app_with_project();
        app.rate_limit_state.usage_5h_pct = Some(42.0);
        app.rate_limit_state.usage_7d_pct = Some(15.0);
        let output = render_to_string(&app, 100, 30);
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
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("RATE LIMITED"));
    }

    #[test]
    fn snapshot_toast_visible() {
        let mut app = test_app_with_project();
        app.show_toast("Test notification", ToastStyle::Success);
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("Test notification"));
    }

    #[test]
    fn snapshot_task_status_indicators() {
        let mut app = test_app_with_tasks();
        // Set varied task statuses
        let t0 = app.tasks[0].id.clone();
        let t1 = app.tasks[1].id.clone();
        app.store
            .update_task_status(&t0, TaskStatus::InProgress)
            .unwrap();
        app.store
            .update_task_status(&t1, TaskStatus::InReview)
            .unwrap();
        app.refresh_data().unwrap();
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("in_progress"));
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
        press(&mut app, KeyCode::Char('3'));
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(app.input_mode, InputMode::SubtaskPanel);
    }

    #[test]
    fn subtask_panel_add_and_close() {
        let mut app = test_app_with_tasks();
        let task_id = app.tasks[0].id.clone();
        press(&mut app, KeyCode::Char('3'));
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

        press(&mut app, KeyCode::Char('3'));
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

        press(&mut app, KeyCode::Char('3'));
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
        // Should open new session, not subtask panel
        assert_eq!(app.input_mode, InputMode::NewSession);
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
        let output = render_to_string(&app, 100, 30);
        assert!(output.contains("Subtasks"));
        assert!(output.contains("step 1"));
    }
}
