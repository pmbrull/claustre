use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Result;

use crate::store::TaskStatus;

use super::{
    App, GitStatsResult, PrPollResult, PrStatus, SessionOpResult, ToastStyle, check_pr_status,
    compute_pane_sizes_for_resize, fetch_and_cache_usage, generate_ai_title, parse_git_diff_stat,
};

impl App {
    /// Spawn a background thread to fetch usage from the Anthropic OAuth API
    /// and write the result to the shared cache file.
    pub(super) fn spawn_usage_fetch(&self) {
        let flag = self.usage_fetch_in_progress.clone();
        flag.store(true, Ordering::SeqCst);

        std::thread::spawn(move || {
            let _result = fetch_and_cache_usage();
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Spawn a background thread to check for updates and auto-install if available.
    #[allow(dead_code)]
    pub(super) fn spawn_update_check(&self) {
        if self.update_check_in_progress.load(Ordering::Relaxed) {
            return;
        }
        let flag = self.update_check_in_progress.clone();
        flag.store(true, Ordering::Relaxed);
        let tx = self.update_tx.clone();

        std::thread::spawn(move || {
            let result = crate::update::check_and_update();
            let _ = tx.send(result);
            flag.store(false, Ordering::Relaxed);
        });
    }

    /// Periodically re-check for updates (every 30 minutes).
    /// Skips if an update was already found or a check is in progress.
    pub(super) fn maybe_poll_update_check(&mut self) {
        const UPDATE_POLL_INTERVAL: Duration = Duration::from_secs(30 * 60);

        if self.updated_version.is_some() || self.available_version.is_some() {
            return;
        }
        if self.last_update_check.elapsed() < UPDATE_POLL_INTERVAL {
            return;
        }
        self.last_update_check = std::time::Instant::now();
        self.spawn_update_check();
    }

    /// Drain update check results from the background thread.
    pub(super) fn poll_update_results(&mut self) {
        while let Ok(result) = self.update_rx.try_recv() {
            match result {
                crate::update::UpdateCheckResult::Updated { new_version } => {
                    self.updated_version = Some(new_version.clone());
                    self.show_toast(
                        format!("Updated to {new_version} — restart to apply"),
                        ToastStyle::Success,
                    );
                }
                crate::update::UpdateCheckResult::UpToDate => {}
                crate::update::UpdateCheckResult::Available {
                    new_version,
                    reason,
                } => {
                    self.available_version = Some(new_version);
                    self.show_toast(format!("Auto-update failed: {reason}"), ToastStyle::Error);
                }
                crate::update::UpdateCheckResult::Failed { reason } => {
                    self.show_toast(format!("Update check failed: {reason}"), ToastStyle::Error);
                }
            }
        }
    }

    /// Spawn a background thread to generate a title for a task via Claude Haiku.
    /// When the title is ready, it's sent through the channel and picked up on the next tick.
    pub(super) fn spawn_title_generation(&mut self, task_id: String, prompt: String) {
        self.pending_titles.insert(task_id.clone());
        let tx = self.title_tx.clone();
        std::thread::spawn(move || {
            let title = generate_ai_title(&prompt);
            let _ = tx.send((task_id, title));
        });
    }

    /// Drain background title results and update tasks in the DB.
    /// If any completed titles belong to autonomous tasks awaiting launch, launch them now.
    pub(super) fn poll_title_results(&mut self) -> Result<()> {
        while let Ok((task_id, title)) = self.title_rx.try_recv() {
            self.pending_titles.remove(&task_id);
            self.store.update_task_title(&task_id, &title)?;

            if let Some(project_id) = self.pending_auto_launch.remove(&task_id) {
                let task = self.store.get_task(&task_id)?;
                let branch_name = crate::session::generate_branch_name(&task.title);
                self.spawn_create_session(project_id, branch_name, task, false);
            }
        }
        Ok(())
    }

    /// Poll PR status for all `in_review` and `conflict` tasks that have a PR URL.
    /// Detects merges, new conflicts, and conflict resolution.
    /// Spawns a background thread every ~15 seconds.
    pub(super) fn maybe_poll_pr_merges(&mut self) {
        const PR_POLL_INTERVAL: Duration = Duration::from_secs(15);

        if self.last_pr_poll.elapsed() < PR_POLL_INTERVAL {
            return;
        }
        self.last_pr_poll = std::time::Instant::now();

        if self.pr_poll_in_progress.load(Ordering::SeqCst) {
            return;
        }

        let Ok(tasks) = self.store.list_in_review_tasks_with_pr() else {
            return;
        };
        if tasks.is_empty() {
            return;
        }

        // Collect task info for the background thread
        let check_list: Vec<_> = tasks
            .into_iter()
            .filter_map(|t| {
                let url = t.pr_url?;
                Some((t.id, t.session_id, url, t.title, t.status, t.ci_status))
            })
            .collect();

        if check_list.is_empty() {
            return;
        }

        let flag = self.pr_poll_in_progress.clone();
        flag.store(true, Ordering::SeqCst);
        let tx = self.pr_poll_tx.clone();

        std::thread::spawn(move || {
            for (task_id, session_id, pr_url, title, task_status, current_ci) in check_list {
                let pr_status = check_pr_status(&pr_url);

                // Derive CI status from the PR check result
                let new_ci = match pr_status {
                    PrStatus::CiRunning => Some(crate::store::CiStatus::Running),
                    PrStatus::CiPassed => Some(crate::store::CiStatus::Passed),
                    PrStatus::CiFailed => Some(crate::store::CiStatus::Failed),
                    _ => None,
                };

                // Send ci_status update if it changed
                if let Some(ci) = new_ci
                    && new_ci != current_ci
                {
                    let _ = tx.send(PrPollResult::CiStatusChanged {
                        task_id: task_id.clone(),
                        ci_status: ci,
                    });
                }

                // Handle task status transitions
                match pr_status {
                    PrStatus::Merged => {
                        let _ = tx.send(PrPollResult::Merged {
                            task_id,
                            session_id,
                            task_title: title,
                        });
                    }
                    PrStatus::Conflicting if task_status != TaskStatus::Conflict => {
                        let _ = tx.send(PrPollResult::Conflict {
                            task_id,
                            task_title: title,
                        });
                    }
                    PrStatus::CiFailed if task_status != TaskStatus::CiFailed => {
                        let _ = tx.send(PrPollResult::CiFailed {
                            task_id,
                            task_title: title,
                        });
                    }
                    PrStatus::Open | PrStatus::CiRunning | PrStatus::CiPassed
                        if task_status == TaskStatus::Conflict =>
                    {
                        let _ = tx.send(PrPollResult::ConflictResolved {
                            task_id,
                            task_title: title,
                        });
                    }
                    PrStatus::Open | PrStatus::CiRunning | PrStatus::CiPassed
                        if task_status == TaskStatus::CiFailed =>
                    {
                        let _ = tx.send(PrPollResult::CiRecovered {
                            task_id,
                            task_title: title,
                        });
                    }
                    _ => {}
                }
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Drain PR poll results and handle merges, conflicts, and conflict resolution.
    pub(super) fn poll_pr_merge_results(&mut self) -> Result<()> {
        while let Ok(result) = self.pr_poll_rx.try_recv() {
            match result {
                PrPollResult::Merged {
                    task_id,
                    session_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::Done)?;
                    if let Some(ref sid) = session_id {
                        self.spawn_teardown_session(sid.clone());
                    }
                    self.show_toast(
                        format!("PR merged — task done: {task_title}"),
                        ToastStyle::Success,
                    );
                }
                PrPollResult::Conflict {
                    task_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::Conflict)?;
                    self.show_toast(format!("PR has conflicts: {task_title}"), ToastStyle::Error);
                }
                PrPollResult::ConflictResolved {
                    task_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::InReview)?;
                    self.show_toast(
                        format!("Conflicts resolved: {task_title}"),
                        ToastStyle::Success,
                    );
                }
                PrPollResult::CiFailed {
                    task_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::CiFailed)?;
                    self.show_toast(format!("CI checks failed: {task_title}"), ToastStyle::Error);
                }
                PrPollResult::CiRecovered {
                    task_id,
                    task_title,
                } => {
                    self.store
                        .update_task_status(&task_id, crate::store::TaskStatus::InReview)?;
                    self.show_toast(
                        format!("CI checks passing: {task_title}"),
                        ToastStyle::Success,
                    );
                }
                PrPollResult::CiStatusChanged { task_id, ci_status } => {
                    self.store
                        .update_task_ci_status(&task_id, Some(ci_status))?;
                }
            }
        }
        Ok(())
    }

    /// Poll git diff stats for all active sessions every ~5 seconds.
    pub(super) fn maybe_poll_git_stats(&mut self) {
        const GIT_STATS_INTERVAL: Duration = Duration::from_secs(5);

        if self.last_git_stats_poll.elapsed() < GIT_STATS_INTERVAL {
            return;
        }
        self.last_git_stats_poll = std::time::Instant::now();

        if self.git_stats_in_progress.load(Ordering::SeqCst) {
            return;
        }

        // Collect all active sessions with their worktree paths and default branches
        let worktrees: Vec<(String, String, String)> = self
            .project_summaries
            .iter()
            .flat_map(|(_, summary)| {
                let branch = summary.default_branch.clone();
                summary
                    .active_sessions
                    .iter()
                    .map(move |s| (s.id.clone(), s.worktree_path.clone(), branch.clone()))
            })
            .collect();

        if worktrees.is_empty() {
            return;
        }

        let flag = self.git_stats_in_progress.clone();
        flag.store(true, Ordering::SeqCst);
        let tx = self.git_stats_tx.clone();

        std::thread::spawn(move || {
            for (session_id, worktree_path, default_branch) in worktrees {
                if let Some(stats) = parse_git_diff_stat(&worktree_path, &default_branch) {
                    let _ = tx.send(GitStatsResult {
                        session_id,
                        files_changed: stats.0,
                        lines_added: stats.1,
                        lines_removed: stats.2,
                    });
                }
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Drain git stats results and persist to the database.
    pub(super) fn poll_git_stats_results(&mut self) {
        while let Ok(result) = self.git_stats_rx.try_recv() {
            let _ = self.store.update_session_git_stats(
                &result.session_id,
                result.files_changed,
                result.lines_added,
                result.lines_removed,
            );
        }
    }

    /// Spawn a background scan for external Claude sessions every 60s.
    pub(super) fn maybe_scan_external_sessions(&mut self) {
        const SCAN_INTERVAL: Duration = Duration::from_secs(60);

        if self.last_scan.elapsed() < SCAN_INTERVAL {
            return;
        }
        self.last_scan = std::time::Instant::now();

        if self.scanner_in_progress.load(Ordering::SeqCst) {
            return;
        }

        let project_paths = self.store.list_all_project_repo_paths().unwrap_or_default();
        let known = self.store.external_session_scan_info().unwrap_or_default();

        let flag = self.scanner_in_progress.clone();
        flag.store(true, Ordering::SeqCst);
        let tx = self.scanner_tx.clone();

        std::thread::spawn(move || {
            if let Ok(result) = crate::scanner::scan_external_sessions(&project_paths, &known) {
                let _ = tx.send(result);
            }
            flag.store(false, Ordering::SeqCst);
        });
    }

    /// Drain scanner results, upsert new data, prune stale entries, and refresh the list.
    pub(super) fn poll_scanner_results(&mut self) {
        while let Ok(result) = self.scanner_rx.try_recv() {
            for session in &result.updated {
                let _ = self.store.upsert_external_session(session);
            }
            // Remove sessions that are no longer active (file not modified recently)
            let _ = self.store.prune_stale_external_sessions(&result.active_ids);
            // Refresh the in-memory list from DB
            self.external_sessions = self.store.list_external_sessions().unwrap_or_default();
        }
    }

    /// Drain background session operation results, spawn PTYs for new sessions, and show toasts.
    pub(super) fn poll_session_ops(&mut self) {
        while let Ok(result) = self.session_op_rx.try_recv() {
            match result {
                SessionOpResult::Created(setup) => {
                    let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                    let cols = term_size.0;
                    let rows = term_size.1.saturating_sub(2);

                    // Claude terminal: spawn directly as a local PTY (same as shell)
                    let wrapped = setup.claude_cmd.unwrap_or_else(|| {
                        // No task: bare `claude` session
                        crate::session::wrap_cmd_with_shell_fallback(vec!["claude".to_string()])
                    });
                    let claude_result = {
                        let mut cmd = portable_pty::CommandBuilder::new(&wrapped[0]);
                        for arg in &wrapped[1..] {
                            cmd.arg(arg);
                        }
                        cmd.cwd(&setup.worktree_path);
                        crate::pty::EmbeddedTerminal::spawn(cmd, rows, cols / 2)
                    };

                    let terminals_result = match claude_result {
                        Ok(claude) => {
                            if let Some(ref layout_config) = self.config.layout {
                                crate::pty::SessionTerminals::from_layout(
                                    claude,
                                    &setup.worktree_path,
                                    layout_config,
                                    rows,
                                    cols,
                                )
                            } else {
                                // Default: spawn shell + use from_parts
                                let shell_path =
                                    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
                                let mut shell_cmd = portable_pty::CommandBuilder::new(&shell_path);
                                shell_cmd.cwd(&setup.worktree_path);
                                crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, cols / 2).map(
                                    |shell| {
                                        crate::pty::SessionTerminals::from_parts(
                                            shell,
                                            claude,
                                            &setup.worktree_path,
                                        )
                                    },
                                )
                            }
                        }
                        Err(e) => Err(e),
                    };

                    match terminals_result {
                        Ok(mut terminals) => {
                            let sizes = compute_pane_sizes_for_resize(
                                &terminals.layout,
                                term_size.0,
                                term_size.1,
                            );
                            let _ = terminals.resize_panes(&sizes);
                            self.add_session_tab(
                                setup.session.id.clone(),
                                Box::new(terminals),
                                setup.tab_label,
                            );
                            self.show_toast("Session launched", ToastStyle::Success);
                        }
                        Err(e) => {
                            self.show_toast(
                                format!("Session launch failed: {e}"),
                                ToastStyle::Error,
                            );
                        }
                    }
                }
                SessionOpResult::CreatedNoTask { message }
                | SessionOpResult::TornDown { message, .. } => {
                    self.show_toast(message, ToastStyle::Success);
                }
                SessionOpResult::Error { message } => {
                    self.show_toast(message, ToastStyle::Error);
                    // Clear any pending relaunch — the operation failed
                    self.pending_relaunch = None;
                }
            }
            self.session_op_in_progress = false;
            let _ = self.refresh_data();
        }

        // If a teardown just completed and a relaunch is queued, launch the task now
        if !self.session_op_in_progress
            && let Some((task_id, project_id)) = self.pending_relaunch.take()
            && let Err(e) = self.launch_task(task_id, project_id)
        {
            self.show_toast(format!("Relaunch failed: {e}"), ToastStyle::Error);
        }
    }
}
