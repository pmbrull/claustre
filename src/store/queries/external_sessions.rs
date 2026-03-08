//! External session CRUD operations.

use anyhow::Result;
use rusqlite::params;
use tracing::warn;

use crate::store::Store;
use crate::store::models::ExternalSession;

impl Store {
    pub fn upsert_external_session(&self, session: &ExternalSession) -> Result<()> {
        self.conn.execute(
            "INSERT INTO external_sessions (id, project_path, project_name, model, git_branch,
                input_tokens, output_tokens, started_at, ended_at, last_scanned_at, jsonl_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
                project_path = excluded.project_path,
                project_name = excluded.project_name,
                model = excluded.model,
                git_branch = excluded.git_branch,
                input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens,
                started_at = excluded.started_at,
                ended_at = excluded.ended_at,
                last_scanned_at = excluded.last_scanned_at,
                jsonl_path = excluded.jsonl_path",
            params![
                session.id,
                session.project_path,
                session.project_name,
                session.model,
                session.git_branch,
                session.input_tokens,
                session.output_tokens,
                session.started_at,
                session.ended_at,
                session.last_scanned_at,
                session.jsonl_path,
            ],
        )?;
        Ok(())
    }

    /// Returns all external sessions currently in the database, ordered by most recent first.
    pub fn list_external_sessions(&self) -> Result<Vec<ExternalSession>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, project_path, project_name, model, git_branch,
                    input_tokens, output_tokens, started_at, ended_at,
                    last_scanned_at, jsonl_path
             FROM external_sessions
             ORDER BY ended_at DESC NULLS LAST",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ExternalSession {
                id: row.get(0)?,
                project_path: row.get(1)?,
                project_name: row.get(2)?,
                model: row.get(3)?,
                git_branch: row.get(4)?,
                input_tokens: row.get(5)?,
                output_tokens: row.get(6)?,
                started_at: row.get(7)?,
                ended_at: row.get(8)?,
                last_scanned_at: row.get(9)?,
                jsonl_path: row.get(10)?,
            })
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Delete external sessions whose IDs are not in the given active set.
    /// Returns the number of rows deleted.
    ///
    /// If `active_ids` is empty, checks whether there are existing external sessions
    /// before deleting — avoids accidental wipe when the scanner fails to find any
    /// active sessions (e.g. `~/.claude/projects/` is temporarily empty).
    pub fn prune_stale_external_sessions(
        &self,
        active_ids: &std::collections::HashSet<String>,
    ) -> Result<usize> {
        if active_ids.is_empty() {
            let existing: i64 =
                self.conn
                    .query_row("SELECT COUNT(*) FROM external_sessions", [], |row| {
                        row.get(0)
                    })?;
            if existing > 0 {
                warn!(
                    existing_count = existing,
                    "skipping external session prune: active_ids is empty but {} sessions exist",
                    existing
                );
                return Ok(0);
            }
            return Ok(0);
        }
        // Build a parameterized IN clause
        let placeholders: Vec<String> = (1..=active_ids.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "DELETE FROM external_sessions WHERE id NOT IN ({})",
            placeholders.join(", ")
        );
        let ids: Vec<&str> = active_ids.iter().map(String::as_str).collect();
        let params: Vec<&dyn rusqlite::types::ToSql> = ids
            .iter()
            .map(|s| s as &dyn rusqlite::types::ToSql)
            .collect();
        let deleted = self.conn.execute(&sql, params.as_slice())?;
        Ok(deleted)
    }

    /// Returns a map of session ID → (`jsonl_path`, `last_scanned_at`) for incremental scanning.
    pub fn external_session_scan_info(
        &self,
    ) -> Result<std::collections::HashMap<String, (String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, jsonl_path, last_scanned_at FROM external_sessions")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut map = std::collections::HashMap::new();
        for row in rows {
            let (id, path, scanned) = row?;
            map.insert(id, (path, scanned));
        }
        Ok(map)
    }

    /// Returns all registered project repo paths for filtering during external scanning.
    ///
    /// Sessions whose `project_path` matches a registered project's `repo_path` are
    /// excluded from external session tracking — they belong to a known claustre project.
    pub fn list_all_project_repo_paths(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT repo_path FROM projects")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut set = std::collections::HashSet::new();
        for row in rows {
            set.insert(row?);
        }
        Ok(set)
    }
}
