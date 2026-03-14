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
}

/// What happened as a result of the session update.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionUpdateOutcome {
    /// Task transitioned to `in_review` after a PR was detected.
    PrDetected { task_id: String, is_new_pr: bool },
    /// User resumed interaction — task moved back to `working`.
    Resumed { task_id: String },
    /// An interrupted task was restored to `working` (hook proves Claude is alive).
    Restored { task_id: String },
    /// A working task exists with no PR — session state unchanged.
    WorkingNoPr,
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
        .or(store.interrupted_task_for_session(args.session_id)?);

    // Update token usage (cumulative replacement, not additive)
    if let (Some(inp), Some(out)) = (args.input_tokens, args.output_tokens)
        && let Some(ref task) = active_task
    {
        let _ = store.set_task_usage(&task.id, inp, out);
    }

    // Branch on what the hook reported:
    if let Some(url) = args.pr_url
        && let Some(ref task) = active_task
    {
        // PR detected → transition task to in_review, session to done
        let is_new_pr = task.pr_url.as_deref() != Some(url);
        store.update_task_pr_url(&task.id, url)?;
        store.update_task_status(&task.id, store::TaskStatus::InReview)?;
        store.update_session_status(args.session_id, store::ClaudeStatus::Done, "")?;
        Ok(SessionUpdateOutcome::PrDetected {
            task_id: task.id.clone(),
            is_new_pr,
        })
    } else if args.resumed
        && let Some(task) = store
            .in_review_task_for_session(args.session_id)?
            .or(store.interrupted_task_for_session(args.session_id)?)
    {
        // User resumed → transition back to working
        store.update_task_status(&task.id, store::TaskStatus::Working)?;
        store.update_session_status(
            args.session_id,
            store::ClaudeStatus::Working,
            &format!("Resumed: {}", task.title),
        )?;
        Ok(SessionUpdateOutcome::Resumed { task_id: task.id })
    } else if let Some(ref task) = active_task {
        if task.status == store::TaskStatus::Interrupted {
            // Hook firing proves Claude is alive → restore to working
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
            // Working task with no PR — keep as-is
            Ok(SessionUpdateOutcome::WorkingNoPr)
        }
    } else {
        // No active task → idle
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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
    fn pr_detected_same_url_marks_not_new() {
        let store = Store::open_in_memory().unwrap();
        let (_proj, session_id, task_id) = setup_working_task(&store);

        let url = "https://github.com/org/repo/pull/42";

        // First PR detection
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
            },
        )
        .unwrap();

        // Resume the task back to working
        store
            .update_task_status(&task_id, TaskStatus::Working)
            .unwrap();
        store
            .update_session_status(&session_id, ClaudeStatus::Working, "")
            .unwrap();

        // Second hook fire with same PR URL
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

    #[test]
    fn working_task_no_pr_keeps_state() {
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
            },
        )
        .unwrap();

        assert_eq!(outcome, SessionUpdateOutcome::WorkingNoPr);
        assert_eq!(
            store.get_task(&task_id).unwrap().status,
            TaskStatus::Working
        );
    }

    // ── Idle session ──

    #[test]
    fn no_active_task_sets_idle() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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
            },
        );
        assert!(result.is_ok());
    }

    // ── Claude session ID ──

    #[test]
    fn claude_session_id_is_stored() {
        let store = Store::open_in_memory().unwrap();
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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
        let project = store.create_project("proj", "/tmp/proj", "main").unwrap();
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
}
