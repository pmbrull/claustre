//! Stats queries and the `ProjectStats` struct.

use anyhow::{Context, Result};
use rusqlite::params;

use crate::store::Store;
use crate::store::models::Task;

impl Store {
    pub fn project_stats(&self, project_id: &str) -> Result<ProjectStats> {
        let stats = self
            .conn
            .query_row(
                "SELECT
                    COUNT(*),
                    COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0),
                    (SELECT COUNT(*) FROM sessions WHERE project_id = ?1),
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
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
                        total_time_seconds: row.get(5)?,
                    })
                },
            )
            .with_context(|| format!("failed to query stats for project '{project_id}'"))?;
        Ok(stats)
    }

    pub fn count_tasks_by_status(
        &self,
        project_id: &str,
    ) -> Result<super::super::models::TaskStatusCounts> {
        let mut stmt = self.conn.prepare(
            "SELECT status, COUNT(*) FROM tasks WHERE project_id = ?1 AND status != 'done' GROUP BY status",
        )?;
        let mut counts = super::super::models::TaskStatusCounts::default();
        let rows = stmt.query_map(params![project_id], |row| {
            let status: String = row.get(0)?;
            let count: usize = row.get(1)?;
            Ok((status, count))
        })?;
        for row in rows {
            let (status, count) = row?;
            match status.as_str() {
                "draft" => counts.draft = count,
                "pending" => counts.pending = count,
                "working" => counts.working = count,
                "interrupted" => counts.interrupted = count,
                "in_review" => counts.in_review = count,
                "conflict" => counts.conflict = count,
                "ci_failed" => counts.ci_failed = count,
                "error" => counts.error = count,
                _ => {}
            }
        }
        Ok(counts)
    }

    /// Return session IDs for completed push-mode tasks that still have open sessions.
    /// These sessions need auto-teardown since there's no PR merge to trigger cleanup.
    pub fn sessions_needing_push_mode_cleanup(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, t.title
             FROM sessions s
             JOIN tasks t ON t.session_id = s.id
             WHERE s.closed_at IS NULL
               AND t.status = 'done'
               AND t.push_mode = 'push'",
        )?;
        let results = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(results)
    }

    /// Return all tasks in `in_review`, `conflict`, `ci_failed`, or `working` status that have a PR URL.
    /// Used by the TUI's PR merge/conflict/CI poller.
    /// `working` tasks are included so that CI status continues to be tracked
    /// when the user resumes work on a task that already has an open PR.
    pub fn list_in_review_tasks_with_pr(&self) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {} FROM tasks \
             WHERE status IN ('in_review', 'conflict', 'ci_failed', 'working') AND pr_url IS NOT NULL",
            super::tasks::TASK_COLUMNS,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let tasks = stmt
            .query_map([], Self::row_to_task)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(tasks)
    }
}

#[derive(Debug, Clone)]
pub struct ProjectStats {
    pub total_tasks: i64,
    pub completed_tasks: i64,
    pub total_sessions: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
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
    use crate::store::{PushMode, TaskMode, TaskStatus};

    fn stats(
        total_tasks: i64,
        completed_tasks: i64,
        total_sessions: i64,
        input_tokens: i64,
        output_tokens: i64,
        time_secs: i64,
    ) -> ProjectStats {
        ProjectStats {
            total_tasks,
            completed_tasks,
            total_sessions,
            total_input_tokens: input_tokens,
            total_output_tokens: output_tokens,
            total_time_seconds: time_secs,
        }
    }

    // ── ProjectStats formatting ──

    #[test]
    fn total_tokens_sums_input_and_output() {
        let s = stats(1, 1, 1, 5000, 3000, 0);
        assert_eq!(s.total_tokens(), 8000);
    }

    #[test]
    fn total_tokens_zero() {
        let s = stats(0, 0, 0, 0, 0, 0);
        assert_eq!(s.total_tokens(), 0);
    }

    #[test]
    fn formatted_time_minutes_only() {
        let s = stats(0, 0, 0, 0, 0, 300); // 5 minutes
        assert_eq!(s.formatted_time(), "5m");
    }

    #[test]
    fn formatted_time_hours_and_minutes() {
        let s = stats(0, 0, 0, 0, 0, 5400); // 1h 30m
        assert_eq!(s.formatted_time(), "1h 30m");
    }

    #[test]
    fn formatted_time_exact_hours() {
        let s = stats(0, 0, 0, 0, 0, 7200); // 2h 0m
        assert_eq!(s.formatted_time(), "2h 0m");
    }

    #[test]
    fn formatted_time_zero() {
        let s = stats(0, 0, 0, 0, 0, 0);
        assert_eq!(s.formatted_time(), "0m");
    }

    #[test]
    fn avg_task_time_no_completed_tasks() {
        let s = stats(5, 0, 1, 0, 0, 600);
        assert_eq!(s.avg_task_time_seconds(), 0);
    }

    #[test]
    fn avg_task_time_with_completed_tasks() {
        let s = stats(10, 5, 1, 0, 0, 600); // 600s / 5 = 120s
        assert_eq!(s.avg_task_time_seconds(), 120);
    }

    #[test]
    fn formatted_avg_task_time_seconds() {
        let s = stats(1, 1, 0, 0, 0, 45); // 45s < 1m
        assert_eq!(s.formatted_avg_task_time(), "45s");
    }

    #[test]
    fn formatted_avg_task_time_minutes() {
        let s = stats(1, 1, 0, 0, 0, 180); // 3 minutes
        assert_eq!(s.formatted_avg_task_time(), "3m");
    }

    #[test]
    fn formatted_avg_task_time_zero() {
        let s = stats(0, 0, 0, 0, 0, 0);
        assert_eq!(s.formatted_avg_task_time(), "0s");
    }

    // ── Database-backed stats queries ──

    #[test]
    fn project_stats_empty_project() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();

        let stats = store.project_stats(&project.id).unwrap();
        assert_eq!(stats.total_tasks, 0);
        assert_eq!(stats.completed_tasks, 0);
        assert_eq!(stats.total_sessions, 0);
        assert_eq!(stats.total_tokens(), 0);
    }

    #[test]
    fn project_stats_counts_tasks_and_sessions() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();

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
        let t2 = store
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
        store.set_task_usage(&t2.id, 1000, 500).unwrap();
        store
            .create_session(&project.id, "feat", "/tmp/wt", "tab")
            .unwrap();

        let stats = store.project_stats(&project.id).unwrap();
        assert_eq!(stats.total_tasks, 2);
        assert_eq!(stats.total_sessions, 1);
        assert_eq!(stats.total_input_tokens, 1000);
        assert_eq!(stats.total_output_tokens, 500);
    }

    #[test]
    fn count_tasks_by_status_excludes_done() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();

        let _t1 = store
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
        let t2 = store
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
        // t1 stays pending, t2 goes to working then done
        store
            .update_task_status(&t2.id, TaskStatus::Working)
            .unwrap();
        store.update_task_status(&t2.id, TaskStatus::Done).unwrap();

        // Create a draft task too
        let t3 = store
            .create_task(
                &project.id,
                "t3",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        // Default status is pending, move to draft
        store
            .conn
            .execute(
                "UPDATE tasks SET status = 'draft' WHERE id = ?1",
                rusqlite::params![t3.id],
            )
            .unwrap();

        let counts = store.count_tasks_by_status(&project.id).unwrap();
        // pending: t1, done: t2 (excluded), draft: t3
        assert_eq!(counts.pending, 1);
        assert_eq!(counts.draft, 1);
        assert_eq!(counts.working, 0);

        // Verify done is excluded (done tasks shouldn't appear)
        let total_non_done = counts.draft
            + counts.pending
            + counts.working
            + counts.interrupted
            + counts.in_review
            + counts.conflict
            + counts.ci_failed
            + counts.error;
        assert_eq!(total_non_done, 2);
    }

    #[test]
    fn sessions_needing_push_mode_cleanup_finds_done_push_tasks() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &project.id,
                "push-task",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Push,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Done)
            .unwrap();

        let results = store.sessions_needing_push_mode_cleanup().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, session.id);
        assert_eq!(results[0].1, "push-task");
    }

    #[test]
    fn sessions_needing_push_mode_cleanup_ignores_pr_mode() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab")
            .unwrap();
        let task = store
            .create_task(
                &project.id,
                "pr-task",
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
            .update_task_status(&task.id, TaskStatus::Done)
            .unwrap();

        let results = store.sessions_needing_push_mode_cleanup().unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn list_in_review_tasks_with_pr_filters_correctly() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();

        // Task with PR in in_review — should be found
        let t1 = store
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
            .update_task_status(&t1.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&t1.id, TaskStatus::InReview)
            .unwrap();
        store
            .update_task_pr_url(&t1.id, "https://example.com/pr/1")
            .unwrap();

        // Task in in_review without PR — should NOT be found
        let t2 = store
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
            .update_task_status(&t2.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_status(&t2.id, TaskStatus::InReview)
            .unwrap();

        // Working task with PR — should be found (CI status tracking)
        let t3 = store
            .create_task(
                &project.id,
                "t3",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .update_task_status(&t3.id, TaskStatus::Working)
            .unwrap();
        store
            .update_task_pr_url(&t3.id, "https://example.com/pr/3")
            .unwrap();

        // Working task without PR — should NOT be found
        let t4 = store
            .create_task(
                &project.id,
                "t4",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .update_task_status(&t4.id, TaskStatus::Working)
            .unwrap();

        let results = store.list_in_review_tasks_with_pr().unwrap();
        assert_eq!(results.len(), 2);
        let ids: Vec<_> = results.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&t1.id.as_str()));
        assert!(ids.contains(&t3.id.as_str()));
    }

    #[test]
    fn list_in_review_tasks_with_pr_includes_ci_failed() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();

        let task = store
            .create_task(
                &project.id,
                "ci-fail",
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
            .update_task_status(&task.id, TaskStatus::InReview)
            .unwrap();
        store
            .update_task_pr_url(&task.id, "https://example.com/pr/1")
            .unwrap();
        store
            .update_task_ci_status(&task.id, Some(crate::store::CiStatus::Failed))
            .unwrap();
        store
            .update_task_status(&task.id, TaskStatus::CiFailed)
            .unwrap();

        let results = store.list_in_review_tasks_with_pr().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, task.id);
        assert_eq!(results[0].status, TaskStatus::CiFailed);
    }
}
