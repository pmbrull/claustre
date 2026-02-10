use std::time::Duration;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::DefaultTerminal;

use std::collections::HashMap;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    NewSession,
    NewProject,
    ConfirmDelete,
    CommandPalette,
    SkillSearch,
    SkillAdd,
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

    // Enhanced task form state
    pub new_task_field: u8,
    pub new_task_title: String,
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
    pub confirm_project_id: String,

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
            new_task_title: String::new(),
            new_task_description: String::new(),
            new_task_mode: crate::store::TaskMode::Supervised,
            new_project_field: 0,
            new_project_name: String::new(),
            new_project_path: String::new(),
            path_suggestions: vec![],
            path_suggestion_index: 0,
            show_path_suggestions: false,
            confirm_target: String::new(),
            confirm_project_id: String::new(),
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
    /// If the cache is stale or missing, spawn a background thread to fetch from the API.
    fn refresh_usage_from_api_cache(&mut self) {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let cache_path = home.join(".claude/statusline-cache.json");

        let cache_fresh = if let Ok(content) = std::fs::read_to_string(&cache_path) {
            if let Ok(cache) = serde_json::from_str::<serde_json::Value>(&content) {
                let timestamp = cache["timestamp"].as_f64().unwrap_or(0.0);
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "millisecond epoch fits in f64 for decades"
                )]
                let age_ms = (chrono::Utc::now().timestamp_millis() as f64) - timestamp;

                if age_ms < 120_000.0 {
                    if let Some(pct) = cache["data"]["pct5h"].as_f64() {
                        self.rate_limit_state.usage_5h_pct = pct;
                    }
                    if let Some(pct) = cache["data"]["pct7d"].as_f64() {
                        self.rate_limit_state.usage_7d_pct = pct;
                    }
                    self.rate_limit_state.reset_5h =
                        cache["data"]["reset5h"].as_str().map(String::from);
                    self.rate_limit_state.reset_7d =
                        cache["data"]["reset7d"].as_str().map(String::from);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };

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
    pub fn visible_tasks(&self) -> Vec<&Task> {
        match self.view {
            View::Active => self
                .tasks
                .iter()
                .filter(|t| t.status != crate::store::TaskStatus::Done)
                .collect(),
            View::History | View::Skills => self.tasks.iter().collect(),
        }
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
                    InputMode::NewSession => self.handle_session_input_key(key.code)?,
                    InputMode::NewProject => self.handle_new_project_key(key.code)?,
                    InputMode::ConfirmDelete => self.handle_confirm_delete_key(key.code)?,
                    InputMode::CommandPalette => self.handle_palette_key(key.code)?,
                    InputMode::SkillSearch => self.handle_skill_search_key(key.code)?,
                    InputMode::SkillAdd => self.handle_skill_add_key(key.code)?,
                },
                AppEvent::Tick => {
                    self.tick_toast();
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

            // Navigation
            (KeyCode::Char('j') | KeyCode::Down, _) => self.move_down(),
            (KeyCode::Char('k') | KeyCode::Up, _) => self.move_up(),

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

            // New session
            (KeyCode::Char('s'), _) => {
                if self.selected_project().is_some() {
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

            // Review task (mark in_review → done)
            (KeyCode::Char('r'), _) => {
                if self.focus == Focus::Tasks
                    && let Some(task) = self.visible_tasks().get(self.task_index).copied()
                    && task.status == crate::store::TaskStatus::InReview
                {
                    self.store
                        .update_task_status(&task.id, crate::store::TaskStatus::Done)?;
                    self.refresh_data()?;
                    self.show_toast("Task marked as done", ToastStyle::Success);
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

            // Delete/teardown session
            (KeyCode::Char('d'), _) => {
                if self.focus == Focus::Sessions
                    && let Some(session_id) = self.selected_session().map(|s| s.id.clone())
                {
                    match crate::session::teardown_session(&self.store, &session_id) {
                        Ok(()) => {
                            self.refresh_data()?;
                            self.show_toast("Session torn down", ToastStyle::Success);
                        }
                        Err(e) => {
                            self.show_toast(format!("Teardown failed: {e}"), ToastStyle::Error);
                        }
                    }
                }
            }

            // Remove project (with confirmation)
            (KeyCode::Char('x'), _) => {
                if self.focus == Focus::Projects
                    && let Some((name, id)) = self
                        .selected_project()
                        .map(|p| (p.name.clone(), p.id.clone()))
                {
                    self.confirm_target = name;
                    self.confirm_project_id = id;
                    self.input_mode = InputMode::ConfirmDelete;
                }
            }

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

    fn handle_input_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Enter => {
                self.save_current_task_field();
                if !self.new_task_title.is_empty() {
                    if let Some(project_id) = self.selected_project().map(|p| p.id.clone()) {
                        self.store.create_task(
                            &project_id,
                            &self.new_task_title,
                            &self.new_task_description,
                            self.new_task_mode,
                        )?;
                    }
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Tab => {
                self.save_current_task_field();
                self.new_task_field = (self.new_task_field + 1) % 3;
                self.load_current_task_field();
            }
            KeyCode::BackTab => {
                self.save_current_task_field();
                self.new_task_field = if self.new_task_field == 0 {
                    2
                } else {
                    self.new_task_field - 1
                };
                self.load_current_task_field();
            }
            KeyCode::Left | KeyCode::Right if self.new_task_field == 2 => {
                self.new_task_mode = match self.new_task_mode {
                    crate::store::TaskMode::Supervised => crate::store::TaskMode::Autonomous,
                    crate::store::TaskMode::Autonomous => crate::store::TaskMode::Supervised,
                };
            }
            KeyCode::Esc => {
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char(c) if self.new_task_field < 2 => {
                self.input_buffer.push(c);
            }
            KeyCode::Backspace if self.new_task_field < 2 => {
                self.input_buffer.pop();
            }
            _ => {}
        }
        Ok(())
    }

    fn save_current_task_field(&mut self) {
        match self.new_task_field {
            0 => self.new_task_title.clone_from(&self.input_buffer),
            1 => self.new_task_description.clone_from(&self.input_buffer),
            _ => {}
        }
    }

    fn load_current_task_field(&mut self) {
        match self.new_task_field {
            0 => self.input_buffer.clone_from(&self.new_task_title),
            1 => self.input_buffer.clone_from(&self.new_task_description),
            _ => self.input_buffer.clear(),
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
                KeyCode::Enter if self.show_path_suggestions => {
                    self.accept_path_suggestion();
                }
                KeyCode::Enter => {
                    self.save_current_project_field();
                    self.submit_new_project()?;
                }
                KeyCode::Tab if self.show_path_suggestions => {
                    self.complete_path_common_prefix();
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

    fn complete_path_common_prefix(&mut self) {
        if self.path_suggestions.is_empty() {
            return;
        }

        if self.path_suggestions.len() == 1 {
            self.accept_path_suggestion();
            return;
        }

        // Find longest common prefix among all suggestions
        let first = &self.path_suggestions[0];
        let mut prefix_len = first.len();
        for s in &self.path_suggestions[1..] {
            prefix_len = prefix_len.min(
                first
                    .chars()
                    .zip(s.chars())
                    .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
                    .count(),
            );
        }

        if prefix_len == 0 {
            return;
        }

        let common = &self.path_suggestions[0][..prefix_len];

        // Find what partial we currently have
        let Some(expanded) = Self::expand_tilde(&self.input_buffer) else {
            return;
        };

        let partial = if expanded.ends_with('/') {
            ""
        } else if let Some(pos) = expanded.rfind('/') {
            &expanded[pos + 1..]
        } else {
            return;
        };

        // Only extend if common prefix is longer than what we have
        if common.len() > partial.len() {
            // Replace partial with common prefix
            let base_end = if self.input_buffer.ends_with('/') {
                self.input_buffer.len()
            } else if let Some(pos) = self.input_buffer.rfind('/') {
                pos + 1
            } else {
                return;
            };
            let base = self.input_buffer[..base_end].to_string();
            self.input_buffer = format!("{base}{common}");
            self.update_path_suggestions();
        }
    }

    fn clear_path_autocomplete(&mut self) {
        self.path_suggestions.clear();
        self.path_suggestion_index = 0;
        self.show_path_suggestions = false;
    }

    fn reset_task_form(&mut self) {
        self.input_buffer.clear();
        self.new_task_title.clear();
        self.new_task_description.clear();
        self.new_task_mode = crate::store::TaskMode::Supervised;
        self.new_task_field = 0;
    }

    fn handle_confirm_delete_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Char('y') => {
                if !self.confirm_project_id.is_empty() {
                    let name = self.confirm_target.clone();
                    self.store.delete_project(&self.confirm_project_id)?;
                    self.confirm_project_id.clear();
                    self.confirm_target.clear();
                    self.input_mode = InputMode::Normal;
                    self.project_index = 0;
                    self.refresh_data()?;
                    self.show_toast(format!("Project '{name}' deleted"), ToastStyle::Success);
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.confirm_project_id.clear();
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
                    self.confirm_project_id = id;
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
        .ok()?;
    let token_json = String::from_utf8(output.stdout).ok()?;
    let creds: serde_json::Value = serde_json::from_str(token_json.trim()).ok()?;
    let access_token = creds["claudeAiOauth"]["accessToken"].as_str()?;

    // Fetch usage from API
    let output = std::process::Command::new("curl")
        .args([
            "-s",
            "https://api.anthropic.com/api/oauth/usage",
            "-H",
            &format!("Authorization: Bearer {access_token}"),
            "-H",
            "anthropic-beta: oauth-2025-04-20",
            "-H",
            "Content-Type: application/json",
        ])
        .output()
        .ok()?;
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

fn build_project_summaries(store: &Store, projects: &[Project]) -> HashMap<String, ProjectSummary> {
    let mut summaries = HashMap::with_capacity(projects.len());
    for project in projects {
        let active_sessions = store
            .list_active_sessions_for_project(&project.id)
            .unwrap_or_default();
        let has_review = store
            .list_tasks_for_project(&project.id)
            .unwrap_or_default()
            .iter()
            .any(|t| t.status == crate::store::TaskStatus::InReview);
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
