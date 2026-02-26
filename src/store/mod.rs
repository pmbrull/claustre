//! `SQLite` persistence layer.
//!
//! Manages the database connection, versioned schema migrations, and
//! re-exports models and query methods used by the rest of the crate.

mod models;
mod queries;

pub use models::{
    ClaudeProgressItem, ClaudeStatus, ExternalSession, Project, PushMode, RateLimitState, Session,
    Subtask, Task, TaskMode, TaskStatus, TaskStatusCounts,
};
pub use queries::ProjectStats;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::config;

struct Migration {
    version: i64,
    sql: &'static str,
}

static MIGRATIONS: &[Migration] = &[
    Migration {
    version: 1,
    sql: "
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                repo_path TEXT NOT NULL UNIQUE,
                default_branch TEXT NOT NULL DEFAULT 'main',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id),
                branch_name TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                tab_label TEXT NOT NULL,
                claude_status TEXT NOT NULL DEFAULT 'idle',
                status_message TEXT NOT NULL DEFAULT '',
                last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
                files_changed INTEGER NOT NULL DEFAULT 0,
                lines_added INTEGER NOT NULL DEFAULT 0,
                lines_removed INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                closed_at TEXT,
                claude_progress TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id),
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'pending',
                mode TEXT NOT NULL DEFAULT 'supervised',
                session_id TEXT REFERENCES sessions(id),
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                sort_order INTEGER NOT NULL DEFAULT 0,
                pr_url TEXT
            );

            CREATE TABLE IF NOT EXISTS subtasks (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                title TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                status TEXT NOT NULL DEFAULT 'pending',
                sort_order INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT
            );

            CREATE TABLE rate_limit_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                is_rate_limited INTEGER NOT NULL DEFAULT 0,
                limit_type TEXT,
                rate_limited_at TEXT,
                reset_at TEXT,
                usage_5h_pct REAL NOT NULL DEFAULT 0.0,
                usage_7d_pct REAL NOT NULL DEFAULT 0.0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            INSERT INTO rate_limit_state (id, is_rate_limited, updated_at)
            VALUES (1, 0, datetime('now'));

            CREATE INDEX IF NOT EXISTS idx_tasks_project_id ON tasks(project_id);
            CREATE INDEX IF NOT EXISTS idx_tasks_session_id ON tasks(session_id);
            CREATE INDEX IF NOT EXISTS idx_sessions_project_closed ON sessions(project_id, closed_at);
            CREATE INDEX IF NOT EXISTS idx_subtasks_task_id ON subtasks(task_id);
        ",
    },
    Migration {
        version: 2,
        sql: "
            CREATE TABLE external_sessions (
                id TEXT PRIMARY KEY,
                project_path TEXT NOT NULL,
                project_name TEXT NOT NULL,
                model TEXT,
                git_branch TEXT,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                started_at TEXT,
                ended_at TEXT,
                last_scanned_at TEXT NOT NULL,
                jsonl_path TEXT NOT NULL
            );
            CREATE INDEX idx_external_sessions_project_path ON external_sessions(project_path);
            CREATE INDEX idx_external_sessions_ended_at ON external_sessions(ended_at);
        ",
    },
    Migration {
        version: 3,
        sql: "
            ALTER TABLE tasks ADD COLUMN branch TEXT;
            ALTER TABLE tasks ADD COLUMN push_mode TEXT NOT NULL DEFAULT 'pr';
        ",
    },
];

pub struct Store {
    conn: Connection,
}

#[cfg(test)]
impl Store {
    /// Create an in-memory store without running migrations.
    /// Used to test the migration system itself.
    fn open_unmigrated() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(Store { conn })
    }
}

impl Store {
    pub fn open() -> Result<Self> {
        let db_path = config::db_path()?;
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(Store { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);",
        )?;

        let current_version: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        for migration in MIGRATIONS.iter().filter(|m| m.version > current_version) {
            self.conn.execute_batch("BEGIN")?;
            let result = (|| -> Result<()> {
                self.conn.execute_batch(migration.sql)?;
                if current_version == 0 && migration.version == MIGRATIONS[0].version {
                    self.conn.execute(
                        "INSERT INTO schema_version (version) VALUES (?1)",
                        rusqlite::params![migration.version],
                    )?;
                } else {
                    self.conn.execute(
                        "UPDATE schema_version SET version = ?1",
                        rusqlite::params![migration.version],
                    )?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => self.conn.execute_batch("COMMIT")?,
                Err(e) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(e)
                        .with_context(|| format!("migration v{} failed", migration.version));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_fresh_database() {
        let store = Store::open_unmigrated().unwrap();
        store.migrate().unwrap();

        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);

        // Should be able to insert and query
        store.create_project("test", "/tmp/test", "main").unwrap();
        assert_eq!(store.list_projects().unwrap().len(), 1);
    }

    #[test]
    fn migrate_is_idempotent() {
        let store = Store::open_unmigrated().unwrap();
        store.migrate().unwrap();
        // Running migrate again should be a no-op
        store.migrate().unwrap();

        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);
    }
}
