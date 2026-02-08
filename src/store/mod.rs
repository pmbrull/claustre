mod models;
mod queries;

pub use models::*;
#[expect(unused_imports, reason = "re-export for convenience even if not all are used")]
pub use queries::*;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::config;

pub struct Store {
    pub conn: Connection,
}

impl Store {
    pub fn open() -> Result<Self> {
        let db_path = config::db_path()?;
        let conn = Connection::open(&db_path)
            .with_context(|| format!("failed to open database at {}", db_path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(Store { conn })
    }

    pub fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            "
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
        )?;
        Ok(())
    }
}
