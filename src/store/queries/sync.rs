//! Upsert operations used by the sync module to import state from other machines.

use anyhow::Result;
use rusqlite::params;

use crate::store::Store;
use crate::sync::{SyncSubtask, SyncTask};

impl Store {
    /// Insert or update a task from sync data.
    ///
    /// On insert, uses the given `project_id` (the local project's ID) and sets
    /// `session_id = NULL` (sessions are machine-specific).
    /// On conflict (same task UUID), updates all portable fields while preserving
    /// the local `session_id`.
    pub fn upsert_task_from_sync(&self, project_id: &str, task: &SyncTask) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tasks (
                id, project_id, title, description, status, mode, session_id,
                created_at, updated_at, started_at, completed_at,
                input_tokens, output_tokens, sort_order, pr_url,
                branch, push_mode, ci_status, review_loop, base
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)
            ON CONFLICT(id) DO UPDATE SET
                project_id = excluded.project_id,
                title = excluded.title,
                description = excluded.description,
                status = excluded.status,
                mode = excluded.mode,
                updated_at = excluded.updated_at,
                started_at = excluded.started_at,
                completed_at = excluded.completed_at,
                input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens,
                sort_order = excluded.sort_order,
                pr_url = excluded.pr_url,
                branch = excluded.branch,
                push_mode = excluded.push_mode,
                ci_status = excluded.ci_status,
                review_loop = excluded.review_loop,
                base = excluded.base",
            params![
                task.id,
                project_id,
                task.title,
                task.description,
                task.status,
                task.mode,
                task.created_at,
                task.updated_at,
                task.started_at,
                task.completed_at,
                task.input_tokens,
                task.output_tokens,
                task.sort_order,
                task.pr_url,
                task.branch,
                task.push_mode,
                task.ci_status,
                task.review_loop,
                task.base,
            ],
        )?;
        Ok(())
    }

    /// Insert or update a subtask from sync data.
    pub fn upsert_subtask_from_sync(&self, task_id: &str, subtask: &SyncSubtask) -> Result<()> {
        self.conn.execute(
            "INSERT INTO subtasks (
                id, task_id, title, description, status, sort_order,
                created_at, started_at, completed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                description = excluded.description,
                status = excluded.status,
                sort_order = excluded.sort_order,
                started_at = excluded.started_at,
                completed_at = excluded.completed_at",
            params![
                subtask.id,
                task_id,
                subtask.title,
                subtask.description,
                subtask.status,
                subtask.sort_order,
                subtask.created_at,
                subtask.started_at,
                subtask.completed_at,
            ],
        )?;
        Ok(())
    }
}
