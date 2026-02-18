mod config;
mod pty;
mod session;
mod session_host;
mod skills;
mod store;
mod tui;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "claustre", about = "Orchestrate multiple Claude Code sessions")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the TUI dashboard (default)
    Dashboard,
    /// Initialize claustre config directory
    Init,
    /// Add a project to claustre
    AddProject {
        /// Display name for the project
        name: String,
        /// Path to the git repository
        #[arg(default_value = ".")]
        path: String,
    },
    /// Add a task to a project
    AddTask {
        /// Project name
        project: String,
        /// Task title
        title: String,
        /// Task description
        #[arg(short, long, default_value = "")]
        description: String,
        /// Task mode: autonomous or supervised
        #[arg(short, long, default_value = "supervised")]
        mode: String,
    },
    /// List projects
    ListProjects,
    /// List tasks for a project
    ListTasks {
        /// Project name
        project: String,
    },
    /// Show stats for a project
    Stats {
        /// Project name
        project: String,
    },
    /// Remove a project from claustre
    RemoveProject {
        /// Project name
        project: String,
    },
    /// Export tasks for a project to .claustre/tasks.json in the project repo
    Export {
        /// Project name
        project: String,
        /// Output path (default: <repo>/.claustre/tasks.json)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Manage agent skills (skills.sh integration)
    Skills {
        #[command(subcommand)]
        action: Option<SkillsAction>,
    },
    /// Run autonomous task chain for a session (blocking loop)
    FeedNext {
        /// Session ID to feed tasks to
        #[arg(long)]
        session_id: String,
    },
    /// Update session state from a Stop hook (set idle, optionally transition task)
    SessionUpdate {
        /// Session ID to update
        #[arg(long)]
        session_id: String,
        /// PR URL — if provided, transitions the in-progress task to `in_review`
        #[arg(long)]
        pr_url: Option<String>,
        /// Cumulative input tokens from this session's conversation
        #[arg(long)]
        input_tokens: Option<i64>,
        /// Cumulative output tokens from this session's conversation
        #[arg(long)]
        output_tokens: Option<i64>,
        /// Signal that the user resumed interaction — transitions `in_review` back to working
        #[arg(long)]
        resumed: bool,
    },
    /// Run a session host (PTY owner + socket server, detached from TUI)
    SessionHost {
        /// Session ID
        #[arg(long)]
        session_id: String,
        /// Working directory (worktree path)
        #[arg(long)]
        worktree_path: String,
        /// Command to run in the PTY (everything after --)
        #[arg(last = true)]
        cmd: Vec<String>,
    },
}

#[derive(Subcommand)]
enum SkillsAction {
    /// Search for skills on skills.sh
    Find {
        /// Search query
        query: String,
    },
    /// Add a skill package
    Add {
        /// Package (e.g. owner/repo or owner/repo@skill)
        package: String,
        /// Install to a project instead of globally
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Remove an installed skill
    Remove {
        /// Skill name to remove
        name: String,
        /// Remove from project instead of global
        #[arg(short, long)]
        project: Option<String>,
    },
    /// Update all installed skills
    Update,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Dashboard) {
        Commands::Init => {
            config::ensure_dirs()?;
            println!("claustre initialized at ~/.claustre/");
            Ok(())
        }
        Commands::AddProject { name, path } => {
            config::ensure_dirs()?;
            let store = store::Store::open()?;
            store.migrate()?;
            let abs_path =
                std::fs::canonicalize(&path).with_context(|| format!("invalid path: {path}"))?;
            let abs_str = abs_path.to_str().context("path contains invalid UTF-8")?;
            let project = store.create_project(&name, abs_str)?;
            println!("Added project '{}' ({})", project.name, project.repo_path);
            Ok(())
        }
        Commands::AddTask {
            project,
            title,
            description,
            mode,
        } => {
            config::ensure_dirs()?;
            let store = store::Store::open()?;
            store.migrate()?;
            let proj = find_project_by_name(&store, &project)?;
            let task_mode: store::TaskMode = mode.parse().map_err(anyhow::Error::msg)?;
            let task = store.create_task(&proj.id, &title, &description, task_mode)?;
            println!(
                "Created task '{}' ({}) for project '{}'",
                task.title,
                task.mode.as_str(),
                proj.name
            );
            Ok(())
        }
        Commands::ListProjects => {
            config::ensure_dirs()?;
            let store = store::Store::open()?;
            store.migrate()?;
            let projects = store.list_projects()?;
            if projects.is_empty() {
                println!("No projects. Use `claustre add-project <name> <path>` to add one.");
            } else {
                for p in &projects {
                    let sessions = store.list_active_sessions_for_project(&p.id)?;
                    let tasks = store.list_tasks_for_project(&p.id)?;
                    let pending = tasks
                        .iter()
                        .filter(|t| t.status == store::TaskStatus::Pending)
                        .count();
                    let in_review = tasks
                        .iter()
                        .filter(|t| t.status == store::TaskStatus::InReview)
                        .count();
                    println!(
                        "  {} — {} sessions, {} pending, {} in review ({})",
                        p.name,
                        sessions.len(),
                        pending,
                        in_review,
                        p.repo_path,
                    );
                }
            }
            Ok(())
        }
        Commands::ListTasks { project } => {
            config::ensure_dirs()?;
            let store = store::Store::open()?;
            store.migrate()?;
            let proj = find_project_by_name(&store, &project)?;
            let tasks = store.list_tasks_for_project(&proj.id)?;
            if tasks.is_empty() {
                println!("No tasks for '{}'.", proj.name);
            } else {
                for t in &tasks {
                    println!(
                        "  {} {} [{}] ({})",
                        t.status.symbol(),
                        t.title,
                        t.status.as_str(),
                        t.mode.as_str(),
                    );
                }
            }
            Ok(())
        }
        Commands::Stats { project } => {
            config::ensure_dirs()?;
            let store = store::Store::open()?;
            store.migrate()?;
            let proj = find_project_by_name(&store, &project)?;
            let stats = store.project_stats(&proj.id)?;
            println!("Stats for '{}':", proj.name);
            println!("  Total tasks:     {}", stats.total_tasks);
            println!("  Completed:       {}", stats.completed_tasks);
            println!("  Sessions run:    {}", stats.total_sessions);
            println!("  Total time:      {}", stats.formatted_time());
            println!("  Tokens used:     {}", stats.total_tokens());
            println!("  Avg task time:   {}", stats.formatted_avg_task_time());
            Ok(())
        }
        Commands::RemoveProject { project } => {
            config::ensure_dirs()?;
            let store = store::Store::open()?;
            store.migrate()?;
            let proj = find_project_by_name(&store, &project)?;
            store.delete_project(&proj.id)?;
            println!("Removed project '{}'", proj.name);
            Ok(())
        }
        Commands::Export { project, output } => {
            config::ensure_dirs()?;
            let store = store::Store::open()?;
            store.migrate()?;
            let proj = find_project_by_name(&store, &project)?;
            let tasks = store.list_tasks_for_project(&proj.id)?;
            let stats = store.project_stats(&proj.id)?;

            let export = serde_json::json!({
                "project": proj.name,
                "repo_path": proj.repo_path,
                "exported_at": chrono::Utc::now().to_rfc3339(),
                "stats": {
                    "total_tasks": stats.total_tasks,
                    "completed_tasks": stats.completed_tasks,
                    "total_sessions": stats.total_sessions,
                    "total_time": stats.formatted_time(),
                    "total_tokens": stats.total_tokens(),
                },
                "tasks": tasks,
            });

            let json = serde_json::to_string_pretty(&export)?;

            let output_path = if let Some(ref out) = output {
                std::path::PathBuf::from(out)
            } else {
                let claustre_dir = Path::new(&proj.repo_path).join(".claustre");
                fs::create_dir_all(&claustre_dir)?;
                claustre_dir.join("tasks.json")
            };

            fs::write(&output_path, &json)?;
            println!(
                "Exported {} tasks to {}",
                tasks.len(),
                output_path.display()
            );
            Ok(())
        }
        Commands::Skills { action } => match action {
            None => {
                println!("Global skills:");
                let global = skills::list_skills(true, None)?;
                if global.is_empty() {
                    println!("  (none)");
                } else {
                    for s in &global {
                        println!("  {} — {}", s.name, s.path);
                        if !s.agents.is_empty() {
                            println!("    Agents: {}", s.agents.join(", "));
                        }
                    }
                }
                Ok(())
            }
            Some(SkillsAction::Find { query }) => {
                let results = skills::find_skills(&query)?;
                if results.is_empty() {
                    println!("No skills found for '{query}'");
                } else {
                    for r in &results {
                        println!("  {} — {}", r.package, r.url);
                    }
                }
                Ok(())
            }
            Some(SkillsAction::Add { package, project }) => {
                let (global, project_path) = if let Some(ref proj_name) = project {
                    let store = store::Store::open()?;
                    store.migrate()?;
                    let proj = find_project_by_name(&store, proj_name)?;
                    (false, Some(proj.repo_path))
                } else {
                    (true, None)
                };

                let msg = skills::add_skill(&package, global, project_path.as_deref())?;
                println!("{msg}");
                Ok(())
            }
            Some(SkillsAction::Remove { name, project }) => {
                let (global, project_path) = if let Some(ref proj_name) = project {
                    let store = store::Store::open()?;
                    store.migrate()?;
                    let proj = find_project_by_name(&store, proj_name)?;
                    (false, Some(proj.repo_path))
                } else {
                    (true, None)
                };

                let msg = skills::remove_skill(&name, global, project_path.as_deref())?;
                println!("{msg}");
                Ok(())
            }
            Some(SkillsAction::Update) => {
                let msg = skills::update_skills()?;
                println!("{msg}");
                Ok(())
            }
        },
        Commands::FeedNext { session_id } => run_feed_next(&session_id),
        Commands::SessionUpdate {
            session_id,
            pr_url,
            input_tokens,
            output_tokens,
            resumed,
        } => {
            let store = store::Store::open()?;
            store.migrate()?;

            // Read Claude's task progress from tmp file (if it exists)
            if let Ok(progress_path) = config::session_progress_file(&session_id)
                && progress_path.exists()
                && let Ok(content) = fs::read_to_string(&progress_path)
                && let Ok(items) = serde_json::from_str::<Vec<store::ClaudeProgressItem>>(&content)
            {
                let _ = store.update_session_progress(&session_id, &items);
            }

            // Update token usage on the working task (cumulative replacement, not additive)
            if let (Some(inp), Some(out)) = (input_tokens, output_tokens)
                && let Some(task) = store.working_task_for_session(&session_id)?
            {
                let _ = store.set_task_usage(&task.id, inp, out);
            }

            // If a PR URL was provided, transition the working task and mark session done
            if let Some(ref url) = pr_url
                && let Some(task) = store.working_task_for_session(&session_id)?
            {
                // Check if this is the same PR we already know about (e.g. stop hook
                // re-firing after a user-prompt --resumed cycle). Only notify once
                // per distinct PR URL to avoid notification spam.
                let is_new_pr = task.pr_url.as_deref() != Some(url.as_str());

                store.update_task_pr_url(&task.id, url)?;
                store.update_task_status(&task.id, store::TaskStatus::InReview)?;
                store.update_session_status(&session_id, store::ClaudeStatus::Done, "")?;

                if is_new_pr {
                    let cfg = config::load()?;
                    if cfg.notifications.enabled {
                        cfg.notifications.notify(&task.title, Some(url));
                    }
                }
            } else if resumed && let Some(task) = store.in_review_task_for_session(&session_id)? {
                // User resumed interaction on an in_review/conflict task — transition back
                store.update_task_status(&task.id, store::TaskStatus::Working)?;
                store.update_session_status(
                    &session_id,
                    store::ClaudeStatus::Working,
                    &format!("Resumed: {}", task.title),
                )?;
            } else if store.working_task_for_session(&session_id)?.is_none() {
                // No working task — session is truly idle (e.g. supervised session
                // where user hasn't assigned a task yet, or task was already completed)
                store.update_session_status(&session_id, store::ClaudeStatus::Idle, "")?;
            }
            // Otherwise, there's a working task with no PR yet — keep session
            // status as "working" since Claude is still actively processing.

            Ok(())
        }
        Commands::SessionHost {
            session_id,
            worktree_path,
            cmd,
        } => session_host::run(&session_id, &cmd, &worktree_path),
        Commands::Dashboard => {
            config::ensure_dirs()?;
            let store = store::Store::open()?;
            store.migrate()?;

            // Clean up socket/PID files from crashed session-hosts
            let _ = config::cleanup_stale_sockets();

            // Run TUI (blocking)
            tui::run(store)
        }
    }
}

/// Check usage cache for rate limit. Returns true if usage is too high to proceed.
#[expect(
    clippy::similar_names,
    reason = "5h and 7d are distinct domain-specific window labels"
)]
fn is_rate_limited_from_cache() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let cache_path = home.join(".claude/statusline-cache.json");
    let Ok(content) = fs::read_to_string(&cache_path) else {
        return false;
    };
    let Ok(cache) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let pct_5h = cache
        .get("pct5h")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let pct_7d = cache
        .get("pct7d")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    pct_5h >= 80.0 || pct_7d >= 80.0
}

/// Blocking loop that feeds autonomous tasks to a Claude session.
///
/// For each task: builds the prompt (including subtasks if any), runs Claude as a
/// blocking subprocess, then checks whether the Stop hook transitioned the task.
/// Continues to the next autonomous task until none remain or rate limited.
fn run_feed_next(session_id: &str) -> Result<()> {
    let store = store::Store::open()?;
    store.migrate()?;

    loop {
        // Check rate limits from the shared cache
        if is_rate_limited_from_cache() {
            eprintln!("feed-next: rate limited (>=80% usage), stopping");
            break;
        }

        // Find the current or next task to work on
        let task = if let Some(t) = store.working_task_for_session(session_id)? {
            // Resume a working task (e.g. after restart)
            t
        } else if let Some(t) = store.in_review_task_for_session(session_id)? {
            // Previous task completed or has conflicts — look for next
            let _ = t; // acknowledged
            match store.next_pending_task_for_session(session_id)? {
                Some(next) => next,
                None => break,
            }
        } else {
            // No working or in-review task — find next pending
            match store.next_pending_task_for_session(session_id)? {
                Some(next) => next,
                None => break,
            }
        };

        // Mark task working if it's still pending
        if task.status == store::TaskStatus::Pending {
            store.assign_task_to_session(&task.id, session_id)?;
            store.update_task_status(&task.id, store::TaskStatus::Working)?;
            store.update_session_status(
                session_id,
                store::ClaudeStatus::Working,
                &format!("Starting: {}", task.title),
            )?;
        }

        // Build prompt: if task has subtasks, concatenate them all into an ordered list
        let subtasks = store.list_subtasks_for_task(&task.id)?;
        let prompt = if subtasks.is_empty() {
            format!(
                "{}{}{}",
                task.description,
                session::AUTONOMOUS_SUFFIX,
                session::COMPLETION_INSTRUCTIONS
            )
        } else {
            use std::fmt::Write;
            let mut p = format!("# {}\n\n{}\n\n## Steps\n\n", task.title, task.description);
            for (i, st) in subtasks.iter().enumerate() {
                let _ = writeln!(p, "{}. **{}**: {}", i + 1, st.title, st.description);
            }
            p.push_str(session::AUTONOMOUS_SUFFIX);
            p.push_str(session::COMPLETION_INSTRUCTIONS);
            p
        };

        // Run Claude as a blocking subprocess
        eprintln!("feed-next: running task '{}'", task.title);
        let status = std::process::Command::new("claude")
            .arg(&prompt)
            .env("CLAUDE_CODE_TASK_LIST_ID", session_id)
            .status()
            .context("failed to run claude")?;

        if !status.success() {
            eprintln!(
                "feed-next: claude exited with status {}, stopping",
                status.code().unwrap_or(-1)
            );
            break;
        }

        // After Claude exits, the Stop hook has already fired.
        // Re-read task from DB to check its state.
        let task = store.get_task(&task.id)?;
        if task.status == store::TaskStatus::Working {
            // Stop hook didn't find a PR — mark in_review as best-effort fallback
            store.update_task_status(&task.id, store::TaskStatus::InReview)?;
        }

        // Mark subtasks done if the task was completed
        if !subtasks.is_empty() {
            for st in &subtasks {
                if st.status != store::TaskStatus::Done {
                    store.update_subtask_status(&st.id, store::TaskStatus::Done)?;
                }
            }
        }

        // Continue loop — will check for next pending task at top
    }

    eprintln!("feed-next: no more tasks, exiting");
    Ok(())
}

fn find_project_by_name(store: &store::Store, name: &str) -> Result<store::Project> {
    let projects = store.list_projects()?;
    projects
        .into_iter()
        .find(|p| p.name == name)
        .with_context(|| format!("project '{name}' not found"))
}
