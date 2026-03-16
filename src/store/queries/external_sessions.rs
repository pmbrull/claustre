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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::store::Store;
    use crate::store::models::ExternalSession;

    fn make_external_session(id: &str, project_path: &str) -> ExternalSession {
        ExternalSession {
            id: id.to_string(),
            project_path: project_path.to_string(),
            project_name: "test-project".to_string(),
            model: Some("claude-sonnet".to_string()),
            git_branch: Some("main".to_string()),
            input_tokens: 100,
            output_tokens: 50,
            started_at: Some("2025-01-01T00:00:00Z".to_string()),
            ended_at: Some("2025-01-01T01:00:00Z".to_string()),
            last_scanned_at: "2025-01-01T01:00:00Z".to_string(),
            jsonl_path: "/tmp/test.jsonl".to_string(),
        }
    }

    #[test]
    fn upsert_and_list_external_sessions() {
        let store = Store::open_in_memory().unwrap();
        let s1 = make_external_session("ext-1", "/home/user/proj-a");
        let s2 = make_external_session("ext-2", "/home/user/proj-b");

        store.upsert_external_session(&s1).unwrap();
        store.upsert_external_session(&s2).unwrap();

        let sessions = store.list_external_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn upsert_updates_existing_session() {
        let store = Store::open_in_memory().unwrap();
        let mut s = make_external_session("ext-1", "/home/user/proj");
        store.upsert_external_session(&s).unwrap();

        // Update the same session with new token counts
        s.input_tokens = 999;
        s.output_tokens = 888;
        store.upsert_external_session(&s).unwrap();

        let sessions = store.list_external_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].input_tokens, 999);
        assert_eq!(sessions[0].output_tokens, 888);
    }

    #[test]
    fn prune_stale_keeps_active_sessions() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_external_session(&make_external_session("keep", "/tmp/keep"))
            .unwrap();
        store
            .upsert_external_session(&make_external_session("remove", "/tmp/remove"))
            .unwrap();

        let mut active = HashSet::new();
        active.insert("keep".to_string());

        let deleted = store.prune_stale_external_sessions(&active).unwrap();
        assert_eq!(deleted, 1);

        let sessions = store.list_external_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "keep");
    }

    #[test]
    fn prune_stale_empty_active_ids_skips_when_sessions_exist() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_external_session(&make_external_session("s1", "/tmp/s1"))
            .unwrap();

        // Empty active_ids with existing sessions should NOT delete (safety guard)
        let deleted = store
            .prune_stale_external_sessions(&HashSet::new())
            .unwrap();
        assert_eq!(deleted, 0);

        let sessions = store.list_external_sessions().unwrap();
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn prune_stale_empty_active_ids_ok_when_no_sessions() {
        let store = Store::open_in_memory().unwrap();

        // Empty active_ids with no existing sessions is fine
        let deleted = store
            .prune_stale_external_sessions(&HashSet::new())
            .unwrap();
        assert_eq!(deleted, 0);
    }

    #[test]
    fn external_session_scan_info_returns_path_and_scanned_at() {
        let store = Store::open_in_memory().unwrap();
        let mut s = make_external_session("s1", "/tmp/p");
        s.jsonl_path = "/home/user/.claude/projects/hash/s1.jsonl".to_string();
        s.last_scanned_at = "2025-06-15T12:00:00Z".to_string();
        store.upsert_external_session(&s).unwrap();

        let info = store.external_session_scan_info().unwrap();
        assert_eq!(info.len(), 1);
        let (path, scanned) = info.get("s1").unwrap();
        assert_eq!(path, &s.jsonl_path);
        assert_eq!(scanned, "2025-06-15T12:00:00Z");
    }

    #[test]
    fn list_all_project_repo_paths_returns_registered_paths() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("a", "/home/user/project-a", "main", true)
            .unwrap();
        store
            .create_project("b", "/home/user/project-b", "main", true)
            .unwrap();

        let paths = store.list_all_project_repo_paths().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains("/home/user/project-a"));
        assert!(paths.contains("/home/user/project-b"));
    }
}
