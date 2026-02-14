mod config;
mod mcp;
mod session;
mod skills;
mod store;
mod tui;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tokio::sync::Mutex;

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
    /// Bridge stdin/stdout to the MCP Unix socket (used by Claude Code)
    McpBridge,
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

#[tokio::main]
async fn main() -> Result<()> {
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
            let task_mode: store::TaskMode = mode.parse().map_err(|e: String| {
                anyhow::anyhow!("{e}. expected 'autonomous' or 'supervised'")
            })?;
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
            println!("  Total cost:      ${:.2}", stats.total_cost);
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
                    "total_cost": stats.total_cost,
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
        Commands::McpBridge => mcp::run_bridge().await,
        Commands::Dashboard => {
            // If not inside Zellij, relaunch inside a new Zellij session
            if std::env::var("ZELLIJ_SESSION_NAME").is_err() {
                return relaunch_in_zellij();
            }

            config::ensure_dirs()?;
            let cfg = config::load()?;
            let store = store::Store::open()?;
            store.migrate()?;

            // Create a second store connection for the MCP server
            let mcp_store = store::Store::open()?;
            let shared_store: mcp::SharedStore = Arc::new(Mutex::new(mcp_store));

            // Build notification callback from config
            let notify: Option<mcp::NotifyFn> = if cfg.notifications.enabled {
                let notif_config = cfg.notifications;
                Some(Arc::new(move |task_title: &str| {
                    notif_config.notify(task_title);
                }))
            } else {
                None
            };

            // Start MCP server in background
            tokio::spawn(async move {
                if let Err(e) = mcp::start_server(shared_store, notify).await {
                    tracing::error!("MCP server error: {}", e);
                }
            });

            // Run TUI (blocking)
            tui::run(store)
        }
    }
}

/// Relaunch claustre inside a Zellij session.
/// If a "claustre" session already exists, attach to it.
/// Otherwise, create a new session with a layout that runs claustre.
fn relaunch_in_zellij() -> Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    let exe = std::env::current_exe().context("failed to determine claustre executable path")?;
    let exe_str = exe
        .to_str()
        .context("executable path contains invalid UTF-8")?;

    // Check if a live "claustre" session already exists.
    // Dead sessions show "(EXITED" in the output — skip those.
    let session_alive = Command::new("zellij")
        .args(["list-sessions", "--no-formatting"])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.starts_with("claustre ") && !l.contains("(EXITED"))
        });

    if session_alive {
        let err = Command::new("zellij").args(["attach", "claustre"]).exec();
        bail!("failed to attach to Zellij session 'claustre': {err}");
    }

    // Clean up any dead "claustre" session so the name is available
    let _ = Command::new("zellij")
        .args(["delete-session", "claustre"])
        .output();

    // Create a temporary layout that launches claustre with tab/status bars
    let layout = format!(
        "\
layout {{
    default_tab_template {{
        pane size=1 borderless=true {{
            plugin location=\"compact-bar\"
        }}
        children
        pane size=2 borderless=true {{
            plugin location=\"status-bar\"
        }}
    }}
    tab name=\"claustre\" {{
        pane command=\"{exe_str}\"
    }}
}}
"
    );
    let layout_path = std::env::temp_dir().join("claustre-layout.kdl");
    fs::write(&layout_path, &layout).context("failed to write temporary Zellij layout")?;
    let layout_str = layout_path
        .to_str()
        .context("temp path contains invalid UTF-8")?;

    let err = Command::new("zellij")
        .args([
            "--session",
            "claustre",
            "--new-session-with-layout",
            layout_str,
        ])
        .exec();
    bail!("failed to start Zellij: {err}");
}

fn find_project_by_name(store: &store::Store, name: &str) -> Result<store::Project> {
    let projects = store.list_projects()?;
    projects
        .into_iter()
        .find(|p| p.name == name)
        .with_context(|| format!("project '{name}' not found"))
}
