//! Project CRUD operations.

use anyhow::{Context, Result};
use rusqlite::params;
use uuid::Uuid;

use crate::store::Store;
use crate::store::models::Project;

impl Store {
    pub fn create_project(
        &self,
        name: &str,
        repo_path: &str,
        default_branch: &str,
    ) -> Result<Project> {
        let id = Uuid::new_v4().to_string();
        self.conn
            .execute(
                "INSERT INTO projects (id, name, repo_path, default_branch) VALUES (?1, ?2, ?3, ?4)",
                params![id, name, repo_path, default_branch],
            )
            .with_context(|| format!("failed to create project '{name}'"))?;
        self.get_project(&id)
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        let project = self
            .conn
            .query_row(
                "SELECT id, name, repo_path, default_branch, created_at FROM projects WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        repo_path: row.get(2)?,
                        default_branch: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                },
            )
            .with_context(|| format!("failed to fetch project '{id}'"))?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, repo_path, default_branch, created_at FROM projects ORDER BY name",
        )?;
        let projects = stmt
            .query_map([], |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    repo_path: row.get(2)?,
                    default_branch: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(projects)
    }

    pub fn delete_project(&self, id: &str) -> Result<()> {
        self.in_transaction(|| {
            self.conn
                .execute("DELETE FROM tasks WHERE project_id = ?1", params![id])?;
            self.conn
                .execute("DELETE FROM sessions WHERE project_id = ?1", params![id])?;
            self.conn
                .execute("DELETE FROM projects WHERE id = ?1", params![id])?;
            Ok(())
        })
        .with_context(|| format!("failed to delete project '{id}'"))
    }
}
