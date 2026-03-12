//! Subtask CRUD operations.

use anyhow::{Context, Result};
use rusqlite::params;
use tracing::warn;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::{Subtask, TaskStatus};

use super::optional;

/// Column list for all queries that use `row_to_subtask`.
/// Keep in sync with the field mapping in `row_to_subtask` below.
const SUBTASK_COLUMNS: &str = "\
    id, task_id, title, description, status, sort_order, \
    created_at, started_at, completed_at";

impl Store {
    pub fn create_subtask(&self, task_id: &str, title: &str, description: &str) -> Result<Subtask> {
        let id = Uuid::new_v4().to_string();
        let max_order: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(sort_order), 0) FROM subtasks WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )?;
        self.conn
            .execute(
                "INSERT INTO subtasks (id, task_id, title, description, sort_order) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, task_id, title, description, max_order + 1],
            )
            .with_context(|| format!("failed to create subtask '{title}'"))?;
        self.get_subtask(&id)
    }

    pub fn get_subtask(&self, id: &str) -> Result<Subtask> {
        let sql = format!("SELECT {SUBTASK_COLUMNS} FROM subtasks WHERE id = ?1");
        let subtask = self
            .conn
            .query_row(&sql, params![id], Self::row_to_subtask)
            .with_context(|| format!("failed to fetch subtask '{id}'"))?;
        Ok(subtask)
    }

    pub fn list_subtasks_for_task(&self, task_id: &str) -> Result<Vec<Subtask>> {
        let sql = format!(
            "SELECT {SUBTASK_COLUMNS} FROM subtasks \
             WHERE task_id = ?1 \
             ORDER BY sort_order, created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
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
            TaskStatus::Working => {
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

    pub fn delete_subtask(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM subtasks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn next_pending_subtask(&self, task_id: &str) -> Result<Option<Subtask>> {
        let sql = format!(
            "SELECT {SUBTASK_COLUMNS} FROM subtasks \
             WHERE task_id = ?1 AND status = 'pending' \
             ORDER BY sort_order, created_at \
             LIMIT 1"
        );
        optional(
            self.conn
                .query_row(&sql, params![task_id], Self::row_to_subtask),
        )
    }

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
        let id: String = row.get(0)?;
        Ok(Subtask {
            status: status_str.parse().unwrap_or_else(|_| {
                warn!(subtask_id = %id, raw = %status_str, "unknown subtask status in DB, defaulting to Pending");
                TaskStatus::Pending
            }),
            id,
            task_id: row.get(1)?,
            title: row.get(2)?,
            description: row.get(3)?,
            sort_order: row.get(5)?,
            created_at: row.get(6)?,
            started_at: row.get(7)?,
            completed_at: row.get(8)?,
        })
    }
}
