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

    /// Return all tasks in `in_review`, `conflict`, or `ci_failed` status that have a PR URL.
    /// Used by the TUI's PR merge/conflict/CI poller.
    pub fn list_in_review_tasks_with_pr(&self) -> Result<Vec<Task>> {
        let sql = format!(
            "SELECT {} FROM tasks \
             WHERE status IN ('in_review', 'conflict', 'ci_failed') AND pr_url IS NOT NULL",
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
