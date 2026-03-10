//! CRUD operations and aggregate queries on the claustre database.
//!
//! All database access goes through `impl Store` methods defined here.
//! Uses `anyhow::Context` for actionable error messages on key operations.

mod external_sessions;
mod projects;
mod rate_limits;
mod sessions;
mod stats;
mod subtasks;
mod tasks;

pub use stats::ProjectStats;

use anyhow::Result;

use super::Store;

/// Convert a rusqlite Result into an Option, treating `QueryReturnedNoRows` as None.
pub(crate) fn optional<T>(result: rusqlite::Result<T>) -> Result<Option<T>> {
    match result {
        Ok(val) => Ok(Some(val)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

impl Store {
    /// Run a closure inside a `SQLite` transaction. Commits on success, rolls back on error.
    fn in_transaction(&self, f: impl FnOnce() -> Result<()>) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        match f() {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(e) => {
                if let Err(rollback_err) = self.conn.execute_batch("ROLLBACK") {
                    return Err(e.context(format!("rollback also failed: {rollback_err}")));
                }
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::Store;
    use super::super::models::{
        ClaudeProgressItem, ClaudeStatus, ExternalSession, PushMode, TaskMode, TaskStatus,
    };

    #[test]
    fn test_create_and_get_project() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("test-proj", "/tmp/repo", "main")
            .unwrap();
        assert_eq!(project.name, "test-proj");
        assert_eq!(project.repo_path, "/tmp/repo");

        let fetched = store.get_project(&project.id).unwrap();
        assert_eq!(fetched.id, project.id);
        assert_eq!(fetched.name, "test-proj");
    }

    #[test]
    fn test_list_projects() {
        let store = Store::open_in_memory().unwrap();
        store.create_project("beta", "/tmp/beta", "main").unwrap();
        store.create_project("alpha", "/tmp/alpha", "main").unwrap();

        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 2);
        // Ordered by name
        assert_eq!(projects[0].name, "alpha");
        assert_eq!(projects[1].name, "beta");
    }

    #[test]
    fn test_delete_project() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("doomed", "/tmp/doomed", "main")
            .unwrap();
        store
            .create_task(
                &project.id,
                "task1",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        store.delete_project(&project.id).unwrap();

        let projects = store.list_projects().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn test_create_task() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "do stuff",
                "details",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        assert_eq!(task.title, "do stuff");
        assert_eq!(task.description, "details");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.mode, TaskMode::Autonomous);
    }

    #[test]
    fn test_task_lifecycle() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "lifecycle",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        // pending -> working
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.status, TaskStatus::Working);
        assert!(t.started_at.is_some());

        // working -> in_review
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
        let p1 = store.create_project("p1", "/tmp/p1", "main").unwrap();
        let p2 = store.create_project("p2", "/tmp/p2", "main").unwrap();

        store
            .create_task(
                &p1.id,
                "t1",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &p1.id,
                "t2",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &p2.id,
                "t3",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        let tasks = store.list_tasks_for_project(&p1.id).unwrap();
        assert_eq!(tasks.len(), 2);

        let tasks = store.list_tasks_for_project(&p2.id).unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn test_create_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let session = store
            .create_session(&project.id, "feat-branch", "/tmp/wt", "tab-1")
            .unwrap();

        assert_eq!(session.branch_name, "feat-branch");
        assert_eq!(session.worktree_path, "/tmp/wt");
        assert_eq!(session.tab_label, "tab-1");
        assert_eq!(session.claude_status, ClaudeStatus::Idle);
        assert!(session.closed_at.is_none());
    }

    #[test]
    fn test_update_session_status() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();

        store
            .create_task(
                &project.id,
                "t1",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &project.id,
                "t2",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
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
    fn test_project_stats_empty_project() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("empty", "/tmp/empty", "main").unwrap();

        let stats = store.project_stats(&project.id).unwrap();
        assert_eq!(stats.total_tasks, 0);
        assert_eq!(stats.completed_tasks, 0);
        assert_eq!(stats.total_sessions, 0);
        assert_eq!(stats.total_input_tokens, 0);
        assert_eq!(stats.total_output_tokens, 0);
        assert_eq!(stats.total_time_seconds, 0);
    }

    #[test]
    fn test_next_pending_task_for_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();

        // Supervised task should not be returned
        let t1 = store
            .create_task(
                &project.id,
                "supervised",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
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
            .create_task(
                &project.id,
                "auto",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "old title",
                "old desc",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        store
            .update_task(
                &task.id,
                "new title",
                "new desc",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.title, "new title");
        assert_eq!(t.description, "new desc");
        assert_eq!(t.mode, TaskMode::Autonomous);
    }

    #[test]
    fn test_delete_task() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "doomed",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        store.delete_task(&task.id).unwrap();
        let tasks = store.list_tasks_for_project(&project.id).unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_task_sort_order() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();

        let t1 = store
            .create_task(
                &project.id,
                "first",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_task(
                &project.id,
                "second",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        let t3 = store
            .create_task(
                &project.id,
                "third",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "parent",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "parent",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        let st = store.create_subtask(&task.id, "step", "do it").unwrap();

        store
            .update_subtask_status(&st.id, TaskStatus::Working)
            .unwrap();
        let st = store.get_subtask(&st.id).unwrap();
        assert_eq!(st.status, TaskStatus::Working);
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "parent",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "parent",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
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
    fn test_working_task_for_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();

        // No tasks assigned — should return None
        assert!(
            store
                .working_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // Pending task assigned — should still return None
        let t1 = store
            .create_task(
                &project.id,
                "pending",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&t1.id, &session.id).unwrap();
        assert!(
            store
                .working_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // Working task — should return it
        store
            .update_task_status(&t1.id, TaskStatus::Working)
            .unwrap();
        let found = store
            .working_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, t1.id);

        // Mark done — should return None again
        store.update_task_status(&t1.id, TaskStatus::Done).unwrap();
        assert!(
            store
                .working_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_in_review_task_for_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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
            .create_task(
                &project.id,
                "task1",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&t1.id, &session.id).unwrap();
        assert!(
            store
                .in_review_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // In-review task — should return it (transition through Working first)
        store
            .update_task_status(&t1.id, TaskStatus::Working)
            .unwrap();
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
    fn test_interrupted_task_for_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();

        // No tasks — should return None
        assert!(
            store
                .interrupted_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // Working task — should return None (not interrupted)
        let t1 = store
            .create_task(
                &project.id,
                "task1",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&t1.id, &session.id).unwrap();
        store
            .update_task_status(&t1.id, TaskStatus::Working)
            .unwrap();
        assert!(
            store
                .interrupted_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );

        // Interrupted task — should return it
        store
            .update_task_status(&t1.id, TaskStatus::Interrupted)
            .unwrap();
        let found = store
            .interrupted_task_for_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(found.id, t1.id);

        // Restore to working — should return None again
        store
            .update_task_status(&t1.id, TaskStatus::Working)
            .unwrap();
        assert!(
            store
                .interrupted_task_for_session(&session.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_delete_subtask() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "parent",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        let st = store.create_subtask(&task.id, "doomed", "bye").unwrap();

        store.delete_subtask(&st.id).unwrap();
        assert!(store.list_subtasks_for_task(&task.id).unwrap().is_empty());
    }

    #[test]
    fn test_update_session_progress() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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

    #[test]
    fn test_list_in_review_tasks_with_pr() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();

        // Create two tasks and move both to in_review
        let t1 = store
            .create_task(
                &project.id,
                "has-pr",
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
                &project.id,
                "no-pr",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        let t3 = store
            .create_task(
                &project.id,
                "pending-with-pr",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        // Transition through Working before InReview
        store
            .update_task_status(&t1.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&t1.id, TaskStatus::InReview)
            .unwrap();
        store
            .update_task_pr_url(&t1.id, "https://github.com/org/repo/pull/1")
            .unwrap();

        store
            .update_task_status(&t2.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&t2.id, TaskStatus::InReview)
            .unwrap();
        // t2 has no pr_url

        // t3 has a pr_url but is still pending
        store
            .update_task_pr_url(&t3.id, "https://github.com/org/repo/pull/3")
            .unwrap();

        let results = store.list_in_review_tasks_with_pr().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, t1.id);
        assert_eq!(
            results[0].pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/1")
        );
    }

    #[test]
    fn test_unassign_task_from_session() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let session = store
            .create_session(&project.id, "b", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &project.id,
                "task",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        store.assign_task_to_session(&task.id, &session.id).unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.session_id.as_deref(), Some(session.id.as_str()));

        store.unassign_task_from_session(&task.id).unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert!(t.session_id.is_none());
    }

    #[test]
    fn test_update_task_title() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "old",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        store.update_task_title(&task.id, "new title").unwrap();

        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.title, "new title");
        // updated_at should have changed
        assert_ne!(t.updated_at, t.created_at);
    }

    #[test]
    fn test_set_task_usage() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "task",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        assert_eq!(task.input_tokens, 0);
        assert_eq!(task.output_tokens, 0);

        store.set_task_usage(&task.id, 1000, 2000).unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.input_tokens, 1000);
        assert_eq!(t.output_tokens, 2000);

        // set_task_usage replaces, not adds
        store.set_task_usage(&task.id, 500, 300).unwrap();
        let t = store.get_task(&task.id).unwrap();
        assert_eq!(t.input_tokens, 500);
        assert_eq!(t.output_tokens, 300);
    }

    #[test]
    fn test_update_session_git_stats() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let session = store
            .create_session(&project.id, "branch", "/tmp/wt", "tab")
            .unwrap();
        assert_eq!(session.files_changed, 0);
        assert_eq!(session.lines_added, 0);
        assert_eq!(session.lines_removed, 0);

        store
            .update_session_git_stats(&session.id, 5, 120, 30)
            .unwrap();

        let s = store.get_session(&session.id).unwrap();
        assert_eq!(s.files_changed, 5);
        assert_eq!(s.lines_added, 120);
        assert_eq!(s.lines_removed, 30);
    }

    #[test]
    fn test_upsert_and_list_external_sessions() {
        let store = Store::open_in_memory().unwrap();
        let session = ExternalSession {
            id: "ext-001".into(),
            project_path: "/home/user/project".into(),
            project_name: "project".into(),
            model: Some("claude-sonnet-4-5-20250514".into()),
            git_branch: Some("main".into()),
            input_tokens: 1000,
            output_tokens: 500,
            started_at: Some("2025-01-01T00:00:00Z".into()),
            ended_at: Some("2025-01-01T01:00:00Z".into()),
            last_scanned_at: "2025-01-02T00:00:00Z".into(),
            jsonl_path: "/home/user/.claude/projects/abc/ext-001.jsonl".into(),
        };
        store.upsert_external_session(&session).unwrap();

        let sessions = store.list_external_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].input_tokens, 1000);
        assert_eq!(sessions[0].output_tokens, 500);
        assert_eq!(sessions[0].project_name, "project");

        // Upsert with updated tokens
        let updated = ExternalSession {
            input_tokens: 2000,
            output_tokens: 1000,
            last_scanned_at: "2025-01-03T00:00:00Z".into(),
            ..session
        };
        store.upsert_external_session(&updated).unwrap();

        let sessions = store.list_external_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].input_tokens, 2000);
        assert_eq!(sessions[0].output_tokens, 1000);
    }

    #[test]
    fn test_prune_stale_external_sessions() {
        let store = Store::open_in_memory().unwrap();
        let s1 = ExternalSession {
            id: "ext-active".into(),
            project_path: "/tmp/active".into(),
            project_name: "active".into(),
            model: None,
            git_branch: None,
            input_tokens: 0,
            output_tokens: 0,
            started_at: None,
            ended_at: None,
            last_scanned_at: "2025-01-01T00:00:00Z".into(),
            jsonl_path: "/path/to/active.jsonl".into(),
        };
        let s2 = ExternalSession {
            id: "ext-stale".into(),
            project_path: "/tmp/stale".into(),
            project_name: "stale".into(),
            model: None,
            git_branch: None,
            input_tokens: 0,
            output_tokens: 0,
            started_at: None,
            ended_at: None,
            last_scanned_at: "2025-01-01T00:00:00Z".into(),
            jsonl_path: "/path/to/stale.jsonl".into(),
        };
        store.upsert_external_session(&s1).unwrap();
        store.upsert_external_session(&s2).unwrap();
        assert_eq!(store.list_external_sessions().unwrap().len(), 2);

        // Prune: only ext-active is still active
        let mut active = std::collections::HashSet::new();
        active.insert("ext-active".to_string());
        let deleted = store.prune_stale_external_sessions(&active).unwrap();
        assert_eq!(deleted, 1);

        let remaining = store.list_external_sessions().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, "ext-active");
    }

    #[test]
    fn test_prune_stale_external_sessions_empty_active() {
        let store = Store::open_in_memory().unwrap();
        let session = ExternalSession {
            id: "ext-001".into(),
            project_path: "/tmp/proj".into(),
            project_name: "proj".into(),
            model: None,
            git_branch: None,
            input_tokens: 0,
            output_tokens: 0,
            started_at: None,
            ended_at: None,
            last_scanned_at: "2025-01-01T00:00:00Z".into(),
            jsonl_path: "/path/to/ext-001.jsonl".into(),
        };
        store.upsert_external_session(&session).unwrap();

        // Empty active set — should skip pruning to avoid accidental wipe
        let active = std::collections::HashSet::new();
        let deleted = store.prune_stale_external_sessions(&active).unwrap();
        assert_eq!(deleted, 0);
        // Session should still exist
        assert_eq!(store.list_external_sessions().unwrap().len(), 1);
    }

    #[test]
    fn test_external_session_scan_info() {
        let store = Store::open_in_memory().unwrap();
        let session = ExternalSession {
            id: "ext-002".into(),
            project_path: "/tmp/proj".into(),
            project_name: "proj".into(),
            model: None,
            git_branch: None,
            input_tokens: 0,
            output_tokens: 0,
            started_at: None,
            ended_at: None,
            last_scanned_at: "2025-01-01T00:00:00Z".into(),
            jsonl_path: "/path/to/ext-002.jsonl".into(),
        };
        store.upsert_external_session(&session).unwrap();

        let info = store.external_session_scan_info().unwrap();
        assert_eq!(info.len(), 1);
        let (path, scanned) = info.get("ext-002").unwrap();
        assert_eq!(path, "/path/to/ext-002.jsonl");
        assert_eq!(scanned, "2025-01-01T00:00:00Z");
    }

    #[test]
    fn test_list_all_project_repo_paths() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("proj-a", "/home/user/github/project-a", "main")
            .unwrap();
        store
            .create_project("proj-b", "/home/user/github/project-b", "main")
            .unwrap();

        let paths = store.list_all_project_repo_paths().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains("/home/user/github/project-a"));
        assert!(paths.contains("/home/user/github/project-b"));
    }

    #[test]
    fn test_list_all_project_repo_paths_empty() {
        let store = Store::open_in_memory().unwrap();
        let paths = store.list_all_project_repo_paths().unwrap();
        assert!(paths.is_empty());
    }

    #[test]
    fn test_try_update_task_status_valid() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "try-transition",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        // Valid: pending -> working
        let result = store
            .try_update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        assert!(result);
        assert_eq!(
            store.get_task(&task.id).unwrap().status,
            TaskStatus::Working
        );
    }

    #[test]
    fn test_try_update_task_status_invalid_returns_false() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "try-invalid",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        // Invalid: pending -> done (not allowed)
        let result = store
            .try_update_task_status(&task.id, TaskStatus::Done)
            .unwrap();
        assert!(!result);
        // Status should remain unchanged
        assert_eq!(
            store.get_task(&task.id).unwrap().status,
            TaskStatus::Pending
        );
    }

    #[test]
    fn test_try_update_task_status_stale_poll_scenario() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "stale-poll",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        // Simulate: task was in_review, polled, then user resumed (-> working)
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::InReview)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();

        // Stale poll result tries working -> ci_failed (invalid)
        let result = store
            .try_update_task_status(&task.id, TaskStatus::CiFailed)
            .unwrap();
        assert!(!result);
        assert_eq!(
            store.get_task(&task.id).unwrap().status,
            TaskStatus::Working
        );
    }
}
