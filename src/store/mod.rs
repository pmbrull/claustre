//! `SQLite` persistence layer.
//!
//! Manages the database connection, versioned schema migrations, and
//! re-exports models and query methods used by the rest of the crate.

mod models;
mod queries;

pub use models::{
    CiStatus, ClaudeProgressItem, ClaudeStatus, ExternalSession, Project, PushMode, RateLimitState,
    Session, Subtask, Task, TaskMode, TaskStatus, TaskStatusCounts,
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
    Migration {
        version: 4,
        sql: "
            ALTER TABLE tasks ADD COLUMN ci_status TEXT;
        ",
    },
    Migration {
        version: 5,
        sql: "
            ALTER TABLE tasks ADD COLUMN review_loop INTEGER NOT NULL DEFAULT 0;
        ",
    },
    Migration {
        version: 6,
        sql: "
            ALTER TABLE tasks ADD COLUMN base TEXT;
            UPDATE tasks SET base = branch, branch = NULL;
        ",
    },
    Migration {
        version: 7,
        sql: "
            ALTER TABLE sessions ADD COLUMN claude_session_id TEXT;
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
        Self::open_at(&db_path)
    }

    /// Open a database at a specific path. Useful for testing with temp files.
    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;
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

    /// Quick sanity check: run `SELECT 1` to prove the DB is accessible.
    pub fn health_check(&self) -> Result<()> {
        let result: i64 = self.conn.query_row("SELECT 1", [], |row| row.get(0))?;
        anyhow::ensure!(result == 1, "health check query returned unexpected value");
        Ok(())
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

    /// Run each migration step by step (v1, then v2, ..., then v7) and verify
    /// the schema version advances correctly after each one.
    #[test]
    fn migrate_sequential_upgrade() {
        let store = Store::open_unmigrated().unwrap();
        store
            .conn
            .execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")
            .unwrap();

        for migration in MIGRATIONS {
            // Simulate applying one migration at a time
            store.conn.execute_batch("BEGIN").unwrap();
            store.conn.execute_batch(migration.sql).unwrap();
            let current: i64 = store
                .conn
                .query_row(
                    "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            if current == 0 {
                store
                    .conn
                    .execute(
                        "INSERT INTO schema_version (version) VALUES (?1)",
                        rusqlite::params![migration.version],
                    )
                    .unwrap();
            } else {
                store
                    .conn
                    .execute(
                        "UPDATE schema_version SET version = ?1",
                        rusqlite::params![migration.version],
                    )
                    .unwrap();
            }
            store.conn.execute_batch("COMMIT").unwrap();

            let version: i64 = store
                .conn
                .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(version, migration.version);
        }

        // Verify final version matches latest
        let version: i64 = store
            .conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, MIGRATIONS.last().unwrap().version);
    }

    /// Verify the schema after all migrations contains the expected tables and columns.
    /// This catches accidental column removals or renames that would break row mappers.
    #[test]
    fn schema_has_expected_tables_and_columns() {
        let store = Store::open_unmigrated().unwrap();
        store.migrate().unwrap();

        // Check all expected tables exist
        let tables: Vec<String> = {
            let mut stmt = store
                .conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        let expected_tables = [
            "external_sessions",
            "projects",
            "rate_limit_state",
            "schema_version",
            "sessions",
            "subtasks",
            "tasks",
        ];
        for table in &expected_tables {
            assert!(
                tables.contains(&(*table).to_string()),
                "missing table: {table}"
            );
        }

        // Check critical columns exist in tasks table (covers all migration-added columns)
        let task_columns: Vec<String> = {
            let mut stmt = store.conn.prepare("PRAGMA table_info(tasks)").unwrap();
            stmt.query_map([], |row| row.get(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        let expected_task_columns = [
            "id",
            "project_id",
            "title",
            "description",
            "status",
            "mode",
            "session_id",
            "created_at",
            "updated_at",
            "started_at",
            "completed_at",
            "input_tokens",
            "output_tokens",
            "sort_order",
            "pr_url",
            // Added by migrations v3-v6:
            "branch",
            "push_mode",
            "ci_status",
            "review_loop",
            "base",
        ];
        for col in &expected_task_columns {
            assert!(
                task_columns.contains(&(*col).to_string()),
                "tasks table missing column: {col}"
            );
        }

        // Check sessions table has the v7 column
        let session_columns: Vec<String> = {
            let mut stmt = store.conn.prepare("PRAGMA table_info(sessions)").unwrap();
            stmt.query_map([], |row| row.get(1))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };

        let expected_session_columns = [
            "id",
            "project_id",
            "branch_name",
            "worktree_path",
            "tab_label",
            "claude_status",
            "status_message",
            "last_activity_at",
            "files_changed",
            "lines_added",
            "lines_removed",
            "created_at",
            "closed_at",
            "claude_progress",
            // Added by migration v7:
            "claude_session_id",
        ];
        for col in &expected_session_columns {
            assert!(
                session_columns.contains(&(*col).to_string()),
                "sessions table missing column: {col}"
            );
        }
    }

    /// Verify migration versions are sequential and non-duplicated.
    #[test]
    #[expect(clippy::cast_possible_wrap, reason = "migration count is tiny")]
    fn migration_versions_are_sequential() {
        for (i, migration) in MIGRATIONS.iter().enumerate() {
            assert_eq!(
                migration.version,
                (i as i64) + 1,
                "migration at index {i} has version {} but expected {}",
                migration.version,
                i + 1,
            );
        }
    }

    /// Verify `row_to_task` maps all columns correctly by round-tripping through
    /// `create_task` + `get_task`. If a column is added to the schema but not to
    /// `TASK_COLUMNS` or `row_to_task`, this will fail with a column index error.
    #[test]
    fn task_row_mapper_covers_all_columns() {
        let store = Store::open_unmigrated().unwrap();
        store.migrate().unwrap();

        let project = store.create_project("p", "/tmp/p", "main").unwrap();

        // Create a task with every optional field populated
        let task = store
            .create_task(
                &project.id,
                "mapper-test",
                "desc",
                super::TaskMode::Autonomous,
                Some("feat/x"),
                Some("develop"),
                super::PushMode::Push,
                true,
            )
            .unwrap();

        // Verify every field was set and round-trips correctly
        let fetched = store.get_task(&task.id).unwrap();
        assert_eq!(fetched.title, "mapper-test");
        assert_eq!(fetched.description, "desc");
        assert_eq!(fetched.mode, super::TaskMode::Autonomous);
        assert_eq!(fetched.branch.as_deref(), Some("feat/x"));
        assert_eq!(fetched.base.as_deref(), Some("develop"));
        assert_eq!(fetched.push_mode, super::PushMode::Push);
        assert!(fetched.review_loop);
        assert_eq!(fetched.status, super::TaskStatus::Pending);
        assert!(fetched.ci_status.is_none());

        // Also verify the task columns match by checking the column count in the schema
        let col_count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('tasks')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // tasks table should have 20 columns after all migrations
        assert_eq!(
            col_count, 20,
            "tasks table column count changed — update TASK_COLUMNS and row_to_task"
        );
    }

    /// Verify that `open_in_memory()` produces a DB where all CRUD operations work.
    /// This is a smoke test for the full migration + initial data path.
    #[test]
    fn open_in_memory_is_fully_functional() {
        let store = Store::open_in_memory().unwrap();

        // Verify health check works
        store.health_check().unwrap();

        // Full CRUD cycle
        let project = store.create_project("test", "/tmp/test", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "task",
                "desc",
                super::TaskMode::Supervised,
                None,
                None,
                super::PushMode::Pr,
                false,
            )
            .unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab")
            .unwrap();
        let subtask = store.create_subtask(&task.id, "step", "").unwrap();

        // Read back
        assert_eq!(store.list_projects().unwrap().len(), 1);
        assert_eq!(store.list_tasks_for_project(&project.id).unwrap().len(), 1);
        assert_eq!(
            store
                .list_active_sessions_for_project(&project.id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.list_subtasks_for_task(&task.id).unwrap().len(), 1);

        // Rate limit state exists (singleton)
        let state = store.get_rate_limit_state().unwrap();
        assert!(!state.is_rate_limited);

        // Clean up
        store.delete_subtask(&subtask.id).unwrap();
        store.close_session(&session.id).unwrap();
        store.delete_task(&task.id).unwrap();
        store.delete_project(&project.id).unwrap();
        assert_eq!(store.list_projects().unwrap().len(), 0);
    }
}
