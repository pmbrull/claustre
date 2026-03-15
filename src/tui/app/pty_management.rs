use anyhow::Result;

use crate::store::TaskStatus;

use super::{
    App, Tab, ToastStyle, compute_pane_sizes_for_resize, screen_shows_permission_prompt,
    screen_shows_question_prompt,
};

impl App {
    /// Restore a session tab for an active session whose PTY was lost (e.g. after
    /// Claustre was closed and reopened). Spawns `claude --continue` as a normal
    /// local PTY in the worktree.
    pub(super) fn restore_session_tab(&mut self, session: &crate::store::Session) -> Result<()> {
        let worktree = std::path::Path::new(&session.worktree_path);
        if !worktree.exists() {
            self.show_toast("Worktree no longer exists on disk", ToastStyle::Error);
            return Ok(());
        }

        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
        let cols = term_size.0;
        let rows = term_size.1.saturating_sub(2);

        // Spawn claude in the worktree — use --resume <id> if we have the Claude
        // session ID for exact conversation resumption, otherwise fall back to --continue.
        // Pass configured model and effort flags for consistency.
        let model = &self.config.claude.model;
        let effort = &self.config.claude.effort;
        let claude_args = if let Some(ref csid) = session.claude_session_id {
            vec![
                "claude".to_string(),
                "--model".to_string(),
                model.clone(),
                "--effort".to_string(),
                effort.clone(),
                "--resume".to_string(),
                csid.clone(),
            ]
        } else {
            vec![
                "claude".to_string(),
                "--model".to_string(),
                model.clone(),
                "--effort".to_string(),
                effort.clone(),
                "--continue".to_string(),
            ]
        };
        let wrapped = crate::session::wrap_cmd_with_shell_fallback(claude_args);
        let mut claude_builder = portable_pty::CommandBuilder::new(&wrapped[0]);
        for arg in &wrapped[1..] {
            claude_builder.arg(arg);
        }
        claude_builder.cwd(&session.worktree_path);
        let claude_terminal = crate::pty::EmbeddedTerminal::spawn(claude_builder, rows, cols / 2)?;

        let mut terminals = if let Some(ref layout_config) = self.config.layout {
            crate::pty::SessionTerminals::from_layout(
                claude_terminal,
                &session.worktree_path,
                layout_config,
                rows,
                cols,
            )?
        } else {
            let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let mut shell_cmd = portable_pty::CommandBuilder::new(&shell_path);
            shell_cmd.cwd(&session.worktree_path);
            let shell_terminal = crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, cols / 2)?;
            crate::pty::SessionTerminals::from_parts(
                shell_terminal,
                claude_terminal,
                &session.worktree_path,
            )
        };

        let sizes = compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
        let _ = terminals.resize_panes_with_clear(&sizes);
        let label = session.tab_label.clone();
        self.add_session_tab(session.id.clone(), Box::new(terminals), label);
        // Switch to the newly added tab
        self.active_tab = self.tabs.len() - 1;

        // Restore session + task status based on task state
        if let Some(task) = self.tasks.iter().find(|t| {
            t.session_id.as_deref() == Some(&session.id) && t.status == TaskStatus::Interrupted
        }) {
            if task.pr_url.is_some() {
                // Interrupted task has an open PR — restore to in_review
                self.store
                    .update_task_status(&task.id, TaskStatus::InReview)?;
                self.store.update_session_status(
                    &session.id,
                    crate::store::ClaudeStatus::Done,
                    "PR in review",
                )?;
            } else {
                self.store
                    .update_task_status(&task.id, TaskStatus::Working)?;
                self.store.update_session_status(
                    &session.id,
                    crate::store::ClaudeStatus::Working,
                    "Restored",
                )?;
            }
        } else if self.tasks.iter().any(|t| {
            t.session_id.as_deref() == Some(&session.id)
                && matches!(
                    t.status,
                    TaskStatus::InReview | TaskStatus::Conflict | TaskStatus::CiFailed
                )
        }) {
            // Task already in review/conflict/ci_failed — session stays Done
            self.store.update_session_status(
                &session.id,
                crate::store::ClaudeStatus::Done,
                "PR in review",
            )?;
        } else {
            self.store.update_session_status(
                &session.id,
                crate::store::ClaudeStatus::Working,
                "Restored",
            )?;
        }
        self.refresh_data()?;

        self.show_toast("Session tab restored", ToastStyle::Success);
        Ok(())
    }

    /// Restore tabs for active sessions on TUI startup.
    ///
    /// Scans DB for active (non-closed) sessions and opens a tab for each one
    /// that doesn't already have one, spawning `claude --continue` in the
    /// worktree as a normal local PTY.
    pub(super) fn reconnect_running_sessions(&mut self) {
        let Ok(projects) = self.store.list_projects() else {
            return;
        };
        for project in &projects {
            let Ok(sessions) = self.store.list_active_sessions_for_project(&project.id) else {
                continue;
            };
            for session in &sessions {
                // Skip if already have a tab for this session
                if self.tabs.iter().any(
                    |t| matches!(t, Tab::Session { session_id: sid, .. } if sid == &session.id),
                ) {
                    continue;
                }

                if let Err(e) = self.restore_session_tab(session) {
                    eprintln!("reconnect: failed to restore session {}: {e}", session.id);
                }
            }
        }
    }

    /// Switch to the next tab (wrapping around to Dashboard).
    pub(super) fn next_tab(&mut self) {
        if self.tabs.len() > 1 {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
    }

    /// Switch to the previous tab (wrapping around to last session).
    pub(super) fn prev_tab(&mut self) {
        if self.tabs.len() > 1 {
            if self.active_tab == 0 {
                self.active_tab = self.tabs.len() - 1;
            } else {
                self.active_tab -= 1;
            }
        }
    }

    /// Process PTY output for all session tabs (budget-limited per pane).
    pub(super) fn process_pty_output(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.process_output();
            }
        }
    }

    /// Flush all pending PTY output for every session without a byte budget.
    /// Called when switching to a session tab so the first rendered frame
    /// shows fully up-to-date content instead of stale data from the last
    /// (potentially 1-second-old) dashboard tick.
    pub(super) fn flush_all_pty_output(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.process_output_full();
            }
        }
    }

    /// Set all session terminal parsers to their scroll offsets for rendering.
    /// Must be called immediately before `terminal.draw()` and paired with
    /// [`Self::restore_live_scrollback`] immediately after.
    pub(super) fn prepare_render_scrollback(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.prepare_for_render();
            }
        }
    }

    /// Restore all session terminal parsers to the live screen (scrollback 0).
    pub(super) fn restore_live_scrollback(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.restore_after_render();
            }
        }
    }

    /// Detect sessions where Claude is blocked on user input by scanning PTY screens.
    ///
    /// Populates two sets:
    /// - `paused_sessions` — Claude is waiting for tool-approval ("Allow Bash?" dialog)
    /// - `waiting_sessions` — Claude asked a question via `AskUserQuestion` and awaits an answer
    ///
    /// Both are in-memory overrides: the DB still shows `working`.
    pub(super) fn detect_paused_sessions(&mut self) {
        self.paused_sessions.clear();
        self.waiting_sessions.clear();
        for tab in &self.tabs {
            if let Tab::Session {
                session_id,
                terminals,
                ..
            } = tab
            {
                // Only check sessions that have a working task
                let has_working_task = self.tasks.iter().any(|t| {
                    t.status == TaskStatus::Working
                        && t.session_id.as_deref() == Some(session_id.as_str())
                });
                if !has_working_task {
                    continue;
                }

                // Use the live screen (scrollback 0) for detection so
                // prompts are not missed when the user has scrolled back.
                let detected = terminals.with_claude_live_screen(|screen| {
                    if screen_shows_permission_prompt(screen) {
                        Some(true)
                    } else if screen_shows_question_prompt(screen) {
                        Some(false)
                    } else {
                        None
                    }
                });
                match detected {
                    Some(Some(true)) => {
                        self.paused_sessions.insert(session_id.clone());
                    }
                    Some(Some(false)) => {
                        self.waiting_sessions.insert(session_id.clone());
                    }
                    _ => {}
                }
            }
        }
    }
}
