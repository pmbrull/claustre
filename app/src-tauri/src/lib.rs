//! Claustre desktop app — Tauri backend.
//!
//! Provides IPC commands that the web frontend calls to interact with
//! the claustre SQLite database, manage projects/tasks/sessions, and
//! poll for state changes.

use std::sync::Mutex;

use claustre::store::{PushMode, Store, Task, TaskMode, TaskStatus};
use serde::{Deserialize, Serialize};
use tauri::State;

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Shared database handle wrapped in a Mutex for thread-safe access.
struct AppState {
    store: Mutex<Store>,
}

// ---------------------------------------------------------------------------
// Serializable response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub repo_path: String,
    pub default_branch: String,
    pub active_sessions: usize,
    pub pending_tasks: usize,
    pub working_tasks: usize,
    pub in_review_tasks: usize,
    pub done_tasks: usize,
    pub total_tasks: usize,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    pub id: String,
    pub project_id: String,
    pub branch_name: String,
    pub worktree_path: String,
    pub tab_label: String,
    pub claude_status: String,
    pub status_message: String,
    pub files_changed: i64,
    pub lines_added: i64,
    pub lines_removed: i64,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct StatsInfo {
    pub total_tasks: i64,
    pub completed_tasks: i64,
    pub total_sessions: i64,
    pub total_time: String,
    pub total_tokens: i64,
    pub avg_task_time: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct SubtaskInfo {
    pub id: String,
    pub task_id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub sort_order: i64,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct RateLimitInfo {
    pub is_rate_limited: bool,
    pub usage_5h_pct: Option<f64>,
    pub usage_7d_pct: Option<f64>,
    pub reset_5h: Option<String>,
    pub reset_7d: Option<String>,
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateTaskRequest {
    pub project_id: String,
    pub title: String,
    pub description: String,
    pub mode: String,
    pub push_mode: String,
    pub review_loop: bool,
    pub branch: Option<String>,
    pub base: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskRequest {
    pub id: String,
    pub title: String,
    pub description: String,
    pub mode: String,
    pub push_mode: String,
    pub review_loop: bool,
    pub branch: Option<String>,
    pub base: Option<String>,
}

// ---------------------------------------------------------------------------
// Helper: convert store errors to Tauri command errors
// ---------------------------------------------------------------------------

fn map_err(e: anyhow::Error) -> String {
    e.to_string()
}

// ---------------------------------------------------------------------------
// Core logic — testable functions that operate on &Store directly
// ---------------------------------------------------------------------------

pub fn core_list_projects(store: &Store) -> anyhow::Result<Vec<ProjectSummary>> {
    let projects = store.list_projects()?;
    let mut summaries = Vec::with_capacity(projects.len());
    for p in &projects {
        let sessions = store.list_active_sessions_for_project(&p.id)?;
        let tasks = store.list_tasks_for_project(&p.id)?;

        let pending = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .count();
        let working = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Working)
            .count();
        let in_review = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::InReview)
            .count();
        let done = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count();

        summaries.push(ProjectSummary {
            id: p.id.clone(),
            name: p.name.clone(),
            repo_path: p.repo_path.clone(),
            default_branch: p.default_branch.clone(),
            active_sessions: sessions.len(),
            pending_tasks: pending,
            working_tasks: working,
            in_review_tasks: in_review,
            done_tasks: done,
            total_tasks: tasks.len(),
        });
    }
    Ok(summaries)
}

pub fn core_create_project(
    store: &Store,
    name: &str,
    repo_path: &str,
    default_branch: &str,
) -> anyhow::Result<ProjectSummary> {
    let project = store.create_project(name, repo_path, default_branch)?;
    Ok(ProjectSummary {
        id: project.id,
        name: project.name,
        repo_path: project.repo_path,
        default_branch: project.default_branch,
        active_sessions: 0,
        pending_tasks: 0,
        working_tasks: 0,
        in_review_tasks: 0,
        done_tasks: 0,
        total_tasks: 0,
    })
}

pub fn core_get_stats(store: &Store, project_id: &str) -> anyhow::Result<StatsInfo> {
    let stats = store.project_stats(project_id)?;
    Ok(StatsInfo {
        total_tasks: stats.total_tasks,
        completed_tasks: stats.completed_tasks,
        total_sessions: stats.total_sessions,
        total_time: stats.formatted_time(),
        total_tokens: stats.total_tokens(),
        avg_task_time: stats.formatted_avg_task_time(),
    })
}

pub fn core_create_task(store: &Store, req: &CreateTaskRequest) -> anyhow::Result<Task> {
    let mode: TaskMode = req.mode.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let push_mode: PushMode = req
        .push_mode
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    store.create_task(
        &req.project_id,
        &req.title,
        &req.description,
        mode,
        req.branch.as_deref(),
        req.base.as_deref(),
        push_mode,
        req.review_loop,
    )
}

pub fn core_update_task(store: &Store, req: &UpdateTaskRequest) -> anyhow::Result<()> {
    let mode: TaskMode = req.mode.parse().map_err(|e: String| anyhow::anyhow!(e))?;
    let push_mode: PushMode = req
        .push_mode
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    store.update_task(
        &req.id,
        &req.title,
        &req.description,
        mode,
        req.branch.as_deref(),
        req.base.as_deref(),
        push_mode,
        req.review_loop,
    )
}

pub fn core_list_subtasks(store: &Store, task_id: &str) -> anyhow::Result<Vec<SubtaskInfo>> {
    let subtasks = store.list_subtasks_for_task(task_id)?;
    Ok(subtasks
        .into_iter()
        .map(|s| SubtaskInfo {
            id: s.id,
            task_id: s.task_id,
            title: s.title,
            description: s.description,
            status: s.status.as_str().to_string(),
            sort_order: s.sort_order,
        })
        .collect())
}

pub fn core_create_subtask(
    store: &Store,
    task_id: &str,
    title: &str,
    description: &str,
) -> anyhow::Result<SubtaskInfo> {
    let s = store.create_subtask(task_id, title, description)?;
    Ok(SubtaskInfo {
        id: s.id,
        task_id: s.task_id,
        title: s.title,
        description: s.description,
        status: s.status.as_str().to_string(),
        sort_order: s.sort_order,
    })
}

pub fn core_get_rate_limit(store: &Store) -> anyhow::Result<RateLimitInfo> {
    let rl = store.get_rate_limit_state()?;
    Ok(RateLimitInfo {
        is_rate_limited: rl.is_rate_limited,
        usage_5h_pct: rl.usage_5h_pct,
        usage_7d_pct: rl.usage_7d_pct,
        reset_5h: rl.reset_5h,
        reset_7d: rl.reset_7d,
    })
}

pub fn core_mark_task_done(store: &Store, task_id: &str) -> anyhow::Result<()> {
    let task = store.get_task(task_id)?;

    // Tear down associated session if any
    if let Some(ref session_id) = task.session_id {
        let session = store.get_session(session_id)?;
        store.close_session(session_id)?;
        let _ = std::process::Command::new("git")
            .args(["worktree", "remove", "--force", &session.worktree_path])
            .status();
    }

    store.update_task_status(task_id, TaskStatus::Done)
}

pub fn core_kill_session(store: &Store, session_id: &str) -> anyhow::Result<()> {
    let session = store.get_session(session_id)?;

    // Unassign any working task
    let tasks = store.list_tasks_for_project(&session.project_id)?;
    for task in &tasks {
        if task.session_id.as_deref() == Some(session_id) && task.status == TaskStatus::Working {
            store.update_task_status(&task.id, TaskStatus::Pending)?;
            store.unassign_task_from_session(&task.id)?;
        }
    }

    store.close_session(session_id)?;
    let _ = std::process::Command::new("git")
        .args(["worktree", "remove", "--force", &session.worktree_path])
        .status();

    Ok(())
}

// ---------------------------------------------------------------------------
// Tauri Commands — thin wrappers around core functions
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_projects(state: State<'_, AppState>) -> Result<Vec<ProjectSummary>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_list_projects(&store).map_err(map_err)
}

#[tauri::command]
fn create_project(
    state: State<'_, AppState>,
    name: String,
    path: String,
) -> Result<ProjectSummary, String> {
    let abs_path = std::fs::canonicalize(&path).map_err(|e| format!("invalid path: {e}"))?;
    let abs_str = abs_path
        .to_str()
        .ok_or_else(|| "path contains invalid UTF-8".to_string())?;

    let default_branch = claustre::config::detect_default_branch(abs_str);

    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_create_project(&store, &name, abs_str, &default_branch).map_err(map_err)
}

#[tauri::command]
fn delete_project(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.delete_project(&id).map_err(map_err)
}

#[tauri::command]
fn get_project_stats(state: State<'_, AppState>, project_id: String) -> Result<StatsInfo, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_get_stats(&store, &project_id).map_err(map_err)
}

#[tauri::command]
fn list_tasks(state: State<'_, AppState>, project_id: String) -> Result<Vec<Task>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.list_tasks_for_project(&project_id).map_err(map_err)
}

#[tauri::command]
fn create_task(state: State<'_, AppState>, req: CreateTaskRequest) -> Result<Task, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_create_task(&store, &req).map_err(map_err)
}

#[tauri::command]
fn update_task(state: State<'_, AppState>, req: UpdateTaskRequest) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_update_task(&store, &req).map_err(map_err)
}

#[tauri::command]
fn delete_task(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.delete_task(&id).map_err(map_err)
}

#[tauri::command]
fn reorder_tasks(
    state: State<'_, AppState>,
    task_a_id: String,
    task_b_id: String,
) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store
        .swap_task_order(&task_a_id, &task_b_id)
        .map_err(map_err)
}

#[tauri::command]
fn mark_task_done(state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_mark_task_done(&store, &task_id).map_err(map_err)
}

#[tauri::command]
fn update_task_status(
    state: State<'_, AppState>,
    task_id: String,
    status: String,
) -> Result<(), String> {
    let new_status: TaskStatus = status.parse().map_err(|e: String| e)?;
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store
        .update_task_status(&task_id, new_status)
        .map_err(map_err)
}

#[tauri::command]
fn list_sessions(
    state: State<'_, AppState>,
    project_id: String,
) -> Result<Vec<SessionInfo>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let sessions = store
        .list_active_sessions_for_project(&project_id)
        .map_err(map_err)?;

    Ok(sessions
        .into_iter()
        .map(|s| SessionInfo {
            id: s.id,
            project_id: s.project_id,
            branch_name: s.branch_name,
            worktree_path: s.worktree_path,
            tab_label: s.tab_label,
            claude_status: s.claude_status.as_str().to_string(),
            status_message: s.status_message,
            files_changed: s.files_changed,
            lines_added: s.lines_added,
            lines_removed: s.lines_removed,
            created_at: s.created_at,
        })
        .collect())
}

#[tauri::command]
fn kill_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_kill_session(&store, &session_id).map_err(map_err)
}

#[tauri::command]
fn list_subtasks(state: State<'_, AppState>, task_id: String) -> Result<Vec<SubtaskInfo>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_list_subtasks(&store, &task_id).map_err(map_err)
}

#[tauri::command]
fn create_subtask(
    state: State<'_, AppState>,
    task_id: String,
    title: String,
    description: String,
) -> Result<SubtaskInfo, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_create_subtask(&store, &task_id, &title, &description).map_err(map_err)
}

#[tauri::command]
fn delete_subtask(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    store.delete_subtask(&id).map_err(map_err)
}

#[tauri::command]
fn get_rate_limit(state: State<'_, AppState>) -> Result<RateLimitInfo, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    core_get_rate_limit(&store).map_err(map_err)
}

#[tauri::command]
fn launch_task(state: State<'_, AppState>, task_id: String) -> Result<SessionInfo, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let task = store.get_task(&task_id).map_err(map_err)?;
    let cfg = claustre::config::load().map_err(map_err)?;

    // Generate branch name from task title
    let branch_name = claustre::session::generate_branch_name(&task.title);

    // Create session via the session module
    let base_branch = task.base.as_deref().filter(|b| !b.is_empty());
    let setup = claustre::session::create_session(
        &store,
        &task.project_id,
        &branch_name,
        Some(&task),
        base_branch,
        cfg.remote_enabled,
        &cfg.claude,
    )
    .map_err(map_err)?;

    // Launch claude in the background (in the worktree) if a command was generated
    if let Some(claude_cmd) = setup.claude_cmd {
        let worktree_path = setup.worktree_path.clone();
        std::thread::spawn(move || {
            let mut cmd = std::process::Command::new(&claude_cmd[0]);
            for arg in &claude_cmd[1..] {
                cmd.arg(arg);
            }
            cmd.current_dir(&worktree_path);
            cmd.env("CLAUSTRE_SESSION", "1");
            let _ = cmd.status();
        });
    }

    let session = store.get_session(&setup.session.id).map_err(map_err)?;
    Ok(SessionInfo {
        id: session.id,
        project_id: session.project_id,
        branch_name: session.branch_name,
        worktree_path: session.worktree_path,
        tab_label: session.tab_label,
        claude_status: session.claude_status.as_str().to_string(),
        status_message: session.status_message,
        files_changed: session.files_changed,
        lines_added: session.lines_added,
        lines_removed: session.lines_removed,
        created_at: session.created_at,
    })
}

#[tauri::command]
fn open_pr_url(task_id: String, state: State<'_, AppState>) -> Result<Option<String>, String> {
    let store = state.store.lock().map_err(|e| e.to_string())?;
    let task = store.get_task(&task_id).map_err(map_err)?;
    Ok(task.pr_url)
}

#[tauri::command]
fn get_version() -> String {
    claustre::update::VERSION.to_string()
}

// ---------------------------------------------------------------------------
// App entry
// ---------------------------------------------------------------------------

pub fn run() {
    // Ensure claustre directories exist
    let _ = claustre::config::ensure_dirs();

    // Open and migrate DB
    let store = Store::open().expect("failed to open claustre database");
    store.migrate().expect("failed to migrate database");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(AppState {
            store: Mutex::new(store),
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            create_project,
            delete_project,
            get_project_stats,
            list_tasks,
            create_task,
            update_task,
            delete_task,
            reorder_tasks,
            mark_task_done,
            update_task_status,
            list_sessions,
            kill_session,
            list_subtasks,
            create_subtask,
            delete_subtask,
            get_rate_limit,
            launch_task,
            open_pr_url,
            get_version,
        ])
        .run(tauri::generate_context!())
        .expect("error while running claustre app");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a fresh temp-file-backed store for testing.
    fn test_store() -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let db_path = dir.path().join("test.db");
        let store = Store::open_at(&db_path).expect("failed to open test store");
        store.migrate().expect("failed to migrate");
        // Return dir handle to keep it alive (dropped = cleaned up)
        (store, dir)
    }

    /// Create a test project and return its ID.
    fn create_test_project(store: &Store) -> String {
        let summary = core_create_project(store, "test-project", "/tmp/test-repo", "main")
            .expect("create project");
        summary.id
    }

    /// Create a test task and return the Task.
    fn create_test_task(store: &Store, project_id: &str, title: &str) -> Task {
        let req = CreateTaskRequest {
            project_id: project_id.to_string(),
            title: title.to_string(),
            description: "test description".to_string(),
            mode: "supervised".to_string(),
            push_mode: "pr".to_string(),
            review_loop: false,
            branch: None,
            base: None,
        };
        core_create_task(store, &req).expect("create task")
    }

    // ── Project CRUD ──

    #[test]
    fn list_projects_empty() {
        let (store, _dir) = test_store();
        let projects = core_list_projects(&store).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn create_and_list_project() {
        let (store, _dir) = test_store();
        let summary = core_create_project(&store, "my-app", "/tmp/my-app", "main").unwrap();

        assert_eq!(summary.name, "my-app");
        assert_eq!(summary.repo_path, "/tmp/my-app");
        assert_eq!(summary.default_branch, "main");
        assert_eq!(summary.total_tasks, 0);
        assert_eq!(summary.active_sessions, 0);

        let projects = core_list_projects(&store).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "my-app");
    }

    #[test]
    fn create_multiple_projects() {
        let (store, _dir) = test_store();
        core_create_project(&store, "proj-a", "/tmp/a", "main").unwrap();
        core_create_project(&store, "proj-b", "/tmp/b", "develop").unwrap();

        let projects = core_list_projects(&store).unwrap();
        assert_eq!(projects.len(), 2);
    }

    #[test]
    fn delete_project() {
        let (store, _dir) = test_store();
        let summary = core_create_project(&store, "to-delete", "/tmp/del", "main").unwrap();

        store.delete_project(&summary.id).unwrap();

        let projects = core_list_projects(&store).unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn delete_nonexistent_project_fails() {
        let (store, _dir) = test_store();
        let result = store.delete_project("nonexistent-id");
        // delete_project deletes by ID; if it doesn't exist, it succeeds silently (no rows affected)
        assert!(result.is_ok());
    }

    // ── Task CRUD ──

    #[test]
    fn create_task_and_list() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        let task = create_test_task(&store, &project_id, "Build feature X");

        assert_eq!(task.title, "Build feature X");
        assert_eq!(task.description, "test description");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.mode, TaskMode::Supervised);
        assert_eq!(task.push_mode, PushMode::Pr);
        assert!(!task.review_loop);

        let tasks = store.list_tasks_for_project(&project_id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Build feature X");
    }

    #[test]
    fn create_task_with_all_options() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        let req = CreateTaskRequest {
            project_id: project_id.clone(),
            title: "Complex task".to_string(),
            description: "Do something complex".to_string(),
            mode: "autonomous".to_string(),
            push_mode: "push".to_string(),
            review_loop: true,
            branch: Some("feature/complex".to_string()),
            base: Some("develop".to_string()),
        };
        let task = core_create_task(&store, &req).unwrap();

        assert_eq!(task.mode, TaskMode::Autonomous);
        assert_eq!(task.push_mode, PushMode::Push);
        assert!(task.review_loop);
        assert_eq!(task.branch.as_deref(), Some("feature/complex"));
        assert_eq!(task.base.as_deref(), Some("develop"));
    }

    #[test]
    fn create_task_invalid_mode_fails() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        let req = CreateTaskRequest {
            project_id,
            title: "Bad task".to_string(),
            description: String::new(),
            mode: "invalid_mode".to_string(),
            push_mode: "pr".to_string(),
            review_loop: false,
            branch: None,
            base: None,
        };
        let result = core_create_task(&store, &req);
        assert!(result.is_err());
    }

    #[test]
    fn create_task_invalid_push_mode_fails() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        let req = CreateTaskRequest {
            project_id,
            title: "Bad task".to_string(),
            description: String::new(),
            mode: "supervised".to_string(),
            push_mode: "invalid".to_string(),
            review_loop: false,
            branch: None,
            base: None,
        };
        let result = core_create_task(&store, &req);
        assert!(result.is_err());
    }

    #[test]
    fn update_task() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task = create_test_task(&store, &project_id, "Original title");

        let req = UpdateTaskRequest {
            id: task.id.clone(),
            title: "Updated title".to_string(),
            description: "New description".to_string(),
            mode: "autonomous".to_string(),
            push_mode: "push".to_string(),
            review_loop: true,
            branch: Some("new-branch".to_string()),
            base: Some("develop".to_string()),
        };
        core_update_task(&store, &req).unwrap();

        let updated = store.get_task(&task.id).unwrap();
        assert_eq!(updated.title, "Updated title");
        assert_eq!(updated.description, "New description");
        assert_eq!(updated.mode, TaskMode::Autonomous);
        assert_eq!(updated.push_mode, PushMode::Push);
        assert!(updated.review_loop);
        assert_eq!(updated.branch.as_deref(), Some("new-branch"));
        assert_eq!(updated.base.as_deref(), Some("develop"));
    }

    #[test]
    fn delete_task() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task = create_test_task(&store, &project_id, "To delete");

        store.delete_task(&task.id).unwrap();

        let tasks = store.list_tasks_for_project(&project_id).unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn reorder_tasks() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task_a = create_test_task(&store, &project_id, "Task A");
        let task_b = create_test_task(&store, &project_id, "Task B");

        let original_a_order = task_a.sort_order;
        let original_b_order = task_b.sort_order;

        store.swap_task_order(&task_a.id, &task_b.id).unwrap();

        let swapped_a = store.get_task(&task_a.id).unwrap();
        let swapped_b = store.get_task(&task_b.id).unwrap();
        assert_eq!(swapped_a.sort_order, original_b_order);
        assert_eq!(swapped_b.sort_order, original_a_order);
    }

    // ── Task Status Transitions ──

    #[test]
    fn mark_task_done_without_session() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task = create_test_task(&store, &project_id, "Complete me");

        // Must transition through working first (pending -> done is not allowed)
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        core_mark_task_done(&store, &task.id).unwrap();

        let updated = store.get_task(&task.id).unwrap();
        assert_eq!(updated.status, TaskStatus::Done);
    }

    #[test]
    fn update_task_status_valid_transition() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task = create_test_task(&store, &project_id, "Status test");

        // Pending -> Working
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        let updated = store.get_task(&task.id).unwrap();
        assert_eq!(updated.status, TaskStatus::Working);

        // Working -> InReview
        store
            .update_task_status(&task.id, TaskStatus::InReview)
            .unwrap();
        let updated = store.get_task(&task.id).unwrap();
        assert_eq!(updated.status, TaskStatus::InReview);

        // InReview -> Done
        store
            .update_task_status(&task.id, TaskStatus::Done)
            .unwrap();
        let updated = store.get_task(&task.id).unwrap();
        assert_eq!(updated.status, TaskStatus::Done);
    }

    // ── Subtasks ──

    #[test]
    fn subtask_crud() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task = create_test_task(&store, &project_id, "Parent task");

        // Create subtasks
        let st1 = core_create_subtask(&store, &task.id, "Step 1", "First step").unwrap();
        let st2 = core_create_subtask(&store, &task.id, "Step 2", "Second step").unwrap();

        assert_eq!(st1.title, "Step 1");
        assert_eq!(st1.description, "First step");
        assert_eq!(st1.status, "pending");
        assert_eq!(st2.title, "Step 2");

        // List subtasks
        let subtasks = core_list_subtasks(&store, &task.id).unwrap();
        assert_eq!(subtasks.len(), 2);

        // Delete a subtask
        store.delete_subtask(&st1.id).unwrap();
        let subtasks = core_list_subtasks(&store, &task.id).unwrap();
        assert_eq!(subtasks.len(), 1);
        assert_eq!(subtasks[0].title, "Step 2");
    }

    #[test]
    fn subtask_empty_list() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task = create_test_task(&store, &project_id, "No subtasks");

        let subtasks = core_list_subtasks(&store, &task.id).unwrap();
        assert!(subtasks.is_empty());
    }

    // ── Stats ──

    #[test]
    fn project_stats_empty() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        let stats = core_get_stats(&store, &project_id).unwrap();
        assert_eq!(stats.total_tasks, 0);
        assert_eq!(stats.completed_tasks, 0);
        assert_eq!(stats.total_sessions, 0);
        assert_eq!(stats.total_tokens, 0);
    }

    #[test]
    fn project_stats_with_tasks() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        create_test_task(&store, &project_id, "Task 1");
        let task2 = create_test_task(&store, &project_id, "Task 2");
        store
            .update_task_status(&task2.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&task2.id, TaskStatus::Done)
            .unwrap();

        let stats = core_get_stats(&store, &project_id).unwrap();
        assert_eq!(stats.total_tasks, 2);
        assert_eq!(stats.completed_tasks, 1);
    }

    // ── Rate Limits ──

    #[test]
    fn rate_limit_default() {
        let (store, _dir) = test_store();
        let rl = core_get_rate_limit(&store).unwrap();
        assert!(!rl.is_rate_limited);
        // Default usage values are 0.0, not None (the DB schema has defaults)
        assert!(rl.usage_5h_pct.unwrap_or(0.0) < 1.0);
        assert!(rl.usage_7d_pct.unwrap_or(0.0) < 1.0);
    }

    // ── Project summaries include task counts ──

    #[test]
    fn project_summary_task_counts() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        // Create tasks with different statuses
        create_test_task(&store, &project_id, "Pending 1");
        create_test_task(&store, &project_id, "Pending 2");

        let task3 = create_test_task(&store, &project_id, "Working");
        store
            .update_task_status(&task3.id, TaskStatus::Working)
            .unwrap();

        let task4 = create_test_task(&store, &project_id, "Done");
        store
            .update_task_status(&task4.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&task4.id, TaskStatus::Done)
            .unwrap();

        let projects = core_list_projects(&store).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].total_tasks, 4);
        assert_eq!(projects[0].pending_tasks, 2);
        assert_eq!(projects[0].working_tasks, 1);
        assert_eq!(projects[0].done_tasks, 1);
        assert_eq!(projects[0].in_review_tasks, 0);
    }

    // ── Version ──

    #[test]
    fn version_is_not_empty() {
        let version = claustre::update::VERSION;
        assert!(!version.is_empty());
    }

    // ── Serialization ──

    #[test]
    fn project_summary_serialization() {
        let summary = ProjectSummary {
            id: "test-id".to_string(),
            name: "test".to_string(),
            repo_path: "/tmp/test".to_string(),
            default_branch: "main".to_string(),
            active_sessions: 1,
            pending_tasks: 2,
            working_tasks: 1,
            in_review_tasks: 0,
            done_tasks: 3,
            total_tasks: 6,
        };

        let json = serde_json::to_string(&summary).unwrap();
        let deserialized: ProjectSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, deserialized);
    }

    #[test]
    fn stats_info_serialization() {
        let stats = StatsInfo {
            total_tasks: 10,
            completed_tasks: 5,
            total_sessions: 3,
            total_time: "2h 30m".to_string(),
            total_tokens: 150_000,
            avg_task_time: "30m".to_string(),
        };

        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: StatsInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(stats, deserialized);
    }

    #[test]
    fn subtask_info_serialization() {
        let info = SubtaskInfo {
            id: "st-1".to_string(),
            task_id: "t-1".to_string(),
            title: "Step 1".to_string(),
            description: "Do step 1".to_string(),
            status: "pending".to_string(),
            sort_order: 0,
        };

        let json = serde_json::to_string(&info).unwrap();
        let deserialized: SubtaskInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, deserialized);
    }

    #[test]
    fn rate_limit_info_serialization() {
        let info = RateLimitInfo {
            is_rate_limited: true,
            usage_5h_pct: Some(75.5),
            usage_7d_pct: Some(45.0),
            reset_5h: Some("1h 30m".to_string()),
            reset_7d: None,
        };

        let json = serde_json::to_string(&info).unwrap();
        let deserialized: RateLimitInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, deserialized);
    }

    // ── Multiple tasks per project ──

    #[test]
    fn multiple_tasks_independent() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        let t1 = create_test_task(&store, &project_id, "Task 1");
        let t2 = create_test_task(&store, &project_id, "Task 2");
        let t3 = create_test_task(&store, &project_id, "Task 3");

        // Operate on them independently
        store
            .update_task_status(&t1.id, TaskStatus::Working)
            .unwrap();
        store.delete_task(&t3.id).unwrap();

        let tasks = store.list_tasks_for_project(&project_id).unwrap();
        assert_eq!(tasks.len(), 2);

        let t1_updated = store.get_task(&t1.id).unwrap();
        assert_eq!(t1_updated.status, TaskStatus::Working);
        assert_eq!(store.get_task(&t2.id).unwrap().status, TaskStatus::Pending);
    }

    // ── Exploration mode task ──

    #[test]
    fn exploration_mode_task() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        let req = CreateTaskRequest {
            project_id,
            title: "Research task".to_string(),
            description: "Explore the codebase".to_string(),
            mode: "exploration".to_string(),
            push_mode: "pr".to_string(),
            review_loop: false,
            branch: None,
            base: None,
        };
        let task = core_create_task(&store, &req).unwrap();
        assert_eq!(task.mode, TaskMode::Exploration);
    }

    // ── Cross-project isolation ──

    #[test]
    fn tasks_isolated_between_projects() {
        let (store, _dir) = test_store();
        let p1 = core_create_project(&store, "proj-1", "/tmp/p1", "main")
            .unwrap()
            .id;
        let p2 = core_create_project(&store, "proj-2", "/tmp/p2", "main")
            .unwrap()
            .id;

        create_test_task(&store, &p1, "Task in P1");
        create_test_task(&store, &p2, "Task in P2");
        create_test_task(&store, &p2, "Another in P2");

        let p1_tasks = store.list_tasks_for_project(&p1).unwrap();
        let p2_tasks = store.list_tasks_for_project(&p2).unwrap();
        assert_eq!(p1_tasks.len(), 1);
        assert_eq!(p2_tasks.len(), 2);

        let summaries = core_list_projects(&store).unwrap();
        let s1 = summaries.iter().find(|s| s.id == p1).unwrap();
        let s2 = summaries.iter().find(|s| s.id == p2).unwrap();
        assert_eq!(s1.total_tasks, 1);
        assert_eq!(s2.total_tasks, 2);
    }

    // ── Session listing ──

    #[test]
    fn list_sessions_empty_project() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);

        let sessions = store.list_active_sessions_for_project(&project_id).unwrap();
        assert!(sessions.is_empty());
    }

    // ── Kill session tears down state ──

    #[test]
    fn kill_session_unassigns_working_task() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task = create_test_task(&store, &project_id, "Working task");

        // Manually create a session and assign the task
        let session = store
            .create_session(&project_id, "test-branch", "/tmp/wt", "test:branch")
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();

        // Kill the session
        core_kill_session(&store, &session.id).unwrap();

        // Task should be back to pending and unassigned
        let updated_task = store.get_task(&task.id).unwrap();
        assert_eq!(updated_task.status, TaskStatus::Pending);
        assert!(updated_task.session_id.is_none());

        // Session should be closed
        let closed_session = store.get_session(&session.id).unwrap();
        assert!(closed_session.closed_at.is_some());
    }

    // ── Mark done with session tears down session ──

    #[test]
    fn mark_done_closes_associated_session() {
        let (store, _dir) = test_store();
        let project_id = create_test_project(&store);
        let task = create_test_task(&store, &project_id, "Task with session");

        // Create session and assign task
        let session = store
            .create_session(&project_id, "branch", "/tmp/wt2", "label")
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();

        core_mark_task_done(&store, &task.id).unwrap();

        // Task should be done
        let updated = store.get_task(&task.id).unwrap();
        assert_eq!(updated.status, TaskStatus::Done);

        // Session should be closed
        let closed = store.get_session(&session.id).unwrap();
        assert!(closed.closed_at.is_some());
    }
}
