use anyhow::Result;
use rusqlite::params;
use uuid::Uuid;

use super::Store;
use super::models::{ClaudeStatus, Project, RateLimitState, Session, Task, TaskMode, TaskStatus};

impl Store {
    // ── Projects ──

    pub fn create_project(&self, name: &str, repo_path: &str) -> Result<Project> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO projects (id, name, repo_path) VALUES (?1, ?2, ?3)",
            params![id, name, repo_path],
        )?;
        self.get_project(&id)
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        let project = self.conn.query_row(
            "SELECT id, name, repo_path, created_at FROM projects WHERE id = ?1",
            params![id],
            |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    repo_path: row.get(2)?,
                    created_at: row.get(3)?,
                })
            },
        )?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, repo_path, created_at FROM projects ORDER BY name")?;
        let projects = stmt
            .query_map([], |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    repo_path: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(projects)
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tasks WHERE project_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM sessions WHERE project_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM projects WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ── Tasks ──

    pub fn create_task(
        &self,
        project_id: &str,
        title: &str,
        description: &str,
        mode: TaskMode,
    ) -> Result<Task> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO tasks (id, project_id, title, description, mode) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, project_id, title, description, mode.as_str()],
        )?;
        self.get_task(&id)
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        let task = self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, cost
             FROM tasks WHERE id = ?1",
            params![id],
            |row| {
                let status_str: String = row.get(4)?;
                let mode_str: String = row.get(5)?;
                Ok(Task {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    status: TaskStatus::from_str(&status_str),
                    mode: TaskMode::from_str(&mode_str),
                    session_id: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    started_at: row.get(9)?,
                    completed_at: row.get(10)?,
                    input_tokens: row.get(11)?,
                    output_tokens: row.get(12)?,
                    cost: row.get(13)?,
                })
            },
        )?;
        Ok(task)
    }

    pub fn list_tasks_for_project(&self, project_id: &str) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, cost
             FROM tasks WHERE project_id = ?1
             ORDER BY created_at",
        )?;
        let tasks = stmt
            .query_map(params![project_id], |row| {
                let status_str: String = row.get(4)?;
                let mode_str: String = row.get(5)?;
                Ok(Task {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    status: TaskStatus::from_str(&status_str),
                    mode: TaskMode::from_str(&mode_str),
                    session_id: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    started_at: row.get(9)?,
                    completed_at: row.get(10)?,
                    input_tokens: row.get(11)?,
                    output_tokens: row.get(12)?,
                    cost: row.get(13)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn update_task_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now, id],
        )?;

        match status {
            TaskStatus::InProgress => {
                self.conn.execute(
                    "UPDATE tasks SET started_at = ?1 WHERE id = ?2 AND started_at IS NULL",
                    params![now, id],
                )?;
            }
            TaskStatus::Done => {
                self.conn.execute(
                    "UPDATE tasks SET completed_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    pub fn assign_task_to_session(&self, task_id: &str, session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET session_id = ?1 WHERE id = ?2",
            params![session_id, task_id],
        )?;
        Ok(())
    }

    pub fn update_task_usage(
        &self,
        id: &str,
        input_tokens: i64,
        output_tokens: i64,
        cost: f64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET input_tokens = input_tokens + ?1, output_tokens = output_tokens + ?2, cost = cost + ?3 WHERE id = ?4",
            params![input_tokens, output_tokens, cost, id],
        )?;
        Ok(())
    }

    pub fn next_pending_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        let result = self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, cost
             FROM tasks
             WHERE session_id = ?1 AND status = 'pending' AND mode = 'autonomous'
             ORDER BY created_at
             LIMIT 1",
            params![session_id],
            |row| {
                let status_str: String = row.get(4)?;
                let mode_str: String = row.get(5)?;
                Ok(Task {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    title: row.get(2)?,
                    description: row.get(3)?,
                    status: TaskStatus::from_str(&status_str),
                    mode: TaskMode::from_str(&mode_str),
                    session_id: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    started_at: row.get(9)?,
                    completed_at: row.get(10)?,
                    input_tokens: row.get(11)?,
                    output_tokens: row.get(12)?,
                    cost: row.get(13)?,
                })
            },
        );
        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ── Sessions ──

    pub fn create_session(
        &self,
        project_id: &str,
        branch_name: &str,
        worktree_path: &str,
        zellij_tab_name: &str,
    ) -> Result<Session> {
        let id = Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO sessions (id, project_id, branch_name, worktree_path, zellij_tab_name)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, project_id, branch_name, worktree_path, zellij_tab_name],
        )?;
        self.get_session(&id)
    }

    pub fn get_session(&self, id: &str) -> Result<Session> {
        let session = self.conn.query_row(
            "SELECT id, project_id, branch_name, worktree_path, zellij_tab_name,
                    claude_status, status_message, last_activity_at,
                    files_changed, lines_added, lines_removed,
                    created_at, closed_at
             FROM sessions WHERE id = ?1",
            params![id],
            |row| {
                let status_str: String = row.get(5)?;
                Ok(Session {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    branch_name: row.get(2)?,
                    worktree_path: row.get(3)?,
                    zellij_tab_name: row.get(4)?,
                    claude_status: ClaudeStatus::from_str(&status_str),
                    status_message: row.get(6)?,
                    last_activity_at: row.get(7)?,
                    files_changed: row.get(8)?,
                    lines_added: row.get(9)?,
                    lines_removed: row.get(10)?,
                    created_at: row.get(11)?,
                    closed_at: row.get(12)?,
                })
            },
        )?;
        Ok(session)
    }

    pub fn list_active_sessions_for_project(&self, project_id: &str) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, branch_name, worktree_path, zellij_tab_name,
                    claude_status, status_message, last_activity_at,
                    files_changed, lines_added, lines_removed,
                    created_at, closed_at
             FROM sessions
             WHERE project_id = ?1 AND closed_at IS NULL
             ORDER BY created_at",
        )?;
        let sessions = stmt
            .query_map(params![project_id], |row| {
                let status_str: String = row.get(5)?;
                Ok(Session {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    branch_name: row.get(2)?,
                    worktree_path: row.get(3)?,
                    zellij_tab_name: row.get(4)?,
                    claude_status: ClaudeStatus::from_str(&status_str),
                    status_message: row.get(6)?,
                    last_activity_at: row.get(7)?,
                    files_changed: row.get(8)?,
                    lines_added: row.get(9)?,
                    lines_removed: row.get(10)?,
                    created_at: row.get(11)?,
                    closed_at: row.get(12)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    pub fn update_session_status(
        &self,
        id: &str,
        claude_status: ClaudeStatus,
        message: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET claude_status = ?1, status_message = ?2, last_activity_at = ?3 WHERE id = ?4",
            params![claude_status.as_str(), message, now, id],
        )?;
        Ok(())
    }

    pub fn update_session_git_stats(
        &self,
        id: &str,
        files_changed: i64,
        lines_added: i64,
        lines_removed: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET files_changed = ?1, lines_added = ?2, lines_removed = ?3 WHERE id = ?4",
            params![files_changed, lines_added, lines_removed, id],
        )?;
        Ok(())
    }

    pub fn close_session(&self, id: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET closed_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        Ok(())
    }

    // ── Rate Limiting ──

    pub fn get_rate_limit_state(&self) -> Result<RateLimitState> {
        let state = self.conn.query_row(
            "SELECT is_rate_limited, limit_type, rate_limited_at, reset_at,
                    usage_5h_pct, usage_7d_pct, updated_at
             FROM rate_limit_state WHERE id = 1",
            [],
            |row| {
                let is_rate_limited: i64 = row.get(0)?;
                Ok(RateLimitState {
                    is_rate_limited: is_rate_limited != 0,
                    limit_type: row.get(1)?,
                    rate_limited_at: row.get(2)?,
                    reset_at: row.get(3)?,
                    usage_5h_pct: row.get(4)?,
                    usage_7d_pct: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )?;
        Ok(state)
    }

    #[expect(
        clippy::similar_names,
        reason = "5h and 7d are distinct domain-specific window labels"
    )]
    pub fn set_rate_limited(
        &self,
        limit_type: &str,
        reset_at: &str,
        usage_5h_pct: f64,
        usage_7d_pct: f64,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                is_rate_limited = 1,
                limit_type = ?1,
                rate_limited_at = ?2,
                reset_at = ?3,
                usage_5h_pct = ?4,
                usage_7d_pct = ?5,
                updated_at = ?2
             WHERE id = 1",
            params![limit_type, now, reset_at, usage_5h_pct, usage_7d_pct],
        )?;
        Ok(())
    }

    pub fn clear_rate_limit(&self) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                is_rate_limited = 0,
                limit_type = NULL,
                rate_limited_at = NULL,
                reset_at = NULL,
                updated_at = ?1
             WHERE id = 1",
            params![now],
        )?;
        Ok(())
    }

    #[expect(
        clippy::similar_names,
        reason = "5h and 7d are distinct domain-specific window labels"
    )]
    pub fn update_usage_windows(&self, usage_5h_pct: f64, usage_7d_pct: f64) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                usage_5h_pct = ?1,
                usage_7d_pct = ?2,
                updated_at = ?3
             WHERE id = 1",
            params![usage_5h_pct, usage_7d_pct, now],
        )?;
        Ok(())
    }

    // ── Stats ──

    pub fn project_stats(&self, project_id: &str) -> Result<ProjectStats> {
        let total_tasks: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;

        let completed_tasks: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE project_id = ?1 AND status = 'done'",
            params![project_id],
            |row| row.get(0),
        )?;

        let total_sessions: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;

        let total_input_tokens: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(input_tokens), 0) FROM tasks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;

        let total_output_tokens: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(output_tokens), 0) FROM tasks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;

        let total_cost: f64 = self.conn.query_row(
            "SELECT COALESCE(SUM(cost), 0.0) FROM tasks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;

        // Total time: sum of (completed_at - started_at) for done tasks
        let total_time_seconds: i64 = self.conn.query_row(
            "SELECT COALESCE(SUM(strftime('%s', completed_at) - strftime('%s', started_at)), 0)
             FROM tasks
             WHERE project_id = ?1 AND status = 'done' AND started_at IS NOT NULL AND completed_at IS NOT NULL",
            params![project_id],
            |row| row.get(0),
        )?;

        Ok(ProjectStats {
            total_tasks,
            completed_tasks,
            total_sessions,
            total_input_tokens,
            total_output_tokens,
            total_cost,
            total_time_seconds,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ProjectStats {
    pub total_tasks: i64,
    pub completed_tasks: i64,
    pub total_sessions: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_cost: f64,
    pub total_time_seconds: i64,
}

impl ProjectStats {
    pub fn total_tokens(&self) -> i64 {
        self.total_input_tokens + self.total_output_tokens
    }

    pub fn formatted_time(&self) -> String {
        let hours = self.total_time_seconds / 3600;
        let minutes = (self.total_time_seconds % 3600) / 60;
        if hours > 0 {
            format!("{hours}h {minutes}m")
        } else {
            format!("{minutes}m")
        }
    }

    pub fn avg_task_time_seconds(&self) -> i64 {
        if self.completed_tasks == 0 {
            0
        } else {
            self.total_time_seconds / self.completed_tasks
        }
    }

    pub fn formatted_avg_task_time(&self) -> String {
        let secs = self.avg_task_time_seconds();
        let minutes = secs / 60;
        if minutes == 0 {
            format!("{secs}s")
        } else {
            format!("{minutes}m")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_get_project() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("test-proj", "/tmp/repo").unwrap();
        assert_eq!(project.name, "test-proj");
        assert_eq!(project.repo_path, "/tmp/repo");

        let fetched = store.get_project(&project.id).unwrap();
        assert_eq!(fetched.id, project.id);
        assert_eq!(fetched.name, "test-proj");
    }

    #[test]
    fn test_list_projects() {
        let store = Store::open_in_memory().unwrap();
        store.create_project("beta", "/tmp/beta").unwrap();
        store.create_project("alpha", "/tmp/alpha").unwrap();

        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 2);
        // Ordered by name
        assert_eq!(projects[0].name, "alpha");
        assert_eq!(projects[1].name, "beta");
    }

    #[test]
    fn test_delete_project() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("doomed", "/tmp/doomed").unwrap();
        store
            .create_task(&project.id, "task1", "", TaskMode::Supervised)
            .unwrap();

        store.delete_project(&project.id).unwrap();

        let projects = store.list_projects().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_create_task() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "do stuff", "details", TaskMode::Autonomous)
            .unwrap();

        assert_eq!(task.title, "do stuff");
        assert_eq!(task.description, "details");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.mode, TaskMode::Autonomous);
    }

    #[test]
    fn test_task_lifecycle() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "lifecycle", "", TaskMode::Supervised)
            .unwrap();

        // pending -> in_progress
        store
            .update_task_status(&task.id, TaskStatus::InProgress)
            .unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.status, TaskStatus::InProgress);
        assert!(t.started_at.is_some());

        // in_progress -> in_review
        store
            .update_task_status(&task.id, TaskStatus::InReview)
            .unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.status, TaskStatus::InReview);

        // in_review -> done
        store
            .update_task_status(&task.id, TaskStatus::Done)
            .unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.status, TaskStatus::Done);
        assert!(t.completed_at.is_some());
    }

    #[test]
    fn test_list_tasks_for_project() {
        let store = Store::open_in_memory().unwrap();
        let p1 = store.create_project("p1", "/tmp/p1").unwrap();
        let p2 = store.create_project("p2", "/tmp/p2").unwrap();

        store
            .create_task(&p1.id, "t1", "", TaskMode::Supervised)
            .unwrap();
        store
            .create_task(&p1.id, "t2", "", TaskMode::Supervised)
            .unwrap();
        store
            .create_task(&p2.id, "t3", "", TaskMode::Supervised)
            .unwrap();

        let tasks = store.list_tasks_for_project(&p1.id).unwrap();
        assert_eq!(tasks.len(), 2);

        let tasks = store.list_tasks_for_project(&p2.id).unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn test_create_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "feat-branch", "/tmp/wt", "tab-1")
            .unwrap();

        assert_eq!(session.branch_name, "feat-branch");
        assert_eq!(session.worktree_path, "/tmp/wt");
        assert_eq!(session.zellij_tab_name, "tab-1");
        assert_eq!(session.claude_status, ClaudeStatus::Idle);
        assert!(session.closed_at.is_none());
    }

    #[test]
    fn test_update_session_status() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "branch", "/tmp/wt", "tab")
            .unwrap();

        store
            .update_session_status(&session.id, ClaudeStatus::Working, "doing things")
            .unwrap();

        let s = store.get_session(&session.id).unwrap();
        assert_eq!(s.claude_status, ClaudeStatus::Working);
        assert_eq!(s.status_message, "doing things");
    }

    #[test]
    fn test_close_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "branch", "/tmp/wt", "tab")
            .unwrap();

        store.close_session(&session.id).unwrap();

        let s = store.get_session(&session.id).unwrap();
        assert!(s.closed_at.is_some());

        // Closed session should not appear in active list
        let active = store.list_active_sessions_for_project(&project.id).unwrap();
        assert!(active.is_empty());
    }

    #[test]
    fn test_project_stats() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();

        store
            .create_task(&project.id, "t1", "", TaskMode::Supervised)
            .unwrap();
        store
            .create_task(&project.id, "t2", "", TaskMode::Supervised)
            .unwrap();
        store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();

        let stats = store.project_stats(&project.id).unwrap();
        assert_eq!(stats.total_tasks, 2);
        assert_eq!(stats.completed_tasks, 0);
        assert_eq!(stats.total_sessions, 1);
    }

    #[test]
    fn test_next_pending_task_for_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();

        // Supervised task should not be returned
        let t1 = store
            .create_task(&project.id, "supervised", "", TaskMode::Supervised)
            .unwrap();
        store.assign_task_to_session(&t1.id, &session.id).unwrap();

        assert!(
            store
                .next_pending_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // Autonomous task assigned to session should be returned
        let t2 = store
            .create_task(&project.id, "auto", "", TaskMode::Autonomous)
            .unwrap();
        store.assign_task_to_session(&t2.id, &session.id).unwrap();

        let next = store
            .next_pending_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(next.id, t2.id);
    }

    #[test]
    fn test_rate_limit_state_default() {
        let store = Store::open_in_memory().unwrap();
        let state = store.get_rate_limit_state().unwrap();
        assert!(!state.is_rate_limited);
        assert!(state.limit_type.is_none());
        assert_eq!(state.usage_5h_pct, 0.0);
        assert_eq!(state.usage_7d_pct, 0.0);
    }

    #[test]
    fn test_set_and_clear_rate_limit() {
        let store = Store::open_in_memory().unwrap();

        store
            .set_rate_limited("5h", "2026-02-08T20:00:00Z", 95.0, 30.0)
            .unwrap();
        let state = store.get_rate_limit_state().unwrap();
        assert!(state.is_rate_limited);
        assert_eq!(state.limit_type.as_deref(), Some("5h"));
        assert_eq!(state.reset_at.as_deref(), Some("2026-02-08T20:00:00Z"));
        assert_eq!(state.usage_5h_pct, 95.0);
        assert_eq!(state.usage_7d_pct, 30.0);

        store.clear_rate_limit().unwrap();
        let state = store.get_rate_limit_state().unwrap();
        assert!(!state.is_rate_limited);
        assert!(state.limit_type.is_none());
        assert!(state.reset_at.is_none());
    }

    #[test]
    fn test_update_usage_windows() {
        let store = Store::open_in_memory().unwrap();

        store.update_usage_windows(45.0, 12.5).unwrap();
        let state = store.get_rate_limit_state().unwrap();
        assert_eq!(state.usage_5h_pct, 45.0);
        assert_eq!(state.usage_7d_pct, 12.5);
        assert!(!state.is_rate_limited);
    }
}
