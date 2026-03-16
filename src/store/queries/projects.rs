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
        is_git_linked: bool,
    ) -> Result<Project> {
        let id = Uuid::new_v4().to_string();
        self.conn
            .execute(
                "INSERT INTO projects (id, name, repo_path, default_branch, is_git_linked) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, name, repo_path, default_branch, is_git_linked],
            )
            .with_context(|| format!("failed to create project '{name}'"))?;
        self.get_project(&id)
    }

    pub fn get_project(&self, id: &str) -> Result<Project> {
        let project = self
            .conn
            .query_row(
                "SELECT id, name, repo_path, default_branch, created_at, is_git_linked FROM projects WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        repo_path: row.get(2)?,
                        default_branch: row.get(3)?,
                        created_at: row.get(4)?,
                        is_git_linked: row.get(5)?,
                    })
                },
            )
            .with_context(|| format!("failed to fetch project '{id}'"))?;
        Ok(project)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, repo_path, default_branch, created_at, is_git_linked FROM projects ORDER BY name",
        )?;
        let projects = stmt
            .query_map([], |row| {
                Ok(Project {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    repo_path: row.get(2)?,
                    default_branch: row.get(3)?,
                    created_at: row.get(4)?,
                    is_git_linked: row.get(5)?,
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

#[cfg(test)]
mod tests {
    use crate::store::{PushMode, Store, TaskMode};

    #[test]
    fn create_and_get_project() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("my-app", "/home/user/my-app", "main", true)
            .unwrap();

        assert_eq!(project.name, "my-app");
        assert_eq!(project.repo_path, "/home/user/my-app");
        assert_eq!(project.default_branch, "main");
        assert!(project.is_git_linked);

        let fetched = store.get_project(&project.id).unwrap();
        assert_eq!(fetched.id, project.id);
        assert_eq!(fetched.name, "my-app");
    }

    #[test]
    fn list_projects_ordered_by_name() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("zebra", "/tmp/z", "main", true)
            .unwrap();
        store
            .create_project("alpha", "/tmp/a", "main", true)
            .unwrap();
        store
            .create_project("middle", "/tmp/m", "main", true)
            .unwrap();

        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 3);
        assert_eq!(projects[0].name, "alpha");
        assert_eq!(projects[1].name, "middle");
        assert_eq!(projects[2].name, "zebra");
    }

    #[test]
    fn delete_project_cascades_to_tasks_and_sessions() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("p", "/tmp/p", "main", true).unwrap();
        store
            .create_task(
                &project.id,
                "t",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store
            .create_session(&project.id, "feat", "/tmp/wt", "tab")
            .unwrap();

        store.delete_project(&project.id).unwrap();

        assert!(store.list_projects().unwrap().is_empty());
        assert!(
            store
                .list_tasks_for_project(&project.id)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .list_active_sessions_for_project(&project.id)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn duplicate_repo_path_fails() {
        let store = Store::open_in_memory().unwrap();
        store
            .create_project("first", "/tmp/same", "main", true)
            .unwrap();

        // repo_path has UNIQUE constraint
        let result = store.create_project("second", "/tmp/same", "main", true);
        assert!(result.is_err());
    }

    #[test]
    fn get_nonexistent_project_fails() {
        let store = Store::open_in_memory().unwrap();
        let result = store.get_project("nonexistent-id");
        assert!(result.is_err());
    }
}
