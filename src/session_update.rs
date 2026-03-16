//! Hook-driven session state transitions.
//!
//! Extracted from `main.rs` so the orchestration logic that runs when
//! `claustre session-update` is called by stop / user-prompt hooks can
//! be tested without spawning a subprocess.

use anyhow::Result;

use crate::store::{self, Store};

/// Arguments passed by the hooks to `claustre session-update`.
pub struct SessionUpdateArgs<'a> {
    pub session_id: &'a str,
    pub pr_url: Option<&'a str>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub resumed: bool,
    pub claude_session_id: Option<&'a str>,
    /// Pre-parsed progress items (read from the tmp file by the caller).
    pub progress: Option<Vec<store::ClaudeProgressItem>>,
    /// Force session to idle (from the Notification hook's `idle_prompt` event).
    pub set_idle: bool,
}

/// What happened as a result of the session update.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionUpdateOutcome {
    /// Task transitioned to `in_review` after a PR was detected.
    PrDetected { task_id: String, is_new_pr: bool },
    /// User resumed interaction — task moved back to `working`.
    Resumed { task_id: String },
    /// User resumed while a working task exists — session set to working.
    ResumedWorking { task_id: String },
    /// An interrupted task was restored to `working` (hook proves Claude is alive).
    Restored { task_id: String },
    /// A working task exists with no PR — session state unchanged.
    WorkingNoPr,
    /// Notification hook reported `idle_prompt` — session forced to idle.
    NotificationIdle,
    /// No active task — session set to idle.
    Idle,
}

/// Core session-update logic. Pure store operations, no file I/O or notifications.
///
/// Returns what happened so the caller can fire notifications or other side effects.
pub fn apply(store: &Store, args: &SessionUpdateArgs<'_>) -> Result<SessionUpdateOutcome> {
    // Sync progress items if provided
    if let Some(ref items) = args.progress {
        let _ = store.update_session_progress(args.session_id, items);
    }

    // Store Claude's internal session ID for --resume support
    if let Some(csid) = args.claude_session_id {
        let _ = store.set_claude_session_id(args.session_id, csid);
    }

    // Find the active task. A hook firing proves Claude is still running,
    // so interrupted tasks count as active.
    let active_task = store
        .working_task_for_session(args.session_id)?
        .or(store.interrupted_task_for_session(args.session_id)?)
        .or(store.in_review_task_for_session(args.session_id)?);

    // Update token usage (cumulative replacement, not additive)
    if let (Some(inp), Some(out)) = (args.input_tokens, args.output_tokens)
        && let Some(ref task) = active_task
    {
        let _ = store.set_task_usage(&task.id, inp, out);
    }

    // Notification hook's idle_prompt — Claude is waiting for user input.
    // Force session to idle regardless of task state. The UserPromptSubmit
    // hook sets it back to Working when the user sends the next message.
    if args.set_idle {
        store.update_session_status(args.session_id, store::ClaudeStatus::Idle, "")?;
        return Ok(SessionUpdateOutcome::NotificationIdle);
    }

    // Branch on what the hook reported:
    if let Some(url) = args.pr_url
        && let Some(ref task) = active_task
    {
        // PR detected → transition task to in_review, session to done.
        // However, if the user already resumed work (task is `working`) and
        // this is the same PR we already know about, skip the transition.
        // Without this guard the Stop hook re-detects the existing PR after
        // every Claude turn and overwrites the `working` status that the
        // UserPromptSubmit hook set via `--resumed`.
        let is_new_pr = task.pr_url.as_deref() != Some(url);
        if !is_new_pr && task.status == store::TaskStatus::Working {
            // Same PR, task already resumed — don't regress to in_review.
            // Session stays working until the task is done.
            Ok(SessionUpdateOutcome::WorkingNoPr)
        } else {
            store.update_task_pr_url(&task.id, url)?;
            store.update_task_status(&task.id, store::TaskStatus::InReview)?;
            store.update_session_status(args.session_id, store::ClaudeStatus::Done, "")?;
            Ok(SessionUpdateOutcome::PrDetected {
                task_id: task.id.clone(),
                is_new_pr,
            })
        }
    } else if args.resumed {
        // User resumed interaction — set session back to working.
        if let Some(task) = store
            .in_review_task_for_session(args.session_id)?
            .or(store.interrupted_task_for_session(args.session_id)?)
        {
            // Resume from in_review/conflict/ci_failed/interrupted task
            store.update_task_status(&task.id, store::TaskStatus::Working)?;
            // Clear stale ci_status so the dashboard doesn't show "CI failed"
            // from a previous run while the user is actively working on fixes.
            store.update_task_ci_status(&task.id, None)?;
            store.update_session_status(
                args.session_id,
                store::ClaudeStatus::Working,
                &format!("Resumed: {}", task.title),
            )?;
            Ok(SessionUpdateOutcome::Resumed { task_id: task.id })
        } else if let Some(ref task) = active_task {
            // User sent a prompt while task is working — ensure session is working too
            store.update_session_status(
                args.session_id,
                store::ClaudeStatus::Working,
                &format!("Working: {}", task.title),
            )?;
            Ok(SessionUpdateOutcome::ResumedWorking {
                task_id: task.id.clone(),
            })
        } else {
            Ok(SessionUpdateOutcome::Idle)
        }
    } else if let Some(ref task) = active_task {
        // Stop/TaskCompleted hook fired with an active task but no PR
        // and not --resumed.
        if task.status == store::TaskStatus::Interrupted {
            // The hook proves Claude is active, so restore to working.
            store.update_task_status(&task.id, store::TaskStatus::Working)?;
            store.update_session_status(
                args.session_id,
                store::ClaudeStatus::Working,
                &format!("Restored: {}", task.title),
            )?;
            Ok(SessionUpdateOutcome::Restored {
                task_id: task.id.clone(),
            })
        } else {
            // Working task with no PR — keep as-is. Session stays working.
            Ok(SessionUpdateOutcome::WorkingNoPr)
        }
    } else {
        // No active task — session is truly idle
        store.update_session_status(args.session_id, store::ClaudeStatus::Idle, "")?;
        Ok(SessionUpdateOutcome::Idle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ClaudeStatus, PushMode, TaskMode, TaskStatus};

    /// Helper: set up a project + session + task, return their IDs.
    fn setup_working_task(store: &Store) -> (String, String, String) {
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab-1")
            .unwrap();
        let task = store
            .create_task(
                &project.id,
                "test-task",
                "description",
                TaskMode::Supervised,
                None,
                None,
                PushMode::Pr,
                false,
            )
            .unwrap();
        store.assign_task_to_session(&task.id, &session.id).unwrap();
        store
            .update_task_status(&task.id, TaskStatus::Working)
            .unwrap();
        store
            .update_session_status(&session.id, ClaudeStatus::Working, "")
            .unwrap();
        (project.id, session.id, task.id)
    }

    // ── PR detection flow ──

    #[test]
    fn pr_detected_transitions_task_to_in_review() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: Some("https://github.com/org/repo/pull/42"),
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SessionUpdateOutcome::PrDetected {
                task_id: task_id.clone(), // compared, not consumed
                is_new_pr: true,
            }
        );
        let task = store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::InReview);
        assert_eq!(
            task.pr_url.as_deref(),
            Some("https://github.com/org/repo/pull/42")
        );
        let session = store.get_session(&session_id).unwrap();
        assert_eq!(session.claude_status, ClaudeStatus::Done);
    }

    #[test]
    fn pr_detected_same_url_marks_not_new_when_still_in_review() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        let url = "https://github.com/org/repo/pull/42";

        // First PR detection → in_review
        apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: Some(url),
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::InReview
        );

        // Second hook fire with same PR URL while still in_review
        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: Some(url),
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SessionUpdateOutcome::PrDetected {
                task_id,
                is_new_pr: false,
            }
        );
    }

    /// Regression test: when user resumes an `in_review` task (sending a prompt),
    /// the Stop hook must not overwrite the `working` status back to `in_review`
    /// by re-detecting the same PR URL.
    #[test]
    fn stop_hook_does_not_regress_resumed_task_to_in_review() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        let url = "https://github.com/org/repo/pull/42";

        // 1. Stop hook detects PR → task goes to in_review
        apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: Some(url),
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::InReview
        );

        // 2. UserPromptSubmit hook fires --resumed → task goes back to working
        apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: true,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::Working
        );

        // 3. Stop hook fires again with same PR URL — must NOT regress
        //    to in_review. Session stays working.
        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: Some(url),
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(outcome, SessionUpdateOutcome::WorkingNoPr);
        // Task must still be working
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::Working
        );
        // Session stays working (Claude is still active on the task)
        assert_eq!(
            store.get_session(&session_id).unwrap().claude_status,
            ClaudeStatus::Working
        );
    }

    // ── Resume flow ──

    #[test]
    fn resume_transitions_in_review_to_working() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        // Move to in_review
        store
            .update_task_status(&task_id, TaskStatus::InReview)
            .unwrap();
        store
            .update_session_status(&session_id, ClaudeStatus::Done, "")
            .unwrap();

        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: true,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SessionUpdateOutcome::Resumed {
                task_id: task_id.clone(), // compared, not consumed
            }
        );
        let task = store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Working);
        let session = store.get_session(&session_id).unwrap();
        assert_eq!(session.claude_status, ClaudeStatus::Working);
    }

    /// Regression test: when CI fails and the user resumes work to fix it,
    /// the stale `ci_status = failed` must be cleared so the dashboard
    /// doesn't keep showing "CI failed" while the user is pushing fixes.
    #[test]
    fn resume_from_ci_failed_clears_ci_status() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        // Simulate: PR created → in_review → CI failed → ci_failed
        store
            .update_task_status(&task_id, TaskStatus::InReview)
            .unwrap();
        store
            .update_task_ci_status(&task_id, Some(store::CiStatus::Failed))
            .unwrap();
        store
            .update_task_status(&task_id, TaskStatus::CiFailed)
            .unwrap();

        // User resumes to fix CI
        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: true,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SessionUpdateOutcome::Resumed {
                task_id: task_id.clone(),
            }
        );
        let task = store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::Working);
        // ci_status must be cleared
        assert_eq!(task.ci_status, None);
    }

    #[test]
    fn resume_transitions_interrupted_to_working() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        // Simulate claustre restart: task goes to interrupted
        store
            .update_task_status(&task_id, TaskStatus::Interrupted)
            .unwrap();

        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: true,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SessionUpdateOutcome::Resumed {
                task_id: task_id.clone(), // compared, not consumed
            }
        );
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::Working
        );
    }

    // ── Interrupted task recovery ──

    #[test]
    fn hook_restores_interrupted_task_to_working() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        store
            .update_task_status(&task_id, TaskStatus::Interrupted)
            .unwrap();

        // Hook fires without --resumed and without PR (just a normal stop hook)
        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            SessionUpdateOutcome::Restored {
                task_id: task_id.clone(), // compared, not consumed
            }
        );
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::Working
        );
        assert_eq!(
            store.get_session(&session_id).unwrap().claude_status,
            ClaudeStatus::Working
        );
    }

    // ── Working task with no PR ──

    /// Hook fires with a working task and no PR — session stays working.
    #[test]
    fn working_task_no_pr_keeps_session_working() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(outcome, SessionUpdateOutcome::WorkingNoPr);
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::Working
        );
        // Session stays working throughout the task
        assert_eq!(
            store.get_session(&session_id).unwrap().claude_status,
            ClaudeStatus::Working
        );
    }

    // ── Notification idle ──

    /// Notification hook fires idle_prompt — session goes idle even with a working task.
    #[test]
    fn set_idle_forces_session_idle_with_working_task() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: true,
            },
        )
        .unwrap();

        assert_eq!(outcome, SessionUpdateOutcome::NotificationIdle);
        // Task stays working — only session status changes
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::Working
        );
        assert_eq!(
            store.get_session(&session_id).unwrap().claude_status,
            ClaudeStatus::Idle
        );
    }

    // ── Idle session ──

    #[test]
    fn no_active_task_sets_idle() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab-1")
            .unwrap();

        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session.id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(outcome, SessionUpdateOutcome::Idle);
        assert_eq!(
            store.get_session(&session.id).unwrap().claude_status,
            ClaudeStatus::Idle
        );
    }

    // ── Token usage ──

    #[test]
    fn token_usage_is_set_on_active_task() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: Some(5000),
                output_tokens: Some(3000),
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        let task = store.get_task(&task_id).unwrap();
        assert_eq!(task.input_tokens, 5000);
        assert_eq!(task.output_tokens, 3000);
    }

    #[test]
    fn token_usage_ignored_without_active_task() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab-1")
            .unwrap();

        // No task assigned — tokens should not crash
        let result = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session.id,
                pr_url: None,
                input_tokens: Some(1000),
                output_tokens: Some(500),
                resumed: false,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        );
        assert!(result.is_ok());
    }

    // ── Claude session ID ──

    #[test]
    fn claude_session_id_is_stored() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab-1")
            .unwrap();

        apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session.id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: Some("claude-xyz-123"),
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        let s = store.get_session(&session.id).unwrap();
        assert_eq!(s.claude_session_id.as_deref(), Some("claude-xyz-123"));
    }

    // ── Progress sync ──

    #[test]
    fn progress_items_are_synced() {
        let store = Store::open_in_memory().unwrap();
        let project = store
            .create_project("proj", "/tmp/proj", "main", true)
            .unwrap();
        let session = store
            .create_session(&project.id, "feat", "/tmp/wt", "tab-1")
            .unwrap();

        let items = vec![store::ClaudeProgressItem {
            subject: "Step 1".to_string(),
            status: "done".to_string(),
        }];

        apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session.id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: false,
                claude_session_id: None,
                progress: Some(items),
                set_idle: false,
            },
        )
        .unwrap();

        let s = store.get_session(&session.id).unwrap();
        assert_eq!(s.claude_progress.len(), 1);
        assert_eq!(s.claude_progress[0].subject, "Step 1");
    }

    // ── Combined: PR + tokens in same call ──

    #[test]
    fn pr_with_tokens_updates_both() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: Some("https://github.com/org/repo/pull/99"),
                input_tokens: Some(10_000),
                output_tokens: Some(5_000),
                resumed: false,
                claude_session_id: Some("sess-abc"),
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert!(matches!(
            outcome,
            SessionUpdateOutcome::PrDetected {
                is_new_pr: true,
                ..
            }
        ));

        let task = store.get_task(&task_id).unwrap();
        assert_eq!(task.status, TaskStatus::InReview);
        assert_eq!(task.input_tokens, 10_000);
        assert_eq!(task.output_tokens, 5_000);

        let session = store.get_session(&session_id).unwrap();
        assert_eq!(session.claude_session_id.as_deref(), Some("sess-abc"));
    }

    // ── Resumed with working task (no in_review/interrupted) ──

    #[test]
    fn resume_with_working_task_sets_session_working() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        let outcome = apply(
            &store,
            &SessionUpdateArgs {
                session_id: &session_id,
                pr_url: None,
                input_tokens: None,
                output_tokens: None,
                resumed: true,
                claude_session_id: None,
                progress: None,
                set_idle: false,
            },
        )
        .unwrap();

        assert_eq!(outcome, SessionUpdateOutcome::ResumedWorking { task_id });
        assert_eq!(
            store.get_session(&session_id).unwrap().claude_status,
            ClaudeStatus::Working
        );
    }
}
