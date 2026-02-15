use anyhow::Result;
use rusqlite::params;
use uuid::Uuid;

use super::Store;
use super::models::{
    ClaudeProgressItem, ClaudeStatus, Project, RateLimitState, Session, Subtask, Task, TaskMode,
    TaskStatus,
};

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
        let max_order: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) FROM tasks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO tasks (id, project_id, title, description, mode, sort_order) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, project_id, title, description, mode.as_str(), max_order + 1],
        )?;
        self.get_task(&id)
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        let task = self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, cost, sort_order, pr_url
             FROM tasks WHERE id = ?1",
            params![id],
            Self::row_to_task,
        )?;
        Ok(task)
    }

    pub fn list_tasks_for_project(&self, project_id: &str) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, cost, sort_order, pr_url
             FROM tasks WHERE project_id = ?1
             ORDER BY sort_order, created_at",
        )?;
        let tasks = stmt
            .query_map(params![project_id], Self::row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    pub fn update_task(
        &self,
        id: &str,
        title: &str,
        description: &str,
        mode: TaskMode,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET title = ?1, description = ?2, mode = ?3, updated_at = ?4 WHERE id = ?5",
            params![title, description, mode.as_str(), now, id],
        )?;
        Ok(())
    }

    pub fn delete_task(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    #[expect(clippy::similar_names, reason = "a/b suffix is clearest for swap")]
    pub fn swap_task_order(&self, task_a_id: &str, task_b_id: &str) -> Result<()> {
        let order_a: i64 = self.conn.query_row(
            "SELECT sort_order FROM tasks WHERE id = ?1",
            params![task_a_id],
            |row| row.get(0),
        )?;
        let order_b: i64 = self.conn.query_row(
            "SELECT sort_order FROM tasks WHERE id = ?1",
            params![task_b_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "UPDATE tasks SET sort_order = ?1 WHERE id = ?2",
            params![order_b, task_a_id],
        )?;
        self.conn.execute(
            "UPDATE tasks SET sort_order = ?1 WHERE id = ?2",
            params![order_a, task_b_id],
        )?;
        Ok(())
    }

    fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
        let status_str: String = row.get(4)?;
        let mode_str: String = row.get(5)?;
        Ok(Task {
            id: row.get(0)?,
            project_id: row.get(1)?,
            title: row.get(2)?,
            description: row.get(3)?,
            status: status_str.parse().unwrap_or(TaskStatus::Pending),
            mode: mode_str.parse().unwrap_or(TaskMode::Supervised),
            session_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            started_at: row.get(9)?,
            completed_at: row.get(10)?,
            input_tokens: row.get(11)?,
            output_tokens: row.get(12)?,
            cost: row.get(13)?,
            sort_order: row.get(14)?,
            pr_url: row.get(15)?,
        })
    }

    pub fn update_task_title(&self, id: &str, title: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, now, id],
        )?;
        Ok(())
    }

    pub fn update_task_pr_url(&self, id: &str, pr_url: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET pr_url = ?1 WHERE id = ?2",
            params![pr_url, id],
        )?;
        Ok(())
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

    #[allow(dead_code, reason = "retained for future CLI/API usage tracking")]
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

    /// Find the in-progress task assigned to a session (if any).
    pub fn in_progress_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        let result = self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, cost, sort_order, pr_url
             FROM tasks
             WHERE session_id = ?1 AND status = 'in_progress'
             LIMIT 1",
            params![session_id],
            Self::row_to_task,
        );
        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Find the in-review task assigned to a session (if any).
    pub fn in_review_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        let result = self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, cost, sort_order, pr_url
             FROM tasks
             WHERE session_id = ?1 AND status = 'in_review'
             LIMIT 1",
            params![session_id],
            Self::row_to_task,
        );
        match result {
            Ok(task) => Ok(Some(task)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn next_pending_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        let result = self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, cost, sort_order, pr_url
             FROM tasks
             WHERE session_id = ?1 AND status = 'pending' AND mode = 'autonomous'
             ORDER BY sort_order, created_at
             LIMIT 1",
            params![session_id],
            Self::row_to_task,
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
                    created_at, closed_at, claude_progress
             FROM sessions WHERE id = ?1",
            params![id],
            Self::row_to_session,
        )?;
        Ok(session)
    }

    pub fn list_active_sessions_for_project(&self, project_id: &str) -> Result<Vec<Session>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, branch_name, worktree_path, zellij_tab_name,
                    claude_status, status_message, last_activity_at,
                    files_changed, lines_added, lines_removed,
                    created_at, closed_at, claude_progress
             FROM sessions
             WHERE project_id = ?1 AND closed_at IS NULL
             ORDER BY created_at",
        )?;
        let sessions = stmt
            .query_map(params![project_id], Self::row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
        let status_str: String = row.get(5)?;
        let progress_str: String = row.get(13)?;
        let claude_progress = if progress_str.is_empty() {
            vec![]
        } else {
            serde_json::from_str(&progress_str).unwrap_or_default()
        };
        Ok(Session {
            id: row.get(0)?,
            project_id: row.get(1)?,
            branch_name: row.get(2)?,
            worktree_path: row.get(3)?,
            zellij_tab_name: row.get(4)?,
            claude_status: status_str.parse().unwrap_or(ClaudeStatus::Idle),
            status_message: row.get(6)?,
            last_activity_at: row.get(7)?,
            files_changed: row.get(8)?,
            lines_added: row.get(9)?,
            lines_removed: row.get(10)?,
            created_at: row.get(11)?,
            closed_at: row.get(12)?,
            claude_progress,
        })
    }

    pub fn update_session_status(
        &self,
        id: &str,
        claude_status: ClaudeStatus,
        message: &str,
    ) -> Result<()> {
        // Only update last_activity_at when Claude finishes a turn (not when starting work)
        if claude_status == ClaudeStatus::Working {
            self.conn.execute(
                "UPDATE sessions SET claude_status = ?1, status_message = ?2 WHERE id = ?3",
                params![claude_status.as_str(), message, id],
            )?;
        } else {
            let now = chrono::Utc::now().to_rfc3339();
            self.conn.execute(
                "UPDATE sessions SET claude_status = ?1, status_message = ?2, last_activity_at = ?3 WHERE id = ?4",
                params![claude_status.as_str(), message, now, id],
            )?;
        }
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

    pub fn update_session_progress(
        &self,
        id: &str,
        progress: &[ClaudeProgressItem],
    ) -> Result<()> {
        let json = serde_json::to_string(progress)?;
        self.conn.execute(
            "UPDATE sessions SET claude_progress = ?1 WHERE id = ?2",
            params![json, id],
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
                    usage_5h_pct: Some(row.get(4)?),
                    usage_7d_pct: Some(row.get(5)?),
                    reset_5h: None,
                    reset_7d: None,
                    updated_at: row.get(6)?,
                })
            },
        )?;
        Ok(state)
    }

    #[allow(dead_code, reason = "retained for future CLI rate limit reporting")]
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

    #[allow(dead_code, reason = "retained for future CLI usage window reporting")]
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

    // ── Subtasks ──

    #[allow(dead_code, reason = "used in tests and future TUI wiring")]
    pub fn create_subtask(&self, task_id: &str, title: &str, description: &str) -> Result<Subtask> {
        let id = Uuid::new_v4().to_string();
        let max_order: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) FROM subtasks WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO subtasks (id, task_id, title, description, sort_order) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, task_id, title, description, max_order + 1],
        )?;
        self.get_subtask(&id)
    }

    #[allow(dead_code, reason = "used in tests and future TUI wiring")]
    pub fn get_subtask(&self, id: &str) -> Result<Subtask> {
        let subtask = self.conn.query_row(
            "SELECT id, task_id, title, description, status, sort_order,
                    created_at, started_at, completed_at
             FROM subtasks WHERE id = ?1",
            params![id],
            Self::row_to_subtask,
        )?;
        Ok(subtask)
    }

    pub fn list_subtasks_for_task(&self, task_id: &str) -> Result<Vec<Subtask>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, title, description, status, sort_order,
                    created_at, started_at, completed_at
             FROM subtasks WHERE task_id = ?1
             ORDER BY sort_order, created_at",
        )?;
        let subtasks = stmt
            .query_map(params![task_id], Self::row_to_subtask)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(subtasks)
    }

    pub fn update_subtask_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE subtasks SET status = ?1 WHERE id = ?2",
            params![status.as_str(), id],
        )?;

        let now = chrono::Utc::now().to_rfc3339();
        match status {
            TaskStatus::InProgress => {
                self.conn.execute(
                    "UPDATE subtasks SET started_at = ?1 WHERE id = ?2 AND started_at IS NULL",
                    params![now, id],
                )?;
            }
            TaskStatus::Done => {
                self.conn.execute(
                    "UPDATE subtasks SET completed_at = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    #[allow(dead_code, reason = "used in tests and future TUI wiring")]
    pub fn delete_subtask(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM subtasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn next_pending_subtask(&self, task_id: &str) -> Result<Option<Subtask>> {
        let result = self.conn.query_row(
            "SELECT id, task_id, title, description, status, sort_order,
                    created_at, started_at, completed_at
             FROM subtasks
             WHERE task_id = ?1 AND status = 'pending'
             ORDER BY sort_order, created_at
             LIMIT 1",
            params![task_id],
            Self::row_to_subtask,
        );
        match result {
            Ok(subtask) => Ok(Some(subtask)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    #[allow(dead_code, reason = "used in tests and future TUI wiring")]
    pub fn subtask_count(&self, task_id: &str) -> Result<(i64, i64)> {
        let (total, done) = self.conn.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END)
             FROM subtasks WHERE task_id = ?1",
            params![task_id],
            |row| {
                let total: i64 = row.get(0)?;
                let done: i64 = row.get::<_, Option<i64>>(1)?.unwrap_or(0);
                Ok((total, done))
            },
        )?;
        Ok((total, done))
    }

    fn row_to_subtask(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subtask> {
        let status_str: String = row.get(4)?;
        Ok(Subtask {
            id: row.get(0)?,
            task_id: row.get(1)?,
            title: row.get(2)?,
            description: row.get(3)?,
            status: status_str.parse().unwrap_or(TaskStatus::Pending),
            sort_order: row.get(5)?,
            created_at: row.get(6)?,
            started_at: row.get(7)?,
            completed_at: row.get(8)?,
        })
    }

    // ── Stats ──

    pub fn project_stats(&self, project_id: &str) -> Result<ProjectStats> {
        let stats = self.conn.query_row(
            "SELECT
                COUNT(*),
                SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END),
                (SELECT COUNT(*) FROM sessions WHERE project_id = ?1),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(cost), 0.0),
                COALESCE(SUM(
                    CASE WHEN status = 'done' AND started_at IS NOT NULL AND completed_at IS NOT NULL
                    THEN strftime('%s', completed_at) - strftime('%s', started_at)
                    ELSE 0 END
                ), 0)
             FROM tasks WHERE project_id = ?1",
            params![project_id],
            |row| {
                Ok(ProjectStats {
                    total_tasks: row.get(0)?,
                    completed_tasks: row.get(1)?,
                    total_sessions: row.get(2)?,
                    total_input_tokens: row.get(3)?,
                    total_output_tokens: row.get(4)?,
                    total_cost: row.get(5)?,
                    total_time_seconds: row.get(6)?,
                })
            },
        )?;
        Ok(stats)
    }

    pub fn has_review_tasks(&self, project_id: &str) -> Result<bool> {
        let has: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM tasks WHERE project_id = ?1 AND status = 'in_review')",
            params![project_id],
            |row| row.get(0),
        )?;
        Ok(has)
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
        assert_eq!(state.usage_5h_pct, Some(0.0));
        assert_eq!(state.usage_7d_pct, Some(0.0));
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
        assert_eq!(state.usage_5h_pct, Some(95.0));
        assert_eq!(state.usage_7d_pct, Some(30.0));

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
        assert_eq!(state.usage_5h_pct, Some(45.0));
        assert_eq!(state.usage_7d_pct, Some(12.5));
        assert!(!state.is_rate_limited);
    }

    #[test]
    fn test_update_task() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "old title", "old desc", TaskMode::Supervised)
            .unwrap();

        store
            .update_task(&task.id, "new title", "new desc", TaskMode::Autonomous)
            .unwrap();

        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.title, "new title");
        assert_eq!(t.description, "new desc");
        assert_eq!(t.mode, TaskMode::Autonomous);
    }

    #[test]
    fn test_delete_task() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "doomed", "", TaskMode::Supervised)
            .unwrap();

        store.delete_task(&task.id).unwrap();
        let tasks = store.list_tasks_for_project(&project.id).unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_task_sort_order() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();

        let t1 = store
            .create_task(&project.id, "first", "", TaskMode::Supervised)
            .unwrap();
        store
            .create_task(&project.id, "second", "", TaskMode::Supervised)
            .unwrap();
        let t3 = store
            .create_task(&project.id, "third", "", TaskMode::Supervised)
            .unwrap();

        // Default order: first, second, third
        let tasks = store.list_tasks_for_project(&project.id).unwrap();
        assert_eq!(tasks[0].title, "first");
        assert_eq!(tasks[1].title, "second");
        assert_eq!(tasks[2].title, "third");

        // Swap first and third
        store.swap_task_order(&t1.id, &t3.id).unwrap();
        let tasks = store.list_tasks_for_project(&project.id).unwrap();
        assert_eq!(tasks[0].title, "third");
        assert_eq!(tasks[2].title, "first");
    }

    #[test]
    fn test_create_and_list_subtasks() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "parent", "", TaskMode::Autonomous)
            .unwrap();

        let s1 = store
            .create_subtask(&task.id, "step 1", "do first")
            .unwrap();
        let s2 = store
            .create_subtask(&task.id, "step 2", "do second")
            .unwrap();

        assert_eq!(s1.title, "step 1");
        assert_eq!(s1.status, TaskStatus::Pending);

        let subtasks = store.list_subtasks_for_task(&task.id).unwrap();
        assert_eq!(subtasks.len(), 2);
        assert_eq!(subtasks[0].title, "step 1");
        assert_eq!(subtasks[1].title, "step 2");

        // Suppress unused variable warnings
        let _ = s2;
    }

    #[test]
    fn test_subtask_lifecycle() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "parent", "", TaskMode::Autonomous)
            .unwrap();
        let st = store.create_subtask(&task.id, "step", "do it").unwrap();

        store
            .update_subtask_status(&st.id, TaskStatus::InProgress)
            .unwrap();
        let st = store.get_subtask(&st.id).unwrap();
        assert_eq!(st.status, TaskStatus::InProgress);
        assert!(st.started_at.is_some());

        store
            .update_subtask_status(&st.id, TaskStatus::Done)
            .unwrap();
        let st = store.get_subtask(&st.id).unwrap();
        assert_eq!(st.status, TaskStatus::Done);
        assert!(st.completed_at.is_some());
    }

    #[test]
    fn test_next_pending_subtask() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "parent", "", TaskMode::Autonomous)
            .unwrap();

        assert!(store.next_pending_subtask(&task.id).unwrap().is_none());

        let s1 = store.create_subtask(&task.id, "step 1", "first").unwrap();
        store.create_subtask(&task.id, "step 2", "second").unwrap();

        let next = store.next_pending_subtask(&task.id).unwrap().unwrap();
        assert_eq!(next.id, s1.id);

        // Mark first done — next pending should be step 2
        store
            .update_subtask_status(&s1.id, TaskStatus::Done)
            .unwrap();
        let next = store.next_pending_subtask(&task.id).unwrap().unwrap();
        assert_eq!(next.title, "step 2");
    }

    #[test]
    fn test_subtask_count() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "parent", "", TaskMode::Autonomous)
            .unwrap();

        let (total, done) = store.subtask_count(&task.id).unwrap();
        assert_eq!(total, 0);
        assert_eq!(done, 0);

        let s1 = store.create_subtask(&task.id, "s1", "").unwrap();
        store.create_subtask(&task.id, "s2", "").unwrap();

        let (total, done) = store.subtask_count(&task.id).unwrap();
        assert_eq!(total, 2);
        assert_eq!(done, 0);

        store
            .update_subtask_status(&s1.id, TaskStatus::Done)
            .unwrap();
        let (total, done) = store.subtask_count(&task.id).unwrap();
        assert_eq!(total, 2);
        assert_eq!(done, 1);
    }

    #[test]
    fn test_in_progress_task_for_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();

        // No tasks assigned — should return None
        assert!(
            store
                .in_progress_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // Pending task assigned — should still return None
        let t1 = store
            .create_task(&project.id, "pending", "", TaskMode::Supervised)
            .unwrap();
        store.assign_task_to_session(&t1.id, &session.id).unwrap();
        assert!(
            store
                .in_progress_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // In-progress task — should return it
        store
            .update_task_status(&t1.id, TaskStatus::InProgress)
            .unwrap();
        let found = store
            .in_progress_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, t1.id);

        // Mark done — should return None again
        store.update_task_status(&t1.id, TaskStatus::Done).unwrap();
        assert!(
            store
                .in_progress_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_in_review_task_for_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();

        // No tasks — should return None
        assert!(
            store
                .in_review_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // Pending task — should return None
        let t1 = store
            .create_task(&project.id, "task1", "", TaskMode::Supervised)
            .unwrap();
        store.assign_task_to_session(&t1.id, &session.id).unwrap();
        assert!(
            store
                .in_review_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // In-review task — should return it
        store
            .update_task_status(&t1.id, TaskStatus::InReview)
            .unwrap();
        let found = store
            .in_review_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, t1.id);

        // Mark done — should return None again
        store.update_task_status(&t1.id, TaskStatus::Done).unwrap();
        assert!(
            store
                .in_review_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_delete_subtask() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let task = store
            .create_task(&project.id, "parent", "", TaskMode::Autonomous)
            .unwrap();
        let st = store.create_subtask(&task.id, "doomed", "bye").unwrap();

        store.delete_subtask(&st.id).unwrap();
        assert!(store.list_subtasks_for_task(&task.id).unwrap().is_empty());
    }

    #[test]
    fn test_update_session_progress() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj").unwrap();
        let session = store
            .create_session(&project.id, "branch", "/tmp/wt", "tab")
            .unwrap();

        // Initially empty
        assert!(session.claude_progress.is_empty());

        // Update with progress items
        let progress = vec![
            ClaudeProgressItem {
                subject: "Step 1".into(),
                status: "completed".into(),
            },
            ClaudeProgressItem {
                subject: "Step 2".into(),
                status: "in_progress".into(),
            },
            ClaudeProgressItem {
                subject: "Step 3".into(),
                status: "pending".into(),
            },
        ];
        store
            .update_session_progress(&session.id, &progress)
            .unwrap();

        let s = store.get_session(&session.id).unwrap();
        assert_eq!(s.claude_progress.len(), 3);
        assert_eq!(s.claude_progress[0].subject, "Step 1");
        assert_eq!(s.claude_progress[0].status, "completed");
        assert_eq!(s.claude_progress[1].status, "in_progress");
        assert_eq!(s.claude_progress[2].status, "pending");

        // Update again (replace)
        let progress2 = vec![ClaudeProgressItem {
            subject: "Step 1".into(),
            status: "completed".into(),
        }];
        store
            .update_session_progress(&session.id, &progress2)
            .unwrap();
        let s = store.get_session(&session.id).unwrap();
        assert_eq!(s.claude_progress.len(), 1);
    }
}
