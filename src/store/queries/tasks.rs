//! Task CRUD operations and queries.

use anyhow::{Context, Result, bail};
use rusqlite::params;
use tracing::warn;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::{CiStatus, PushMode, Task, TaskMode, TaskStatus};

use super::optional;

impl Store {
    #[expect(
        clippy::too_many_arguments,
        reason = "task creation requires all fields"
    )]
    pub fn create_task(
        &self,
        project_id: &str,
        title: &str,
        description: &str,
        mode: TaskMode,
        branch: Option<&str>,
        push_mode: PushMode,
        review_loop: bool,
    ) -> Result<Task> {
        let id = Uuid::new_v4().to_string();
        let max_order: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) FROM tasks WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )?;
        self.conn
            .execute(
                "INSERT INTO tasks (id, project_id, title, description, mode, sort_order, branch, push_mode, review_loop) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![id, project_id, title, description, mode.as_str(), max_order + 1, branch, push_mode.as_str(), review_loop],
            )
            .with_context(|| format!("failed to create task '{title}'"))?;
        self.get_task(&id)
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        let task = self
            .conn
            .query_row(
                "SELECT id, project_id, title, description, status, mode, session_id,
                        created_at, updated_at, started_at, completed_at,
                        input_tokens, output_tokens, sort_order, pr_url,
                        branch, push_mode, ci_status, review_loop
                 FROM tasks WHERE id = ?1",
                params![id],
                Self::row_to_task,
            )
            .with_context(|| format!("failed to fetch task '{id}'"))?;
        Ok(task)
    }

    pub fn list_tasks_for_project(&self, project_id: &str) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, sort_order, pr_url,
                    branch, push_mode, ci_status, review_loop
             FROM tasks WHERE project_id = ?1
             ORDER BY sort_order, created_at",
        )?;
        let tasks = stmt
            .query_map(params![project_id], Self::row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tasks)
    }

    #[expect(clippy::too_many_arguments, reason = "task update requires all fields")]
    pub fn update_task(
        &self,
        id: &str,
        title: &str,
        description: &str,
        mode: TaskMode,
        branch: Option<&str>,
        push_mode: PushMode,
        review_loop: bool,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET title = ?1, description = ?2, mode = ?3, updated_at = ?4, branch = ?5, push_mode = ?6, review_loop = ?7 WHERE id = ?8",
            params![title, description, mode.as_str(), now, branch, push_mode.as_str(), review_loop, id],
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
        self.in_transaction(|| {
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
        })
        .context("failed to swap task order")
    }

    pub(crate) fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
        let status_str: String = row.get(4)?;
        let mode_str: String = row.get(5)?;
        let push_mode_str: String = row.get(16)?;
        let ci_status_str: Option<String> = row.get(17)?;
        let id: String = row.get(0)?;
        Ok(Task {
            status: status_str.parse().unwrap_or_else(|_| {
                warn!(task_id = %id, raw = %status_str, "unknown task status in DB, defaulting to Pending");
                TaskStatus::Pending
            }),
            mode: mode_str.parse().unwrap_or_else(|_| {
                warn!(task_id = %id, raw = %mode_str, "unknown task mode in DB, defaulting to Supervised");
                TaskMode::Supervised
            }),
            push_mode: push_mode_str.parse().unwrap_or_else(|_| {
                warn!(task_id = %id, raw = %push_mode_str, "unknown push mode in DB, defaulting to Pr");
                PushMode::Pr
            }),
            ci_status: ci_status_str.and_then(|s| {
                s.parse::<CiStatus>().map_err(|_| {
                    warn!(task_id = %id, raw = %s, "unknown CI status in DB, defaulting to None");
                }).ok()
            }),
            id,
            project_id: row.get(1)?,
            title: row.get(2)?,
            description: row.get(3)?,
            session_id: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
            started_at: row.get(9)?,
            completed_at: row.get(10)?,
            input_tokens: row.get(11)?,
            output_tokens: row.get(12)?,
            sort_order: row.get(13)?,
            pr_url: row.get(14)?,
            branch: row.get(15)?,
            review_loop: row.get::<_, i64>(18).unwrap_or(0) != 0,
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

    pub fn update_task_ci_status(&self, id: &str, ci_status: Option<CiStatus>) -> Result<()> {
        let val = ci_status.map(|s| s.as_str().to_string());
        self.conn.execute(
            "UPDATE tasks SET ci_status = ?1 WHERE id = ?2",
            params![val, id],
        )?;
        Ok(())
    }

    pub fn update_task_status(&self, id: &str, status: TaskStatus) -> Result<()> {
        // Validate the transition against the state machine
        let current_status_str: String = self
            .conn
            .query_row(
                "SELECT status FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .context("task not found for status update")?;
        let current_status: TaskStatus = current_status_str.parse().unwrap_or(TaskStatus::Pending);

        if !current_status.can_transition_to(status) {
            bail!("invalid task status transition: {current_status} -> {status} (task {id})");
        }

        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET status = ?1, updated_at = ?2 WHERE id = ?3",
            params![status.as_str(), now, id],
        )?;

        match status {
            TaskStatus::Working => {
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

    /// Remove the session assignment from a task so it can be re-launched.
    pub fn unassign_task_from_session(&self, task_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET session_id = NULL WHERE id = ?1",
            params![task_id],
        )?;
        Ok(())
    }

    /// Set absolute token usage on a task (replaces, not additive).
    /// Used by the stop hook which reports cumulative totals.
    pub fn set_task_usage(&self, id: &str, input_tokens: i64, output_tokens: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET input_tokens = ?1, output_tokens = ?2 WHERE id = ?3",
            params![input_tokens, output_tokens, id],
        )?;
        Ok(())
    }

    /// Find the working task assigned to a session (if any).
    pub fn working_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        optional(self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, sort_order, pr_url,
                    branch, push_mode, ci_status, review_loop
             FROM tasks
             WHERE session_id = ?1 AND status = 'working'
             LIMIT 1",
            params![session_id],
            Self::row_to_task,
        ))
    }

    /// Find the in-review, conflict, or ci-failed task assigned to a session (if any).
    pub fn in_review_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        optional(self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, sort_order, pr_url,
                    branch, push_mode, ci_status, review_loop
             FROM tasks
             WHERE session_id = ?1 AND status IN ('in_review', 'conflict', 'ci_failed')
             LIMIT 1",
            params![session_id],
            Self::row_to_task,
        ))
    }

    /// Find the interrupted task assigned to a session (if any).
    ///
    /// When claustre restarts, active sessions are marked `interrupted`. If the
    /// underlying Claude process is still running (session-host survived), hooks
    /// will keep firing. This query lets `session-update` locate the task even
    /// though it's no longer `working`.
    pub fn interrupted_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        optional(self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, sort_order, pr_url,
                    branch, push_mode, ci_status, review_loop
             FROM tasks
             WHERE session_id = ?1 AND status = 'interrupted'
             LIMIT 1",
            params![session_id],
            Self::row_to_task,
        ))
    }

    pub fn next_pending_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        optional(self.conn.query_row(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, sort_order, pr_url,
                    branch, push_mode, ci_status, review_loop
             FROM tasks
             WHERE session_id = ?1 AND status = 'pending' AND mode = 'autonomous'
             ORDER BY sort_order, created_at
             LIMIT 1",
            params![session_id],
            Self::row_to_task,
        ))
    }

    /// Find all pending autonomous tasks not assigned to any session.
    /// Used on startup to auto-launch tasks that were pending when claustre was closed.
    pub fn pending_autonomous_tasks_unassigned(&self) -> Result<Vec<Task>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, title, description, status, mode, session_id,
                    created_at, updated_at, started_at, completed_at,
                    input_tokens, output_tokens, sort_order, pr_url,
                    branch, push_mode, ci_status, review_loop
             FROM tasks
             WHERE status = 'pending' AND mode = 'autonomous' AND session_id IS NULL
             ORDER BY sort_order, created_at",
        )?;
        let tasks = stmt
            .query_map([], Self::row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tasks)
    }
}
