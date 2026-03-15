//! Task CRUD operations and queries.

use anyhow::{Context, Result, bail};
use rusqlite::params;
use tracing::warn;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::{CiStatus, PushMode, Task, TaskMode, TaskStatus};

use super::optional;

/// Column list for all queries that use `row_to_task`.
/// Keep in sync with the field mapping in `row_to_task` below.
/// `pub(super)` because `stats.rs` also queries tasks via `row_to_task`.
pub(super) const TASK_COLUMNS: &str = "\
    id, project_id, title, description, status, mode, session_id, \
    created_at, updated_at, started_at, completed_at, \
    input_tokens, output_tokens, sort_order, pr_url, \
    branch, push_mode, ci_status, review_loop, base";

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
        base: Option<&str>,
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
                "INSERT INTO tasks (id, project_id, title, description, mode, sort_order, branch, base, push_mode, review_loop) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![id, project_id, title, description, mode.as_str(), max_order + 1, branch, base, push_mode.as_str(), review_loop],
            )
            .with_context(|| format!("failed to create task '{title}'"))?;
        self.get_task(&id)
    }

    pub fn get_task(&self, id: &str) -> Result<Task> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
        let task = self
            .conn
            .query_row(&sql, params![id], Self::row_to_task)
            .with_context(|| format!("failed to fetch task '{id}'"))?;
        Ok(task)
    }

    pub fn list_tasks_for_project(&self, project_id: &str) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks \
             WHERE project_id = ?1 \
             ORDER BY sort_order, created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
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
        base: Option<&str>,
        push_mode: PushMode,
        review_loop: bool,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE tasks SET title = ?1, description = ?2, mode = ?3, updated_at = ?4, branch = ?5, base = ?6, push_mode = ?7, review_loop = ?8 WHERE id = ?9",
            params![title, description, mode.as_str(), now, branch, base, push_mode.as_str(), review_loop, id],
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
            base: row.get(19)?,
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
        if !self.try_update_task_status(id, status)? {
            let current_status_str: String = self
                .conn
                .query_row(
                    "SELECT status FROM tasks WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .context("task not found for status update")?;
            bail!("invalid task status transition: {current_status_str} -> {status} (task {id})");
        }
        Ok(())
    }

    /// Attempt a task status transition, returning `Ok(true)` if the transition
    /// was applied or `Ok(false)` if it was invalid. Returns `Err` only on
    /// database errors. Use this for background/polling code where stale state
    /// makes invalid transitions expected rather than exceptional.
    pub fn try_update_task_status(&self, id: &str, status: TaskStatus) -> Result<bool> {
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
            return Ok(false);
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
        Ok(true)
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
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks \
             WHERE session_id = ?1 AND status = 'working' \
             LIMIT 1"
        );
        optional(
            self.conn
                .query_row(&sql, params![session_id], Self::row_to_task),
        )
    }

    /// Find the in-review, conflict, or ci-failed task assigned to a session (if any).
    pub fn in_review_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks \
             WHERE session_id = ?1 AND status IN ('in_review', 'conflict', 'ci_failed') \
             LIMIT 1"
        );
        optional(
            self.conn
                .query_row(&sql, params![session_id], Self::row_to_task),
        )
    }

    /// Find the interrupted task assigned to a session (if any).
    ///
    /// When claustre restarts, active sessions are marked `interrupted`. If the
    /// underlying Claude process is still running (session-host survived), hooks
    /// will keep firing. This query lets `session-update` locate the task even
    /// though it's no longer `working`.
    pub fn interrupted_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks \
             WHERE session_id = ?1 AND status = 'interrupted' \
             LIMIT 1"
        );
        optional(
            self.conn
                .query_row(&sql, params![session_id], Self::row_to_task),
        )
    }

    pub fn next_pending_task_for_session(&self, session_id: &str) -> Result<Option<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks \
             WHERE session_id = ?1 AND status = 'pending' AND mode = 'autonomous' \
             ORDER BY sort_order, created_at \
             LIMIT 1"
        );
        optional(
            self.conn
                .query_row(&sql, params![session_id], Self::row_to_task),
        )
    }

    /// Find all pending autonomous tasks not assigned to any session.
    /// Used on startup to auto-launch tasks that were pending when claustre was closed.
    pub fn pending_autonomous_tasks_unassigned(&self) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM tasks \
             WHERE status = 'pending' AND mode = 'autonomous' AND session_id IS NULL \
             ORDER BY sort_order, created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let tasks = stmt
            .query_map([], Self::row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tasks)
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{PushMode, Store, TaskMode, TaskStatus};

    fn setup(store: &Store) -> String {
        store.create_project("p", "/tmp/p", "main").unwrap().id
    }

    // ── try_update_task_status ──

    #[test]
    fn try_update_valid_transition_returns_true() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        let ok = store
            .try_update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        assert!(ok);
        assert_eq!(
            store.get_task(&task.id).unwrap().status,
            TaskStatus::Working
        );
    }

    #[test]
    fn try_update_invalid_transition_returns_false() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        // Pending → Done is invalid
        let ok = store
            .try_update_task_status(&task.id, TaskStatus::Done)
            .unwrap();
        assert!(!ok);
        assert_eq!(
            store.get_task(&task.id).unwrap().status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn update_task_status_errors_on_invalid_transition() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        let result = store.update_task_status(&task.id, TaskStatus::Done);
        assert!(result.is_err());
    }

    #[test]
    fn working_sets_started_at_once() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        assert!(store.get_task(&task.id).unwrap().started_at.is_none());

        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        let started = store.get_task(&task.id).unwrap().started_at.unwrap();

        // Going back to pending and then working again should NOT overwrite started_at
        store
            .update_task_status(&task.id, TaskStatus::Pending)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        assert_eq!(
            store.get_task(&task.id).unwrap().started_at.as_deref(),
            Some(started.as_str())
        );
    }

    #[test]
    fn done_sets_completed_at() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Done)
            .unwrap();

        let task = store.get_task(&task.id).unwrap();
        assert!(task.completed_at.is_some());
    }

    // ── Task finders ──

    #[test]
    fn working_task_for_session_finds_correct_task() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let session = store
            .create_session(&pid, "feat", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();

        let found = store
            .working_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
    }

    #[test]
    fn working_task_for_session_returns_none_when_pending() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let session = store
            .create_session(&pid, "feat", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        // Task is still pending — should not be found

        assert!(
            store
                .working_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn in_review_task_for_session_finds_in_review() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let session = store
            .create_session(&pid, "feat", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::InReview)
            .unwrap();

        let found = store
            .in_review_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
    }

    #[test]
    fn in_review_task_for_session_finds_conflict() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let session = store
            .create_session(&pid, "feat", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::InReview)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Conflict)
            .unwrap();

        let found = store
            .in_review_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
    }

    #[test]
    fn interrupted_task_for_session_finds_interrupted() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let session = store
            .create_session(&pid, "feat", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Interrupted)
            .unwrap();

        let found = store
            .interrupted_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, task.id);
    }

    // ── Swap order ──

    #[test]
    fn swap_task_order_exchanges_sort_orders() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let t1 = store
            .create_task(
                &pid,
                "first",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        let t2 = store
            .create_task(
                &pid,
                "second",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        let order1_before = t1.sort_order;
        let order2_before = t2.sort_order;

        store.swap_task_order(&t1.id, &t2.id).unwrap();

        let t1_after = store.get_task(&t1.id).unwrap();
        let t2_after = store.get_task(&t2.id).unwrap();
        assert_eq!(t1_after.sort_order, order2_before);
        assert_eq!(t2_after.sort_order, order1_before);
    }

    // ── Pending autonomous unassigned ──

    #[test]
    fn pending_autonomous_unassigned_only_returns_matching() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);

        // Autonomous pending unassigned — should be found
        store
            .create_task(
                &pid,
                "auto",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        // Supervised pending — should NOT be found
        store
            .create_task(
                &pid,
                "sup",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        // Autonomous but assigned — should NOT be found
        let session = store
            .create_session(&pid, "feat", "/tmp/wt", "tab")
            .unwrap();
        let assigned = store
            .create_task(
                &pid,
                "assigned",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .assign_task_to_session(&assigned.id, &session.id)
            .unwrap();

        let results = store.pending_autonomous_tasks_unassigned().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "auto");
    }

    // ── Next pending for session ──

    #[test]
    fn next_pending_for_session_returns_autonomous_only() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let session = store
            .create_session(&pid, "feat", "/tmp/wt", "tab")
            .unwrap();

        // Create supervised task assigned to session — should NOT be returned
        let sup = store
            .create_task(
                &pid,
                "supervised",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&sup.id, &session.id).unwrap();

        // Create autonomous pending task assigned to session — should be returned
        let auto = store
            .create_task(
                &pid,
                "auto",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&auto.id, &session.id).unwrap();

        let next = store
            .next_pending_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(next.id, auto.id);
    }

    // ── Update helpers ──

    #[test]
    fn update_task_pr_url_and_ci_status() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        store
            .update_task_pr_url(&task.id, "https://github.com/pr/1")
            .unwrap();
        store
            .update_task_ci_status(&task.id, Some(crate::store::CiStatus::Running))
            .unwrap();

        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.pr_url.as_deref(), Some("https://github.com/pr/1"));
        assert_eq!(t.ci_status, Some(crate::store::CiStatus::Running));

        // Clear CI status
        store.update_task_ci_status(&task.id, None).unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert!(t.ci_status.is_none());
    }

    #[test]
    fn set_task_usage_replaces_values() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        store.set_task_usage(&task.id, 5000, 3000).unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.input_tokens, 5000);
        assert_eq!(t.output_tokens, 3000);

        // Replace with new values (not additive)
        store.set_task_usage(&task.id, 8000, 4000).unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.input_tokens, 8000);
        assert_eq!(t.output_tokens, 4000);
    }

    #[test]
    fn unassign_task_from_session() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let session = store
            .create_session(&pid, "feat", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &pid,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        store.assign_task_to_session(&task.id, &session.id).unwrap();
        assert!(store.get_task(&task.id).unwrap().session_id.is_some());

        store.unassign_task_from_session(&task.id).unwrap();
        assert!(store.get_task(&task.id).unwrap().session_id.is_none());
    }

    #[test]
    fn update_task_preserves_fields() {
        let store = Store::open_in_memory().unwrap();
        let pid = setup(&store);
        let task = store
            .create_task(
                &pid,
                "original",
                "desc",
                TaskMode::Supervised,
                Some("feat/x"),
                Some("develop"),
                PushMode::Pr,
                false,
            )
            .unwrap();

        store
            .update_task(
                &task.id,
                "updated",
                "new desc",
                TaskMode::Autonomous,
                Some("feat/y"),
                Some("main"),
                PushMode::Push,
                true,
            )
            .unwrap();

        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.title, "updated");
        assert_eq!(t.description, "new desc");
        assert_eq!(t.mode, TaskMode::Autonomous);
        assert_eq!(t.branch.as_deref(), Some("feat/y"));
        assert_eq!(t.base.as_deref(), Some("main"));
        assert_eq!(t.push_mode, PushMode::Push);
        assert!(t.review_loop);
    }
}
