//! CLI entry point for claustre.
//!
//! Parses subcommands via clap and dispatches to the TUI dashboard,
//! session management, autonomous task chains, or skill operations.

mod config;
mod pty;
mod scanner;
mod session;
mod session_host;
mod skills;
mod store;
mod tui;
mod update;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

fn open_store() -> Result<store::Store> {
    config::ensure_dirs()?;
    let store = store::Store::open()?;
    store.migrate()?;
    Ok(store)
}

#[derive(Parser)]
#[command(
    name = "claustre",
    about = "Orchestrate multiple Claude Code sessions",
    version = update::VERSION,
    before_help = concat!("claustre ", env!("CLAUSTRE_VERSION")),
)]
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
        /// Launch Claude with --remote
        #[arg(long)]
        remote: bool,
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
    /// Monitor PR comments and implement valid review feedback in a loop
    ReviewLoop {
        /// Session ID whose task's PR to monitor
        #[arg(long)]
        session_id: String,
    },
    /// Verify the binary is functional (used by auto-update smoke test)
    HealthCheck,
    /// Roll back to the previous binary version after a bad auto-update
    Rollback,
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
            anyhow::ensure!(!name.trim().is_empty(), "project name must not be empty");
            let store = open_store()?;
            let abs_path =
                std::fs::canonicalize(&path).with_context(|| format!("invalid path: {path}"))?;
            let abs_str = abs_path.to_str().context("path contains invalid UTF-8")?;
            let default_branch = detect_default_branch(abs_str);
            let project = store.create_project(&name, abs_str, &default_branch)?;
            println!(
                "Added project '{}' ({}) [branch: {}]",
                project.name, project.repo_path, project.default_branch
            );
            Ok(())
        }
        Commands::AddTask {
            project,
            title,
            description,
            mode,
        } => {
            anyhow::ensure!(!title.trim().is_empty(), "task title must not be empty");
            let store = open_store()?;
            let proj = find_project_by_name(&store, &project)?;
            let task_mode: store::TaskMode = mode.parse().map_err(|_| {
                anyhow::anyhow!("invalid task mode '{mode}': expected 'autonomous' or 'supervised'")
            })?;
            let task = store.create_task(
                &proj.id,
                &title,
                &description,
                task_mode,
                None,
                store::PushMode::Pr,
                false,
            )?;
            println!(
                "Created task '{}' ({}) for project '{}'",
                task.title,
                task.mode.as_str(),
                proj.name
            );
            Ok(())
        }
        Commands::ListProjects => {
            let store = open_store()?;
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
            let store = open_store()?;
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
            let store = open_store()?;
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
            let store = open_store()?;
            let proj = find_project_by_name(&store, &project)?;
            store.delete_project(&proj.id)?;
            println!("Removed project '{}'", proj.name);
            Ok(())
        }
        Commands::Export { project, output } => {
            let store = open_store()?;
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
                    let store = open_store()?;
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
                    let store = open_store()?;
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
        Commands::FeedNext { session_id, remote } => run_feed_next(&session_id, remote),
        Commands::SessionUpdate {
            session_id,
            pr_url,
            input_tokens,
            output_tokens,
            resumed,
        } => {
            let store = open_store()?;

            // Read Claude's task progress from tmp file (if it exists)
            if let Ok(progress_path) = config::session_progress_file(&session_id)
                && progress_path.exists()
                && let Ok(content) = fs::read_to_string(&progress_path)
                && let Ok(items) = serde_json::from_str::<Vec<store::ClaudeProgressItem>>(&content)
            {
                let _ = store.update_session_progress(&session_id, &items);
            }

            // Find the active task for this session. A hook firing proves Claude is
            // still running, so `interrupted` tasks count as active (claustre was
            // restarted but the session-host / Claude process survived).
            let active_task = store
                .working_task_for_session(&session_id)?
                .or(store.interrupted_task_for_session(&session_id)?);

            // Update token usage on the active task (cumulative replacement, not additive)
            if let (Some(inp), Some(out)) = (input_tokens, output_tokens)
                && let Some(ref task) = active_task
            {
                let _ = store.set_task_usage(&task.id, inp, out);
            }

            // If a PR URL was provided, transition the active task and mark session done
            if let Some(ref url) = pr_url
                && let Some(ref task) = active_task
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
            } else if resumed
                && let Some(task) = store
                    .in_review_task_for_session(&session_id)?
                    .or(store.interrupted_task_for_session(&session_id)?)
            {
                // User resumed interaction on an in_review/conflict/interrupted task
                store.update_task_status(&task.id, store::TaskStatus::Working)?;
                store.update_session_status(
                    &session_id,
                    store::ClaudeStatus::Working,
                    &format!("Resumed: {}", task.title),
                )?;
            } else if let Some(ref task) = active_task {
                // Hook fired with an interrupted task but no PR and not --resumed.
                // The hook proves Claude is active, so restore to working.
                if task.status == store::TaskStatus::Interrupted {
                    store.update_task_status(&task.id, store::TaskStatus::Working)?;
                    store.update_session_status(
                        &session_id,
                        store::ClaudeStatus::Working,
                        &format!("Restored: {}", task.title),
                    )?;
                }
                // Otherwise there's a working task with no PR — keep session as-is.
            } else {
                // No active task — session is truly idle (e.g. supervised session
                // where user hasn't assigned a task yet, or task was already completed)
                store.update_session_status(&session_id, store::ClaudeStatus::Idle, "")?;
            }

            Ok(())
        }
        Commands::SessionHost {
            session_id,
            worktree_path,
            cmd,
        } => session_host::run(&session_id, &cmd, &worktree_path),
        Commands::ReviewLoop { session_id } => run_review_loop(&session_id),
        Commands::HealthCheck => {
            let store = open_store()?;
            store.health_check()?;
            println!("ok {}", crate::update::VERSION);
            Ok(())
        }
        Commands::Rollback => crate::update::rollback(),
        Commands::Dashboard => {
            // Auto-update before opening TUI (if configured)
            let cfg = config::load().unwrap_or_default();
            if cfg.auto_update {
                match update::check_and_update() {
                    update::UpdateCheckResult::Updated { new_version } => {
                        eprintln!("Updated to {new_version}, restarting...");
                        // Re-exec the new binary so the TUI starts with the fresh version
                        let exe =
                            std::env::current_exe().context("could not determine executable")?;
                        let args: Vec<String> = std::env::args().collect();
                        let err = exec_process(&exe, &args);
                        // exec_process only returns on error
                        anyhow::bail!("failed to re-exec after update: {err}");
                    }
                    update::UpdateCheckResult::Available {
                        new_version,
                        reason,
                    } => {
                        eprintln!("Update to {new_version} available but install failed: {reason}");
                    }
                    update::UpdateCheckResult::UpToDate
                    | update::UpdateCheckResult::Failed { .. } => {}
                }
            }

            let store = open_store()?;

            // Clean up socket/PID files from crashed session-hosts
            let _ = config::cleanup_stale_sockets();

            // Install panic hook to restore terminal on panics
            let default_hook = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    crossterm::event::DisableMouseCapture,
                    crossterm::event::DisableBracketedPaste,
                );
                ratatui::restore();
                default_hook(info);
            }));

            // Run TUI (blocking)
            tui::run(store)
        }
    }
}

const RATE_LIMIT_THRESHOLD: f64 = 80.0;

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
    let pct_5h = cache["data"]["pct5h"].as_f64().unwrap_or(0.0);
    let pct_7d = cache["data"]["pct7d"].as_f64().unwrap_or(0.0);
    pct_5h >= RATE_LIMIT_THRESHOLD || pct_7d >= RATE_LIMIT_THRESHOLD
}

/// Blocking loop that feeds autonomous tasks to a Claude session.
///
/// For each task: builds the prompt (including subtasks if any), runs Claude as a
/// blocking subprocess, then checks whether the Stop hook transitioned the task.
/// Continues to the next autonomous task until none remain or rate limited.
fn run_feed_next(session_id: &str, remote: bool) -> Result<()> {
    let store = open_store()?;

    // Look up the project's default branch for PR target instructions
    let session = store.get_session(session_id)?;
    let project = store.get_project(&session.project_id)?;

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
        } else if let Some(t) = store.interrupted_task_for_session(session_id)? {
            // Resume an interrupted task (claustre restarted while task was active)
            t
        } else if store.in_review_task_for_session(session_id)?.is_some() {
            // Previous task completed or has conflicts — look for next
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

        // Mark task working if it's still pending or interrupted
        if task.status == store::TaskStatus::Pending {
            store.assign_task_to_session(&task.id, session_id)?;
            store.update_task_status(&task.id, store::TaskStatus::Working)?;
            store.update_session_status(
                session_id,
                store::ClaudeStatus::Working,
                &format!("Starting: {}", task.title),
            )?;
        } else if task.status == store::TaskStatus::Interrupted {
            store.update_task_status(&task.id, store::TaskStatus::Working)?;
            store.update_session_status(
                session_id,
                store::ClaudeStatus::Working,
                &format!("Resumed: {}", task.title),
            )?;
        }

        // Build prompt: if task has subtasks, concatenate them all into an ordered list
        let subtasks = store.list_subtasks_for_task(&task.id)?;
        let instructions =
            session::completion_instructions(&project.default_branch, task.push_mode);
        let prompt = if subtasks.is_empty() {
            format!(
                "{}{}{}",
                task.description,
                session::AUTONOMOUS_SUFFIX,
                instructions
            )
        } else {
            use std::fmt::Write;
            let mut p = format!("# {}\n\n{}\n\n## Steps\n\n", task.title, task.description);
            for (i, st) in subtasks.iter().enumerate() {
                let _ = writeln!(p, "{}. **{}**: {}", i + 1, st.title, st.description);
            }
            p.push_str(session::AUTONOMOUS_SUFFIX);
            p.push_str(&instructions);
            p
        };

        // Run Claude as a blocking subprocess
        eprintln!("feed-next: running task '{}'", task.title);
        let mut cmd = std::process::Command::new("claude");
        if remote {
            cmd.arg("--remote");
        }
        cmd.arg(&prompt);
        let status = cmd
            .env("CLAUDE_CODE_TASK_LIST_ID", session_id)
            .env("CLAUSTRE_SESSION", "1")
            .status()
            .context("failed to run claude")?;

        if !status.success() {
            let exit_info = match status.code() {
                Some(code) => format!("exit code {code}"),
                None => "terminated by signal".to_string(),
            };
            eprintln!("feed-next: claude exited with {exit_info}, stopping");
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

/// The review-loop prompt template. Tells Claude to fetch PR comments,
/// evaluate them adversarially, implement valid ones, and provide a summary.
const REVIEW_LOOP_PROMPT: &str = r#"You are reviewing PR comments on this branch. Follow these steps:

1. Run `gh pr view --json number,url --jq '.number'` to get the PR number.
2. Run `gh api repos/{owner}/{repo}/pulls/{number}/comments --jq '.[] | select(.in_reply_to_id == null) | {id: .id, path: .path, line: .line, body: .body, user: .user.login}'` to fetch review comments. Also run `gh api repos/{owner}/{repo}/pulls/{number}/reviews --jq '.[] | select(.state == "CHANGES_REQUESTED" or .state == "COMMENTED") | {id: .id, body: .body, user: .user.login, state: .state}'` to fetch review-level comments.
   - Derive {owner}/{repo} from `gh repo view --json nameWithOwner --jq .nameWithOwner`
3. For EACH comment, evaluate it adversarially:
   - Is this a valid, actionable code review comment?
   - Reject: nitpicks, pure style preferences without substance, comments that misunderstand the code, comments from bots
   - Accept: bug fixes, logic errors, missing edge cases, security issues, meaningful improvements
4. For each ACCEPTED comment:
   - Implement the requested change
   - Stage and commit with a message referencing the review comment
5. If any changes were made, push: `git push`
6. At the end, print a summary table:

## Review Loop Summary

| Comment | Author | Verdict | Reason |
|---------|--------|---------|--------|
| <brief description> | <user> | Accepted/Rejected | <WHY you accepted or rejected it> |

If there are no comments or no actionable comments, just say "No actionable review comments found."

IMPORTANT: This is an autonomous task. Do NOT ask the user for clarification. Make your best judgment and proceed."#;

/// Run a review loop: periodically check PR comments and implement valid feedback.
fn run_review_loop(session_id: &str) -> Result<()> {
    let store = open_store()?;
    let cfg = config::load()?;
    let poll_interval = std::time::Duration::from_secs(cfg.review_loop.poll_interval_secs);
    let prompt = cfg
        .review_loop
        .prompt
        .as_deref()
        .unwrap_or(REVIEW_LOOP_PROMPT);

    loop {
        // Find the in_review task for this session
        let task = store.in_review_task_for_session(session_id)?;
        let task = match task {
            Some(t) if t.pr_url.is_some() => t,
            Some(_) => {
                eprintln!("review-loop: task has no PR URL yet, waiting...");
                std::thread::sleep(poll_interval);
                continue;
            }
            None => {
                // Check if task is done — if so, exit
                let working = store.working_task_for_session(session_id)?;
                if working.is_some() {
                    eprintln!("review-loop: task still working, waiting...");
                    std::thread::sleep(poll_interval);
                    continue;
                }
                eprintln!("review-loop: no in_review task found, exiting");
                break;
            }
        };

        eprintln!("review-loop: checking PR comments for '{}'", task.title);

        // Run Claude with the review prompt
        let status = std::process::Command::new("claude")
            .arg(prompt)
            .env("CLAUSTRE_SESSION", "1")
            .status()
            .context("failed to run claude for review loop")?;

        if !status.success() {
            eprintln!(
                "review-loop: claude exited with status {}, will retry",
                status.code().unwrap_or(-1)
            );
        }

        // Check rate limits
        if is_rate_limited_from_cache() {
            eprintln!("review-loop: rate limited, stopping");
            break;
        }

        // Re-check task status — if it's no longer in_review (e.g. merged), stop
        let task = store.get_task(&task.id)?;
        if task.status == store::TaskStatus::Done {
            eprintln!("review-loop: task is done, exiting");
            break;
        }

        eprintln!(
            "review-loop: sleeping {}s before next check",
            poll_interval.as_secs()
        );
        std::thread::sleep(poll_interval);
    }

    Ok(())
}

/// Auto-detect the default branch of a git repo by querying `origin`.
/// Falls back to `"main"` if detection fails.
pub(crate) fn detect_default_branch(repo_path: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["-C", repo_path, "remote", "show", "origin"])
        .output();

    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if let Some(branch) = trimmed.strip_prefix("HEAD branch:") {
                return branch.trim().to_string();
            }
        }
    }

    "main".to_string()
}

/// Replace the current process with a new invocation of the given executable.
/// Uses Unix `execv` — only returns on error.
fn exec_process(exe: &Path, args: &[String]) -> std::io::Error {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_exe = CString::new(exe.as_os_str().as_bytes()).expect("executable path contains null");
    let c_args: Vec<CString> = args
        .iter()
        .map(|a| CString::new(a.as_bytes()).expect("argument contains null"))
        .collect();
    let c_arg_ptrs: Vec<&std::ffi::CStr> = c_args.iter().map(AsRef::as_ref).collect();

    // This replaces the process; it only returns on failure.
    nix_execv(&c_exe, &c_arg_ptrs)
}

/// Thin wrapper around `libc::execv` that returns an `io::Error` on failure.
fn nix_execv(exe: &std::ffi::CStr, args: &[&std::ffi::CStr]) -> std::io::Error {
    let ptrs: Vec<*const libc::c_char> = args
        .iter()
        .map(|a| a.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // SAFETY: `exe` and all `ptrs` entries are valid C strings; the array is null-terminated.
    unsafe {
        libc::execv(exe.as_ptr(), ptrs.as_ptr());
    }
    std::io::Error::last_os_error()
}

fn find_project_by_name(store: &store::Store, name: &str) -> Result<store::Project> {
    let projects = store.list_projects()?;
    projects
        .into_iter()
        .find(|p| p.name == name)
        .with_context(|| format!("project '{name}' not found"))
}
