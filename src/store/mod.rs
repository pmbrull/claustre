mod models;
mod queries;

#[allow(
    unused_imports,
    reason = "Subtask exported for use in later tasks (MCP, TUI)"
)]
pub use models::{
    ClaudeStatus, Project, RateLimitState, Session, Subtask, Task, TaskMode, TaskStatus,
};

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
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id),
                branch_name TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                zellij_tab_name TEXT NOT NULL,
                claude_status TEXT NOT NULL DEFAULT 'idle',
                status_message TEXT NOT NULL DEFAULT '',
                last_activity_at TEXT NOT NULL DEFAULT (datetime('now')),
                files_changed INTEGER NOT NULL DEFAULT 0,
                lines_added INTEGER NOT NULL DEFAULT 0,
                lines_removed INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                closed_at TEXT
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
                cost REAL NOT NULL DEFAULT 0.0
            );
        ",
    },
    Migration {
        version: 2,
        sql: "
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
        ",
    },
    Migration {
        version: 3,
        sql: "
            ALTER TABLE tasks ADD COLUMN sort_order INTEGER NOT NULL DEFAULT 0;
            UPDATE tasks SET sort_order = CAST((julianday(created_at) - 2460000) * 86400 AS INTEGER);
        ",
    },
    Migration {
        version: 4,
        sql: "
            ALTER TABLE tasks ADD COLUMN pr_url TEXT;
        ",
    },
    Migration {
        version: 5,
        sql: "
            CREATE TABLE subtasks (
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
        ",
    },
];

pub struct Store {
    conn: Connection,
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
        // Create schema_version table if it doesn't exist
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

        // Detect legacy databases: have tables but no schema_version row
        if current_version == 0 {
            let has_projects: bool = self
                .conn
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='projects'",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(false);

            if has_projects {
                // Legacy DB — tables already exist, just record v1
                self.conn
                    .execute("INSERT INTO schema_version (version) VALUES (1)", [])?;
                // Apply any migrations after v1
                for migration in MIGRATIONS.iter().filter(|m| m.version > 1) {
                    self.conn.execute_batch(migration.sql)?;
                    self.conn.execute(
                        "UPDATE schema_version SET version = ?1",
                        rusqlite::params![migration.version],
                    )?;
                }
                return Ok(());
            }
        }

        // Apply unapplied migrations in order
        for migration in MIGRATIONS.iter().filter(|m| m.version > current_version) {
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
        }

        Ok(())
    }
}
