use std::time::Duration;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::DefaultTerminal;

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
    CommandPalette,
    SkillSearch,
    SkillAdd,
}

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub label: String,
    pub action: PaletteAction,
}

#[derive(Debug, Clone)]
pub enum PaletteAction {
    NewTask,
    NewSession,
    ToggleView,
    FocusProjects,
    FocusSessions,
    FocusTasks,
    FindSkills,
    UpdateSkills,
    Quit,
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

    // Selection indices
    pub project_index: usize,
    pub session_index: usize,
    pub task_index: usize,

    // Input buffer for new task creation
    pub input_buffer: String,

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
            PaletteItem { label: "New Task".into(), action: PaletteAction::NewTask },
            PaletteItem { label: "New Session".into(), action: PaletteAction::NewSession },
            PaletteItem { label: "Toggle View (Active/History)".into(), action: PaletteAction::ToggleView },
            PaletteItem { label: "Focus Projects".into(), action: PaletteAction::FocusProjects },
            PaletteItem { label: "Focus Sessions".into(), action: PaletteAction::FocusSessions },
            PaletteItem { label: "Focus Tasks".into(), action: PaletteAction::FocusTasks },
            PaletteItem { label: "Find Skills".into(), action: PaletteAction::FindSkills },
            PaletteItem { label: "Update Skills".into(), action: PaletteAction::UpdateSkills },
            PaletteItem { label: "Quit".into(), action: PaletteAction::Quit },
        ];
        let palette_filtered: Vec<usize> = (0..palette_items.len()).collect();

        Ok(App {
            store,
            should_quit: false,
            view: View::Active,
            focus: Focus::Projects,
            input_mode: InputMode::Normal,
            projects,
            sessions,
            tasks,
            project_index: 0,
            session_index: 0,
            task_index: 0,
            input_buffer: String::new(),
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
            self.sessions = self
                .store
                .list_active_sessions_for_project(&project.id)?;
            self.tasks = self.store.list_tasks_for_project(&project.id)?;
        } else {
            self.sessions.clear();
            self.tasks.clear();
        }

        // Clamp indices
        if self.project_index >= self.projects.len() && !self.projects.is_empty() {
            self.project_index = self.projects.len() - 1;
        }
        if self.session_index >= self.sessions.len() && !self.sessions.is_empty() {
            self.session_index = self.sessions.len() - 1;
        }
        if self.task_index >= self.tasks.len() && !self.tasks.is_empty() {
            self.task_index = self.tasks.len() - 1;
        }

        Ok(())
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_index)
    }

    pub fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.session_index)
    }

    pub async fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        let tick_rate = Duration::from_millis(250);

        loop {
            terminal.draw(|frame| ui::draw(frame, self))?;

            match event::poll(tick_rate)? {
                AppEvent::Key(key) => {
                    match self.input_mode {
                        InputMode::Normal => {
                            if self.view == View::Skills {
                                self.handle_skills_key(key.code, key.modifiers)?;
                            } else {
                                self.handle_normal_key(key.code, key.modifiers)?;
                            }
                        }
                        InputMode::NewTask => self.handle_input_key(key.code)?,
                        InputMode::NewSession => self.handle_session_input_key(key.code)?,
                        InputMode::CommandPalette => self.handle_palette_key(key.code)?,
                        InputMode::SkillSearch => self.handle_skill_search_key(key.code)?,
                        InputMode::SkillAdd => self.handle_skill_add_key(key.code)?,
                    }
                }
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
            (KeyCode::Char('q'), _) => {
                self.should_quit = true;
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
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
            (KeyCode::Char('h'), _) | (KeyCode::Tab, _) => {
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
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => self.move_down(),
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => self.move_up(),

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
                            let session = session.clone();
                            let _ = crate::session::goto_session(&session);
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
                    self.input_mode = InputMode::NewTask;
                    self.input_buffer.clear();
                }
            }

            // Review task (mark in_review → done)
            (KeyCode::Char('r'), _) => {
                if self.focus == Focus::Tasks {
                    if let Some(task) = self.tasks.get(self.task_index) {
                        if task.status == crate::store::TaskStatus::InReview {
                            self.store
                                .update_task_status(&task.id, crate::store::TaskStatus::Done)?;
                            self.refresh_data()?;
                        }
                    }
                }
            }

            // Delete/teardown session
            (KeyCode::Char('d'), _) => {
                if self.focus == Focus::Sessions {
                    if let Some(session) = self.selected_session() {
                        let session_id = session.id.clone();
                        crate::session::teardown_session(&self.store, &session_id)?;
                        self.refresh_data()?;
                    }
                }
            }

            _ => {}
        }
        Ok(())
    }

    fn handle_input_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty() {
                    if let Some(project) = self.selected_project() {
                        let project_id = project.id.clone();
                        self.store.create_task(
                            &project_id,
                            &self.input_buffer,
                            "",
                            crate::store::TaskMode::Supervised,
                        )?;
                        self.input_buffer.clear();
                        self.input_mode = InputMode::Normal;
                        self.refresh_data()?;
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

    fn handle_session_input_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty() {
                    if let Some(project) = self.selected_project() {
                        let project_id = project.id.clone();
                        let branch_name = self.input_buffer.clone();
                        self.input_buffer.clear();
                        self.input_mode = InputMode::Normal;

                        crate::session::create_session(
                            &self.store,
                            &project_id,
                            &branch_name,
                            None,
                        )?;
                        self.refresh_data()?;
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
                if !self.tasks.is_empty() {
                    self.task_index = (self.task_index + 1).min(self.tasks.len() - 1);
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
                    let action = self.palette_items[idx].action.clone();
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
                    self.input_mode = InputMode::NewTask;
                    self.input_buffer.clear();
                }
            }
            PaletteAction::NewSession => {
                if self.selected_project().is_some() {
                    self.input_mode = InputMode::NewSession;
                    self.input_buffer.clear();
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
                        self.skill_status_message = format!("Update failed: {}", e);
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
            let project_skills = crate::skills::list_skills(false, Some(&project.repo_path))
                .unwrap_or_default();
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
            (KeyCode::Char('q'), _) => self.should_quit = true,
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => self.should_quit = true,

            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                self.input_mode = InputMode::CommandPalette;
                self.input_buffer.clear();
                self.palette_index = 0;
                self.filter_palette();
            }

            (KeyCode::Tab, _) => {
                self.view = View::Active;
            }

            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                if !self.installed_skills.is_empty() {
                    self.skill_index =
                        (self.skill_index + 1).min(self.installed_skills.len() - 1);
                    self.refresh_skill_detail();
                }
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
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
                    let project_path = if let crate::skills::SkillScope::Project(ref p) = skill.scope {
                        Some(p.clone())
                    } else {
                        None
                    };

                    match crate::skills::remove_skill(&name, global, project_path.as_deref()) {
                        Ok(_) => {
                            self.skill_status_message = format!("Removed {}", name);
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.skill_status_message = format!("Remove failed: {}", e);
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
                        self.skill_status_message = format!("Update failed: {}", e);
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
                    if !self.search_results.is_empty() {
                        if let Some(result) = self.search_results.get(self.skill_index) {
                            let package = result.package.clone();
                            let global = self.skill_scope_global;
                            let project_path = if !global {
                                self.selected_project().map(|p| p.repo_path.clone())
                            } else {
                                None
                            };

                            self.skill_status_message = format!("Installing {}...", package);
                            match crate::skills::add_skill(&package, global, project_path.as_deref()) {
                                Ok(_) => {
                                    self.skill_status_message = format!("Installed {}", package);
                                    self.input_mode = InputMode::Normal;
                                    self.input_buffer.clear();
                                    self.search_results.clear();
                                    self.refresh_skills();
                                }
                                Err(e) => {
                                    self.skill_status_message = format!("Install failed: {}", e);
                                }
                            }
                        }
                    } else {
                        let query = self.input_buffer.clone();
                        self.skill_status_message = format!("Searching for '{}'...", query);
                        match crate::skills::find_skills(&query) {
                            Ok(results) => {
                                self.skill_status_message = format!("Found {} results", results.len());
                                self.search_results = results;
                                self.skill_index = 0;
                            }
                            Err(e) => {
                                self.skill_status_message = format!("Search failed: {}", e);
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
                    let project_path = if !global {
                        self.selected_project().map(|p| p.repo_path.clone())
                    } else {
                        None
                    };

                    self.skill_status_message = format!("Installing {}...", package);
                    match crate::skills::add_skill(&package, global, project_path.as_deref()) {
                        Ok(_) => {
                            self.skill_status_message = format!("Installed {}", package);
                            self.input_mode = InputMode::Normal;
                            self.input_buffer.clear();
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.skill_status_message = format!("Install failed: {}", e);
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
