//! Git-based state sync for sharing claustre state across machines.
//!
//! Exports portable state (projects, tasks, subtasks) as JSON files to
//! `~/.claustre/sync/`, a git repo that can be pushed/pulled between machines.
//! Sessions, rate limits, and other runtime state are not synced.
//!
//! ## Directory layout
//!
//! ```text
//! sync/
//!   projects/
//!     <project-name>/
//!       project.json               # Project metadata (name, default_branch)
//!       tasks/
//!         <task-uuid>.json         # Individual task with embedded subtasks
//!   config.toml                    # Shared config
//! ```
//!
//! Each task is stored as a separate file so that git diffs are granular and
//! concurrent edits to different tasks don't cause merge conflicts.
//!
//! ## Backward compatibility
//!
//! The import path also accepts the legacy flat format where each project was
//! a single `projects/<name>.json` file containing all tasks in a `tasks` array.
//! This allows pulling from a sync repo that hasn't been re-exported yet.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::store::Store;

/// Project metadata written to `project.json` (no tasks — those are separate files).
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncProjectMeta {
    pub name: String,
    pub default_branch: String,
}

/// Legacy format: project with all tasks inlined. Used only for backward-compatible import.
#[derive(Debug, Serialize, Deserialize)]
struct LegacySyncProject {
    name: String,
    default_branch: String,
    tasks: Vec<SyncTask>,
}

/// Portable task representation (no `session_id` — that's machine-specific).
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub mode: String,
    pub sort_order: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    pub push_mode: String,
    pub review_loop: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtasks: Vec<SyncSubtask>,
}

/// Portable subtask representation.
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncSubtask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub sort_order: i64,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

/// Result of an import operation.
pub struct ImportResult {
    pub projects_synced: usize,
    pub tasks_synced: usize,
    pub subtasks_synced: usize,
    pub skipped_projects: Vec<String>,
}

/// Initialize the sync git repo at `~/.claustre/sync/`.
///
/// If a remote URL is provided, clones from it. Otherwise, initializes a new repo.
pub fn init(remote_url: Option<&str>) -> Result<()> {
    let sync_dir = config::sync_dir()?;

    if sync_dir.join(".git").exists() {
        bail!("sync repo already exists at {}", sync_dir.display());
    }

    if let Some(url) = remote_url {
        let status = Command::new("git")
            .args(["clone", url])
            .arg(&sync_dir)
            .status()
            .context("failed to run git clone")?;
        if !status.success() {
            bail!("git clone failed");
        }
        println!("Cloned sync repo from {url}");
    } else {
        fs::create_dir_all(&sync_dir)?;
        let status = Command::new("git")
            .arg("-C")
            .arg(&sync_dir)
            .arg("init")
            .status()
            .context("failed to run git init")?;
        if !status.success() {
            bail!("git init failed");
        }
        println!("Initialized sync repo at {}", sync_dir.display());
        println!(
            "Add a remote with: git -C {} remote add origin <url>",
            sync_dir.display()
        );
    }

    fs::create_dir_all(sync_dir.join("projects"))?;

    Ok(())
}

/// Build a `SyncTask` from a store `Task` and its subtasks.
fn build_sync_task(task: &crate::store::Task, store: &Store) -> Result<SyncTask> {
    let subtasks = store.list_subtasks_for_task(&task.id)?;
    let sync_subtasks: Vec<SyncSubtask> = subtasks
        .iter()
        .map(|st| SyncSubtask {
            id: st.id.clone(),
            title: st.title.clone(),
            description: st.description.clone(),
            status: st.status.as_str().to_string(),
            sort_order: st.sort_order,
            created_at: st.created_at.clone(),
            started_at: st.started_at.clone(),
            completed_at: st.completed_at.clone(),
        })
        .collect();

    Ok(SyncTask {
        id: task.id.clone(),
        title: task.title.clone(),
        description: task.description.clone(),
        status: task.status.as_str().to_string(),
        mode: task.mode.as_str().to_string(),
        sort_order: task.sort_order,
        branch: task.branch.clone(),
        base: task.base.clone(),
        push_mode: task.push_mode.as_str().to_string(),
        review_loop: task.review_loop,
        created_at: task.created_at.clone(),
        updated_at: task.updated_at.clone(),
        started_at: task.started_at.clone(),
        completed_at: task.completed_at.clone(),
        input_tokens: task.input_tokens,
        output_tokens: task.output_tokens,
        pr_url: task.pr_url.clone(),
        ci_status: task.ci_status.map(|s| s.as_str().to_string()),
        subtasks: sync_subtasks,
    })
}

/// Export portable state from the database to the sync directory.
///
/// Layout: `projects/<name>/project.json` + `projects/<name>/tasks/<id>.json`
fn export_state(store: &Store, sync_dir: &Path) -> Result<usize> {
    let projects_dir = sync_dir.join("projects");

    // Clear existing project files and rewrite (git tracks content changes only)
    if projects_dir.exists() {
        fs::remove_dir_all(&projects_dir)?;
    }
    fs::create_dir_all(&projects_dir)?;

    let projects = store.list_projects()?;
    let count = projects.len();

    for project in &projects {
        let sanitized = sanitize_filename(&project.name);
        let proj_dir = projects_dir.join(&sanitized);
        let tasks_dir = proj_dir.join("tasks");
        fs::create_dir_all(&tasks_dir)?;

        // Write project metadata
        let meta = SyncProjectMeta {
            name: project.name.clone(),
            default_branch: project.default_branch.clone(),
        };
        let meta_json = serde_json::to_string_pretty(&meta)?;
        fs::write(proj_dir.join("project.json"), meta_json)
            .with_context(|| format!("failed to write project.json for {}", project.name))?;

        // Write each task as a separate file
        let tasks = store.list_tasks_for_project(&project.id)?;
        for task in &tasks {
            let sync_task = build_sync_task(task, store)?;
            let task_json = serde_json::to_string_pretty(&sync_task)?;
            fs::write(tasks_dir.join(format!("{}.json", task.id)), task_json)
                .with_context(|| format!("failed to write task {}", task.id))?;
        }
    }

    // Copy config.toml if it exists
    let config_path = config::base_dir()?.join("config.toml");
    if config_path.exists() {
        fs::copy(&config_path, sync_dir.join("config.toml"))?;
    }

    Ok(count)
}

/// Import tasks for a single project from a set of task JSON files.
fn import_tasks_from_dir(
    store: &Store,
    local_project_id: &str,
    tasks_dir: &Path,
    result: &mut ImportResult,
) -> Result<()> {
    if !tasks_dir.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(tasks_dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let content = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let sync_task: SyncTask = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        store.upsert_task_from_sync(local_project_id, &sync_task)?;
        result.tasks_synced += 1;

        for sync_subtask in &sync_task.subtasks {
            store.upsert_subtask_from_sync(&sync_task.id, sync_subtask)?;
            result.subtasks_synced += 1;
        }
    }

    Ok(())
}

/// Import state from the sync directory into the database.
///
/// Supports both the new hierarchical format (`projects/<name>/project.json` +
/// `projects/<name>/tasks/*.json`) and the legacy flat format
/// (`projects/<name>.json` with all tasks inlined).
fn import_state(store: &Store, sync_dir: &Path) -> Result<ImportResult> {
    let projects_dir = sync_dir.join("projects");
    if !projects_dir.exists() {
        bail!("no projects/ directory in sync repo");
    }

    let mut result = ImportResult {
        projects_synced: 0,
        tasks_synced: 0,
        subtasks_synced: 0,
        skipped_projects: Vec::new(),
    };

    let local_projects = store.list_projects()?;
    let project_by_name: HashMap<&str, &crate::store::Project> = local_projects
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();

    for entry in fs::read_dir(&projects_dir)?.flatten() {
        let path = entry.path();

        if path.is_dir() {
            // New hierarchical format: projects/<name>/project.json + tasks/*.json
            let meta_path = path.join("project.json");
            if !meta_path.exists() {
                continue;
            }

            let content = fs::read_to_string(&meta_path)
                .with_context(|| format!("failed to read {}", meta_path.display()))?;
            let meta: SyncProjectMeta = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", meta_path.display()))?;

            let Some(&local_project) = project_by_name.get(meta.name.as_str()) else {
                result.skipped_projects.push(meta.name.clone());
                continue;
            };

            import_tasks_from_dir(store, &local_project.id, &path.join("tasks"), &mut result)?;
            result.projects_synced += 1;
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            // Legacy flat format: projects/<name>.json with all tasks inlined
            let content = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let legacy: LegacySyncProject = serde_json::from_str(&content)
                .with_context(|| format!("failed to parse {}", path.display()))?;

            let Some(&local_project) = project_by_name.get(legacy.name.as_str()) else {
                result.skipped_projects.push(legacy.name.clone());
                continue;
            };

            for sync_task in &legacy.tasks {
                store.upsert_task_from_sync(&local_project.id, sync_task)?;
                result.tasks_synced += 1;

                for sync_subtask in &sync_task.subtasks {
                    store.upsert_subtask_from_sync(&sync_task.id, sync_subtask)?;
                    result.subtasks_synced += 1;
                }
            }
            result.projects_synced += 1;
        }
    }

    Ok(result)
}

/// Export state, commit, and push to the sync repo.
pub fn push(store: &Store) -> Result<()> {
    let sync_dir = config::sync_dir()?;
    if !sync_dir.join(".git").exists() {
        bail!("sync repo not initialized. Run `claustre sync init` first.");
    }

    let count = export_state(store, &sync_dir)?;

    // Stage all changes
    let status = Command::new("git")
        .arg("-C")
        .arg(&sync_dir)
        .args(["add", "-A"])
        .status()
        .context("failed to run git add")?;
    if !status.success() {
        bail!("git add failed");
    }

    // Check if there are changes to commit
    let diff_status = Command::new("git")
        .arg("-C")
        .arg(&sync_dir)
        .args(["diff", "--cached", "--quiet"])
        .status()
        .context("failed to run git diff")?;
    if diff_status.success() {
        println!("No changes to sync ({count} projects up to date).");
        return Ok(());
    }

    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC");
    let host = hostname();
    let msg = format!("sync: {host} at {now}");

    let status = Command::new("git")
        .arg("-C")
        .arg(&sync_dir)
        .args(["commit", "-m", &msg])
        .status()
        .context("failed to run git commit")?;
    if !status.success() {
        bail!("git commit failed");
    }

    // Push (may fail if no remote is configured — that's OK)
    let push_result = Command::new("git")
        .arg("-C")
        .arg(&sync_dir)
        .arg("push")
        .status();
    match push_result {
        Ok(s) if s.success() => println!("Synced {count} projects and pushed."),
        Ok(_) => println!("Synced {count} projects (committed locally, push failed — no remote?)."),
        Err(_) => println!("Synced {count} projects (committed locally, push unavailable)."),
    }

    Ok(())
}

/// Pull from the sync repo and import state.
pub fn pull(store: &Store) -> Result<()> {
    let sync_dir = config::sync_dir()?;
    if !sync_dir.join(".git").exists() {
        bail!("sync repo not initialized. Run `claustre sync init` first.");
    }

    // Try to pull (may fail if no remote — still import local state)
    let pull_result = Command::new("git")
        .arg("-C")
        .arg(&sync_dir)
        .args(["pull", "--rebase"])
        .status();
    match pull_result {
        Ok(s) if s.success() => {}
        _ => eprintln!("warning: git pull failed (no remote?), importing local sync state"),
    }

    let result = import_state(store, &sync_dir)?;

    println!(
        "Imported {} projects ({} tasks, {} subtasks).",
        result.projects_synced, result.tasks_synced, result.subtasks_synced,
    );
    if !result.skipped_projects.is_empty() {
        println!(
            "Skipped projects not registered locally: {}",
            result.skipped_projects.join(", ")
        );
        println!("Register them with `claustre add-project <name> <path>` then pull again.");
    }

    Ok(())
}

/// Sanitize a project name for use as a filename.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Get the hostname for commit messages.
fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{PushMode, Store, TaskMode};
    use std::fs;

    #[test]
    fn sanitize_filename_preserves_alphanumeric() {
        assert_eq!(sanitize_filename("my-project_1"), "my-project_1");
    }

    #[test]
    fn sanitize_filename_replaces_special_chars() {
        assert_eq!(sanitize_filename("my project/foo"), "my_project_foo");
        assert_eq!(sanitize_filename("a.b.c"), "a_b_c");
    }

    #[test]
    fn export_creates_hierarchical_layout() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("TestProject", "/tmp/test", "main")
            .unwrap();
        let task = store
            .create_task(
                &project.id,
                "task-1",
                "do stuff",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let count = export_state(&store, dir.path()).unwrap();

        assert_eq!(count, 1);

        // project.json should exist
        let meta_path = dir.path().join("projects/TestProject/project.json");
        assert!(meta_path.exists());
        let meta: SyncProjectMeta =
            serde_json::from_str(&fs::read_to_string(&meta_path).unwrap()).unwrap();
        assert_eq!(meta.name, "TestProject");
        assert_eq!(meta.default_branch, "main");

        // Task file should exist
        let task_path = dir
            .path()
            .join(format!("projects/TestProject/tasks/{}.json", task.id));
        assert!(task_path.exists());
        let sync_task: SyncTask =
            serde_json::from_str(&fs::read_to_string(&task_path).unwrap()).unwrap();
        assert_eq!(sync_task.title, "task-1");
        assert_eq!(sync_task.description, "do stuff");
    }

    #[test]
    fn export_includes_subtasks_in_task_file() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("P", "/tmp/p", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "task",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.create_subtask(&task.id, "step-1", "first").unwrap();
        store.create_subtask(&task.id, "step-2", "second").unwrap();

        let dir = tempfile::tempdir().unwrap();
        export_state(&store, dir.path()).unwrap();

        let task_path = dir
            .path()
            .join(format!("projects/P/tasks/{}.json", task.id));
        let sync_task: SyncTask =
            serde_json::from_str(&fs::read_to_string(&task_path).unwrap()).unwrap();
        assert_eq!(sync_task.subtasks.len(), 2);
        assert_eq!(sync_task.subtasks[0].title, "step-1");
    }

    #[test]
    fn export_multiple_tasks_creates_separate_files() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("P", "/tmp/p", "main").unwrap();
        let t1 = store
            .create_task(
                &project.id,
                "task-a",
                "",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        let t2 = store
            .create_task(
                &project.id,
                "task-b",
                "",
                TaskMode::Autonomous,
                None,
                None,
                PushMode::Push,
                false,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        export_state(&store, dir.path()).unwrap();

        let tasks_dir = dir.path().join("projects/P/tasks");
        let f1 = tasks_dir.join(format!("{}.json", t1.id));
        let f2 = tasks_dir.join(format!("{}.json", t2.id));
        assert!(f1.exists());
        assert!(f2.exists());

        let st1: SyncTask = serde_json::from_str(&fs::read_to_string(&f1).unwrap()).unwrap();
        let st2: SyncTask = serde_json::from_str(&fs::read_to_string(&f2).unwrap()).unwrap();
        assert_eq!(st1.title, "task-a");
        assert_eq!(st2.title, "task-b");
    }

    #[test]
    fn import_from_hierarchical_layout() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("MyProject", "/tmp/my", "main")
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("projects/MyProject");
        let tasks_dir = project_dir.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();

        // Write project.json
        let meta = SyncProjectMeta {
            name: "MyProject".to_string(),
            default_branch: "main".to_string(),
        };
        fs::write(
            project_dir.join("project.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();

        // Write task file
        let sync_task = SyncTask {
            id: "task-uuid-1".to_string(),
            title: "synced task".to_string(),
            description: "from another machine".to_string(),
            status: "pending".to_string(),
            mode: "supervised".to_string(),
            sort_order: 1,
            branch: None,
            base: None,
            push_mode: "pr".to_string(),
            review_loop: false,
            created_at: "2026-03-15T00:00:00Z".to_string(),
            updated_at: "2026-03-15T00:00:00Z".to_string(),
            started_at: None,
            completed_at: None,
            input_tokens: 100,
            output_tokens: 200,
            pr_url: None,
            ci_status: None,
            subtasks: vec![],
        };
        fs::write(
            tasks_dir.join("task-uuid-1.json"),
            serde_json::to_string_pretty(&sync_task).unwrap(),
        )
        .unwrap();

        let result = import_state(&store, dir.path()).unwrap();
        assert_eq!(result.projects_synced, 1);
        assert_eq!(result.tasks_synced, 1);
        assert!(result.skipped_projects.is_empty());

        let tasks = store.list_tasks_for_project(&project.id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "synced task");
        assert_eq!(tasks[0].id, "task-uuid-1");
    }

    #[test]
    fn import_legacy_flat_format() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("MyProject", "/tmp/my", "main")
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        fs::create_dir_all(&projects_dir).unwrap();

        // Write legacy flat file
        let legacy = LegacySyncProject {
            name: "MyProject".to_string(),
            default_branch: "main".to_string(),
            tasks: vec![SyncTask {
                id: "task-uuid-1".to_string(),
                title: "legacy task".to_string(),
                description: "from old format".to_string(),
                status: "pending".to_string(),
                mode: "supervised".to_string(),
                sort_order: 1,
                branch: None,
                base: None,
                push_mode: "pr".to_string(),
                review_loop: false,
                created_at: "2026-03-15T00:00:00Z".to_string(),
                updated_at: "2026-03-15T00:00:00Z".to_string(),
                started_at: None,
                completed_at: None,
                input_tokens: 100,
                output_tokens: 200,
                pr_url: None,
                ci_status: None,
                subtasks: vec![],
            }],
        };

        fs::write(
            projects_dir.join("MyProject.json"),
            serde_json::to_string_pretty(&legacy).unwrap(),
        )
        .unwrap();

        let result = import_state(&store, dir.path()).unwrap();
        assert_eq!(result.projects_synced, 1);
        assert_eq!(result.tasks_synced, 1);

        let tasks = store.list_tasks_for_project(&project.id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "legacy task");
    }

    #[test]
    fn import_skips_unregistered_projects() {
        let store = Store::open_in_memory().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("projects/UnknownProject");
        fs::create_dir_all(project_dir.join("tasks")).unwrap();

        let meta = SyncProjectMeta {
            name: "UnknownProject".to_string(),
            default_branch: "main".to_string(),
        };
        fs::write(
            project_dir.join("project.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        let result = import_state(&store, dir.path()).unwrap();
        assert_eq!(result.projects_synced, 0);
        assert_eq!(result.skipped_projects, vec!["UnknownProject"]);
    }

    #[test]
    fn import_updates_existing_tasks() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("P", "/tmp/p", "main").unwrap();
        let task = store
            .create_task(
                &project.id,
                "original",
                "old desc",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let project_dir = dir.path().join("projects/P");
        let tasks_dir = project_dir.join("tasks");
        fs::create_dir_all(&tasks_dir).unwrap();

        let meta = SyncProjectMeta {
            name: "P".to_string(),
            default_branch: "main".to_string(),
        };
        fs::write(
            project_dir.join("project.json"),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();

        let sync_task = SyncTask {
            id: task.id.clone(),
            title: "updated title".to_string(),
            description: "new desc".to_string(),
            status: "done".to_string(),
            mode: "supervised".to_string(),
            sort_order: 1,
            branch: None,
            base: None,
            push_mode: "pr".to_string(),
            review_loop: false,
            created_at: task.created_at.clone(),
            updated_at: "2026-03-15T12:00:00Z".to_string(),
            started_at: None,
            completed_at: Some("2026-03-15T12:00:00Z".to_string()),
            input_tokens: 500,
            output_tokens: 1000,
            pr_url: Some("https://github.com/example/pr/1".to_string()),
            ci_status: None,
            subtasks: vec![],
        };
        fs::write(
            tasks_dir.join(format!("{}.json", task.id)),
            serde_json::to_string_pretty(&sync_task).unwrap(),
        )
        .unwrap();

        let result = import_state(&store, dir.path()).unwrap();
        assert_eq!(result.tasks_synced, 1);

        let updated = store.get_task(&task.id).unwrap();
        assert_eq!(updated.title, "updated title");
        assert_eq!(updated.description, "new desc");
        assert_eq!(updated.input_tokens, 500);
        assert_eq!(updated.output_tokens, 1000);
        assert!(updated.pr_url.is_some());
    }

    #[test]
    fn round_trip_export_import() {
        // Create state on "machine A"
        let store_a = Store::open_in_memory().unwrap();
        let project_a = store_a
            .create_project("RoundTrip", "/tmp/rt", "main")
            .unwrap();
        let task_a = store_a
            .create_task(
                &project_a.id,
                "task-rt",
                "round trip test",
                TaskMode::Autonomous,
                Some("feat/rt"),
                Some("develop"),
                PushMode::Push,
                true,
            )
            .unwrap();
        store_a
            .create_subtask(&task_a.id, "step-rt", "substep")
            .unwrap();

        // Export from "machine A"
        let dir = tempfile::tempdir().unwrap();
        export_state(&store_a, dir.path()).unwrap();

        // Verify hierarchical layout was created
        assert!(dir.path().join("projects/RoundTrip/project.json").exists());
        assert!(
            dir.path()
                .join(format!("projects/RoundTrip/tasks/{}.json", task_a.id))
                .exists()
        );

        // Import into "machine B"
        let store_b = Store::open_in_memory().unwrap();
        let _project_b = store_b
            .create_project("RoundTrip", "/home/user/rt", "main")
            .unwrap();

        let result = import_state(&store_b, dir.path()).unwrap();
        assert_eq!(result.projects_synced, 1);
        assert_eq!(result.tasks_synced, 1);
        assert_eq!(result.subtasks_synced, 1);

        // Verify the task was imported with same ID but different project_id
        let imported_task = store_b.get_task(&task_a.id).unwrap();
        assert_eq!(imported_task.title, "task-rt");
        assert_eq!(imported_task.description, "round trip test");
        assert_eq!(imported_task.mode, TaskMode::Autonomous);
        assert_eq!(imported_task.branch.as_deref(), Some("feat/rt"));
        assert_eq!(imported_task.base.as_deref(), Some("develop"));
        assert_eq!(imported_task.push_mode, PushMode::Push);
        assert!(imported_task.review_loop);
        // project_id should be the local project's ID, not the original
        assert_ne!(imported_task.project_id, project_a.id);

        let imported_subtasks = store_b.list_subtasks_for_task(&task_a.id).unwrap();
        assert_eq!(imported_subtasks.len(), 1);
        assert_eq!(imported_subtasks[0].title, "step-rt");
    }
}
