use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;

use crate::store::{Store, TaskStatus};

use super::{
    App, DeleteTarget, Focus, InputMode, PaletteAction, PaletteItem, Tab, ToastStyle,
    build_project_summaries,
};

impl App {
    pub fn new(store: Store) -> Result<Self> {
        let projects = store.list_projects()?;

        // Detect stale working sessions (no PTY tab on startup = interrupted).
        // These will be restored by reconnect_running_sessions() later.
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
                label: "Configure Claude Permissions".into(),
                action: PaletteAction::Configure,
            },
            PaletteItem {
                label: "Sprint Board".into(),
                action: PaletteAction::SprintBoard,
            },
            PaletteItem {
                label: "Quit".into(),
                action: PaletteAction::Quit,
            },
        ];
        let palette_filtered: Vec<usize> = (0..palette_items.len()).collect();

        let project_summaries = build_project_summaries(&store, &projects);
        let rate_limit_state = store.get_rate_limit_state().unwrap_or_default();
        let external_sessions = store.list_external_sessions().unwrap_or_default();

        // Find pending autonomous tasks without a session to auto-launch on startup
        let startup_auto_launch: VecDeque<(String, crate::store::Task)> = store
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
        let (sc_tx, sc_rx) = mpsc::channel();
        let (so_tx, so_rx) = mpsc::channel();
        let (up_tx, up_rx) = mpsc::channel();

        let config = crate::config::load().unwrap_or_default();
        let theme = config.theme.build();
        let board_columns: Vec<String> = config
            .board
            .columns
            .iter()
            .map(|c| c.name.clone())
            .collect();
        let config_warning = crate::configure::check_config_status();

        let mut app = App {
            store,
            config,
            theme,
            keymap: super::super::keymap::KeyMap::default_keymap(),
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
            new_task_base: String::new(),
            new_task_branch: String::new(),
            new_task_push_mode: crate::store::PushMode::Pr,
            new_task_review_loop: false,
            new_project_field: 0,
            new_project_name: String::new(),
            new_project_path: String::new(),
            new_project_git_linked: true,
            board_issues: vec![],
            board_columns,
            board_column_index: 0,
            board_issue_index: 0,
            board_milestone_filter: None,
            board_milestones: vec![],
            board_milestone_index: 0,
            board_loading: false,
            board_error: None,
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
            task_details_scroll: 0,
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
            selected_search_indices: HashSet::new(),
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
            scanner_in_progress: Arc::new(AtomicBool::new(false)),
            scanner_tx: sc_tx,
            scanner_rx: sc_rx,
            last_scan: Instant::now()
                .checked_sub(Duration::from_secs(120))
                .unwrap(),
            external_sessions,
            session_op_tx: so_tx,
            session_op_rx: so_rx,
            session_op_in_progress: false,
            pending_relaunch: None,
            toast_message: None,
            toast_style: ToastStyle::Info,
            toast_expires: None,
            prev_task_statuses,
            notified_in_review: HashSet::new(),
            review_loop_spawned: HashSet::new(),
            last_slow_tick: Instant::now(),
            last_terminal_area: Rect::default(),
            paused_sessions: HashSet::new(),
            waiting_sessions: HashSet::new(),
            cached_visible_indices: Vec::new(),
            update_check_in_progress: Arc::new(AtomicBool::new(false)),
            config_warning,
            cached_config_status: None,
            update_tx: up_tx,
            update_rx: up_rx,
            last_update_check: Instant::now(),
            updated_version: None,
            available_version: None,
        };

        app.recompute_visible_tasks();

        // Reconnect to any session-host processes that survived a TUI restart
        app.reconnect_running_sessions();
        // Always start on the dashboard, even if sessions were restored above
        app.active_tab = 0;

        // Check for updates in the background on startup (skip in tests)
        #[cfg(not(test))]
        app.spawn_update_check();

        Ok(app)
    }
}
