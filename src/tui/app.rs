use std::time::Duration;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::DefaultTerminal;

use std::collections::HashMap;

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

        Ok(())
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
                        if let Some(session) = self.selected_session() {
                            let _ = crate::session::goto_session(session);
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
                }
            }

            // Delete/teardown session
            (KeyCode::Char('d'), _) => {
                if self.focus == Focus::Sessions
                    && let Some(session_id) = self.selected_session().map(|s| s.id.clone())
                {
                    crate::session::teardown_session(&self.store, &session_id)?;
                    self.refresh_data()?;
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
                self.new_project_path.clear();
                self.new_project_field = 0;
            }

            _ => {}
        }
        Ok(())
    }

    fn handle_input_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Enter => match self.new_task_field {
                0 => {
                    if !self.input_buffer.is_empty() {
                        self.new_task_title = std::mem::take(&mut self.input_buffer);
                        self.new_task_field = 1;
                    }
                }
                1 => {
                    self.new_task_description = std::mem::take(&mut self.input_buffer);
                    self.new_task_field = 2;
                }
                _ => {
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
            },
            KeyCode::Tab | KeyCode::BackTab if self.new_task_field == 2 => {
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
        match code {
            KeyCode::Enter => {
                if self.new_project_field == 0 && !self.input_buffer.is_empty() {
                    self.new_project_name = std::mem::take(&mut self.input_buffer);
                    self.new_project_field = 1;
                    self.input_buffer = String::from(".");
                } else if self.new_project_field == 1 && !self.input_buffer.is_empty() {
                    self.new_project_path = std::mem::take(&mut self.input_buffer);
                    if let Ok(abs_path) = std::fs::canonicalize(&self.new_project_path)
                        && let Some(abs_str) = abs_path.to_str()
                    {
                        self.store.create_project(&self.new_project_name, abs_str)?;
                    }
                    self.new_project_name.clear();
                    self.new_project_path.clear();
                    self.new_project_field = 0;
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if self.new_project_field == 0 {
                    self.new_project_name = std::mem::take(&mut self.input_buffer);
                    self.input_buffer = std::mem::take(&mut self.new_project_path);
                    self.new_project_field = 1;
                } else {
                    self.new_project_path = std::mem::take(&mut self.input_buffer);
                    self.input_buffer = std::mem::take(&mut self.new_project_name);
                    self.new_project_field = 0;
                }
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.new_project_name.clear();
                self.new_project_path.clear();
                self.new_project_field = 0;
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
                    self.store.delete_project(&self.confirm_project_id)?;
                    self.confirm_project_id.clear();
                    self.confirm_target.clear();
                    self.input_mode = InputMode::Normal;
                    self.project_index = 0;
                    self.refresh_data()?;
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
                self.new_project_path.clear();
                self.new_project_field = 0;
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
