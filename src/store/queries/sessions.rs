//! Session CRUD operations.

use anyhow::{Context, Result};
use rusqlite::params;
use tracing::warn;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::{ClaudeProgressItem, ClaudeStatus, Session};

/// Column list for all queries that use `row_to_session`.
/// Keep in sync with the field mapping in `row_to_session` below.
const SESSION_COLUMNS: &str = "\
    id, project_id, branch_name, worktree_path, tab_label, \
    claude_status, status_message, last_activity_at, \
    files_changed, lines_added, lines_removed, \
    created_at, closed_at, claude_progress, claude_session_id";

impl Store {
    pub fn create_session(
        &self,
        project_id: &str,
        branch_name: &str,
        worktree_path: &str,
        tab_label: &str,
    ) -> Result<Session> {
        let id = Uuid::new_v4().to_string();
        self.conn
            .execute(
                "INSERT INTO sessions (id, project_id, branch_name, worktree_path, tab_label)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, project_id, branch_name, worktree_path, tab_label],
            )
            .with_context(|| format!("failed to create session for branch '{branch_name}'"))?;
        self.get_session(&id)
    }

    pub fn get_session(&self, id: &str) -> Result<Session> {
        let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE id = ?1");
        let session = self
            .conn
            .query_row(&sql, params![id], Self::row_to_session)
            .with_context(|| format!("failed to fetch session '{id}'"))?;
        Ok(session)
    }

    pub fn list_active_sessions_for_project(&self, project_id: &str) -> Result<Vec<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions \
             WHERE project_id = ?1 AND closed_at IS NULL \
             ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let sessions = stmt
            .query_map(params![project_id], Self::row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    /// List all sessions (including closed) for a project.
    /// Used by the TUI to show session details for completed tasks.
    pub fn list_sessions_for_project(&self, project_id: &str) -> Result<Vec<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM sessions \
             WHERE project_id = ?1 \
             ORDER BY created_at"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let sessions = stmt
            .query_map(params![project_id], Self::row_to_session)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sessions)
    }

    fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
        let status_str: String = row.get(5)?;
        let progress_str: String = row.get(13)?;
        let id: String = row.get(0)?;
        let claude_progress = if progress_str.is_empty() {
            vec![]
        } else {
            serde_json::from_str(&progress_str).unwrap_or_else(|e| {
                warn!(session_id = %id, error = %e, "failed to parse claude_progress JSON, defaulting to empty");
                vec![]
            })
        };
        Ok(Session {
            claude_status: status_str.parse().unwrap_or_else(|_| {
                warn!(session_id = %id, raw = %status_str, "unknown claude status in DB, defaulting to Idle");
                ClaudeStatus::Idle
            }),
            id,
            project_id: row.get(1)?,
            branch_name: row.get(2)?,
            worktree_path: row.get(3)?,
            tab_label: row.get(4)?,
            status_message: row.get(6)?,
            last_activity_at: row.get(7)?,
            files_changed: row.get(8)?,
            lines_added: row.get(9)?,
            lines_removed: row.get(10)?,
            created_at: row.get(11)?,
            closed_at: row.get(12)?,
            claude_progress,
            claude_session_id: row.get(14)?,
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

    pub fn set_claude_session_id(&self, id: &str, claude_session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET claude_session_id = ?1 WHERE id = ?2",
            params![claude_session_id, id],
        )?;
        Ok(())
    }

    pub fn update_session_progress(&self, id: &str, progress: &[ClaudeProgressItem]) -> Result<()> {
        let json = serde_json::to_string(progress)?;
        self.conn.execute(
            "UPDATE sessions SET claude_progress = ?1 WHERE id = ?2",
            params![json, id],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{ClaudeProgressItem, ClaudeStatus, Store};

    fn setup(store: &Store) -> (String, String) {
        let project = store.create_project("p", "/tmp/p", "main").unwrap();
        let session = store
            .create_session(&project.id, "feat-branch", "/tmp/wt", "p:feat")
            .unwrap();
        (project.id, session.id)
    }

    #[test]
    fn create_session_populates_all_fields() {
        let store = Store::open_in_memory().unwrap();
        let (pid, sid) = setup(&store);
        let session = store.get_session(&sid).unwrap();

        assert_eq!(session.project_id, pid);
        assert_eq!(session.branch_name, "feat-branch");
        assert_eq!(session.worktree_path, "/tmp/wt");
        assert_eq!(session.tab_label, "p:feat");
        assert_eq!(session.claude_status, ClaudeStatus::Idle);
        assert!(session.closed_at.is_none());
        assert!(session.claude_progress.is_empty());
        assert!(session.claude_session_id.is_none());
    }

    #[test]
    fn update_session_status_working_does_not_update_activity() {
        let store = Store::open_in_memory().unwrap();
        let (_, sid) = setup(&store);

        let before = store.get_session(&sid).unwrap().last_activity_at;
        store
            .update_session_status(&sid, ClaudeStatus::Working, "working on it")
            .unwrap();

        let session = store.get_session(&sid).unwrap();
        assert_eq!(session.claude_status, ClaudeStatus::Working);
        assert_eq!(session.status_message, "working on it");
        // Working should NOT update last_activity_at
        assert_eq!(session.last_activity_at, before);
    }

    #[test]
    fn update_session_status_idle_updates_activity() {
        let store = Store::open_in_memory().unwrap();
        let (_, sid) = setup(&store);

        let before = store.get_session(&sid).unwrap().last_activity_at;
        store
            .update_session_status(&sid, ClaudeStatus::Idle, "")
            .unwrap();

        let session = store.get_session(&sid).unwrap();
        assert_eq!(session.claude_status, ClaudeStatus::Idle);
        // Non-working status SHOULD update last_activity_at
        assert_ne!(session.last_activity_at, before);
    }

    #[test]
    fn update_git_stats() {
        let store = Store::open_in_memory().unwrap();
        let (_, sid) = setup(&store);

        store.update_session_git_stats(&sid, 5, 100, 30).unwrap();

        let session = store.get_session(&sid).unwrap();
        assert_eq!(session.files_changed, 5);
        assert_eq!(session.lines_added, 100);
        assert_eq!(session.lines_removed, 30);
    }

    #[test]
    fn close_session_sets_closed_at() {
        let store = Store::open_in_memory().unwrap();
        let (_, sid) = setup(&store);

        assert!(store.get_session(&sid).unwrap().closed_at.is_none());

        store.close_session(&sid).unwrap();

        assert!(store.get_session(&sid).unwrap().closed_at.is_some());
    }

    #[test]
    fn list_active_sessions_excludes_closed() {
        let store = Store::open_in_memory().unwrap();
        let (pid, sid1) = setup(&store);
        let s2 = store
            .create_session(&pid, "other", "/tmp/wt2", "tab2")
            .unwrap();

        // Close sid1
        store.close_session(&sid1).unwrap();

        let active = store.list_active_sessions_for_project(&pid).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, s2.id);
    }

    #[test]
    fn list_sessions_includes_closed() {
        let store = Store::open_in_memory().unwrap();
        let (pid, sid) = setup(&store);
        store.close_session(&sid).unwrap();

        let all = store.list_sessions_for_project(&pid).unwrap();
        assert_eq!(all.len(), 1);
        assert!(all[0].closed_at.is_some());
    }

    #[test]
    fn set_claude_session_id() {
        let store = Store::open_in_memory().unwrap();
        let (_, sid) = setup(&store);

        store.set_claude_session_id(&sid, "claude-abc-123").unwrap();

        let session = store.get_session(&sid).unwrap();
        assert_eq!(session.claude_session_id.as_deref(), Some("claude-abc-123"));
    }

    #[test]
    fn update_session_progress_round_trips_json() {
        let store = Store::open_in_memory().unwrap();
        let (_, sid) = setup(&store);

        let progress = vec![
            ClaudeProgressItem {
                subject: "Step 1".to_string(),
                status: "done".to_string(),
            },
            ClaudeProgressItem {
                subject: "Step 2".to_string(),
                status: "pending".to_string(),
            },
        ];
        store.update_session_progress(&sid, &progress).unwrap();

        let session = store.get_session(&sid).unwrap();
        assert_eq!(session.claude_progress.len(), 2);
        assert_eq!(session.claude_progress[0].subject, "Step 1");
        assert_eq!(session.claude_progress[0].status, "done");
        assert_eq!(session.claude_progress[1].subject, "Step 2");
        assert_eq!(session.claude_progress[1].status, "pending");
    }

    #[test]
    fn empty_progress_string_parses_to_empty_vec() {
        let store = Store::open_in_memory().unwrap();
        let (_, sid) = setup(&store);

        // Default progress is empty string in DB
        let session = store.get_session(&sid).unwrap();
        assert!(session.claude_progress.is_empty());
    }
}
