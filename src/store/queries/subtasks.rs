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

#[cfg(test)]
mod tests {
    use crate::store::{PushMode, Store, TaskMode, TaskStatus};

    fn make_task(store: &Store) -> (String, String) {
        let project = store.create_project("p", "/tmp/p", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "task",
                "desc",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        (project.id, task.id)
    }

    #[test]
    fn subtask_count_empty() {
        let store = Store::open_in_memory().unwrap();
        let (_, task_id) = make_task(&store);

        let (total, done) = store.subtask_count(&task_id).unwrap();
        assert_eq!(total, 0);
        assert_eq!(done, 0);
    }

    #[test]
    fn subtask_count_with_mixed_statuses() {
        let store = Store::open_in_memory().unwrap();
        let (_, task_id) = make_task(&store);

        let s1 = store.create_subtask(&task_id, "step 1", "").unwrap();
        let s2 = store.create_subtask(&task_id, "step 2", "").unwrap();
        let _s3 = store.create_subtask(&task_id, "step 3", "").unwrap();

        store
            .update_subtask_status(&s1.id, TaskStatus::Working)
            .unwrap();
        store
            .update_subtask_status(&s1.id, TaskStatus::Done)
            .unwrap();
        store
            .update_subtask_status(&s2.id, TaskStatus::Working)
            .unwrap();

        let (total, done) = store.subtask_count(&task_id).unwrap();
        assert_eq!(total, 3);
        assert_eq!(done, 1);
    }

    #[test]
    fn next_pending_subtask_returns_lowest_sort_order() {
        let store = Store::open_in_memory().unwrap();
        let (_, task_id) = make_task(&store);

        let s1 = store.create_subtask(&task_id, "step 1", "").unwrap();
        let s2 = store.create_subtask(&task_id, "step 2", "").unwrap();

        // s1 has sort_order 1, s2 has sort_order 2
        let next = store.next_pending_subtask(&task_id).unwrap().unwrap();
        assert_eq!(next.id, s1.id);

        // Complete s1 — next should be s2
        store
            .update_subtask_status(&s1.id, TaskStatus::Working)
            .unwrap();
        store
            .update_subtask_status(&s1.id, TaskStatus::Done)
            .unwrap();

        let next = store.next_pending_subtask(&task_id).unwrap().unwrap();
        assert_eq!(next.id, s2.id);
    }

    #[test]
    fn next_pending_subtask_returns_none_when_all_done() {
        let store = Store::open_in_memory().unwrap();
        let (_, task_id) = make_task(&store);

        let s1 = store.create_subtask(&task_id, "step", "").unwrap();
        store
            .update_subtask_status(&s1.id, TaskStatus::Working)
            .unwrap();
        store
            .update_subtask_status(&s1.id, TaskStatus::Done)
            .unwrap();

        assert!(store.next_pending_subtask(&task_id).unwrap().is_none());
    }

    #[test]
    fn subtask_sort_order_auto_increments() {
        let store = Store::open_in_memory().unwrap();
        let (_, task_id) = make_task(&store);

        let s1 = store.create_subtask(&task_id, "step 1", "").unwrap();
        let s2 = store.create_subtask(&task_id, "step 2", "").unwrap();
        let s3 = store.create_subtask(&task_id, "step 3", "").unwrap();

        assert_eq!(s1.sort_order, 1);
        assert_eq!(s2.sort_order, 2);
        assert_eq!(s3.sort_order, 3);
    }

    #[test]
    fn update_subtask_status_sets_started_at_once() {
        let store = Store::open_in_memory().unwrap();
        let (_, task_id) = make_task(&store);
        let s = store.create_subtask(&task_id, "step", "").unwrap();

        assert!(s.started_at.is_none());

        store
            .update_subtask_status(&s.id, TaskStatus::Working)
            .unwrap();
        let s = store.get_subtask(&s.id).unwrap();
        let first_started = s.started_at.clone().unwrap();

        // Transition away and back — started_at should NOT change
        store
            .update_subtask_status(&s.id, TaskStatus::Done)
            .unwrap();
        // Re-read to verify completed_at was set
        let s = store.get_subtask(&s.id).unwrap();
        assert!(s.completed_at.is_some());
        assert_eq!(s.started_at.as_deref(), Some(first_started.as_str()));
    }

    #[test]
    fn delete_subtask_removes_it() {
        let store = Store::open_in_memory().unwrap();
        let (_, task_id) = make_task(&store);
        let s = store.create_subtask(&task_id, "step", "").unwrap();

        store.delete_subtask(&s.id).unwrap();
        let subtasks = store.list_subtasks_for_task(&task_id).unwrap();
        assert!(subtasks.is_empty());
    }

    #[test]
    fn list_subtasks_ordered_by_sort_order() {
        let store = Store::open_in_memory().unwrap();
        let (_, task_id) = make_task(&store);

        store.create_subtask(&task_id, "third", "").unwrap();
        store.create_subtask(&task_id, "first", "").unwrap();
        // sort_order: third=1, first=2

        let subtasks = store.list_subtasks_for_task(&task_id).unwrap();
        assert_eq!(subtasks[0].title, "third");
        assert_eq!(subtasks[1].title, "first");
    }
}
