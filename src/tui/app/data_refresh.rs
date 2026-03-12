use std::sync::atomic::Ordering;

use anyhow::Result;

use crate::store::{Project, Session, Task, TaskStatus};

use super::{App, ToastStyle, build_project_summaries};

impl App {
    /// Auto-teardown sessions for completed push-mode tasks.
    ///
    /// Push-mode tasks don't create PRs, so the PR merge poller never triggers cleanup.
    /// This method detects sessions whose push-mode task is `Done` and tears them down.
    pub fn maybe_teardown_push_mode_sessions(&mut self) {
        if self.session_op_in_progress {
            return;
        }
        let sessions = self
            .store
            .sessions_needing_push_mode_cleanup()
            .unwrap_or_default();
        if let Some((session_id, task_title)) = sessions.into_iter().next() {
            self.spawn_teardown_session(session_id);
            self.show_toast(
                format!("Push completed — session closed: {task_title}"),
                ToastStyle::Success,
            );
        }
    }

    pub fn refresh_data(&mut self) -> Result<()> {
        self.projects = self.store.list_projects()?;

        if let Some(project) = self.projects.get(self.project_index) {
            self.sessions = self.store.list_sessions_for_project(&project.id)?;
            self.tasks = self.store.list_tasks_for_project(&project.id)?;
        } else {
            self.sessions.clear();
            self.tasks.clear();
        }

        // Detect Working → InReview transitions and show a toast (once per task)
        let new_review_title = self.tasks.iter().find_map(|t| {
            (t.status == TaskStatus::InReview
                && self.prev_task_statuses.get(&t.id) == Some(&TaskStatus::Working)
                && !self.notified_in_review.contains(&t.id))
            .then(|| (t.id.clone(), t.title.clone()))
        });
        // Clear notified set for tasks that left InReview (e.g. marked done)
        self.notified_in_review.retain(|id| {
            self.tasks
                .iter()
                .any(|t| t.id == *id && t.status == TaskStatus::InReview)
        });
        self.prev_task_statuses = self
            .tasks
            .iter()
            .map(|t| (t.id.clone(), t.status))
            .collect();
        if let Some((id, title)) = new_review_title {
            self.notified_in_review.insert(id.clone());
            self.show_toast(format!("Ready for review: {title}"), ToastStyle::Success);

            // Spawn review loop pane if the task has review_loop enabled
            self.maybe_spawn_review_loop(&id);
        }

        // Clean up spawned set for tasks that are done or no longer exist
        self.review_loop_spawned.retain(|id| {
            self.tasks
                .iter()
                .any(|t| t.id == *id && t.status == TaskStatus::InReview)
        });

        // Pre-fetch sidebar summaries for all projects
        self.project_summaries = build_project_summaries(&self.store, &self.projects);

        // Refresh cached project stats for the selected project
        self.project_stats = self
            .selected_project()
            .and_then(|p| self.store.project_stats(&p.id).ok());

        // Pre-fetch subtask counts for visible tasks
        self.subtask_counts.clear();
        for task in &self.tasks {
            if let Ok(counts) = self.store.subtask_count(&task.id)
                && counts.0 > 0
            {
                self.subtask_counts.insert(task.id.clone(), counts);
            }
        }

        // Recompute visible tasks cache after data changes
        self.recompute_visible_tasks();

        // Clamp indices
        if self.project_index >= self.projects.len() {
            self.project_index = self.projects.len().saturating_sub(1);
        }
        let visible_count = self.visible_task_count();
        if self.task_index >= visible_count && visible_count > 0 {
            self.task_index = visible_count.saturating_sub(1);
        } else if visible_count == 0 {
            self.task_index = 0;
        }

        // Refresh subtasks for selected task
        if let Some(task) = self.visible_task_at(self.task_index) {
            self.subtasks = self
                .store
                .list_subtasks_for_task(&task.id)
                .unwrap_or_default();
        } else {
            self.subtasks.clear();
        }
        if self.subtask_index >= self.subtasks.len() {
            self.subtask_index = self.subtasks.len().saturating_sub(1);
        } else if self.subtasks.is_empty() {
            self.subtask_index = 0;
        }

        // Refresh rate limit state and auto-clear if expired
        if let Ok(state) = self.store.get_rate_limit_state() {
            if state.is_rate_limited
                && let Some(ref reset_at) = state.reset_at
                && let Ok(reset_time) = chrono::DateTime::parse_from_rfc3339(reset_at)
                && chrono::Utc::now() > reset_time
            {
                let _ = self.store.clear_rate_limit();
                self.rate_limit_state = self.store.get_rate_limit_state().unwrap_or_default();
            } else {
                self.rate_limit_state = state;
            }
        }

        // Read usage percentages from the Claude API cache
        self.refresh_usage_from_api_cache();

        // Refresh external sessions list
        self.external_sessions = self.store.list_external_sessions().unwrap_or_default();

        Ok(())
    }

    /// Read usage percentages from ~/.claude/statusline-cache.json (shared with statusline).
    /// Always uses cached data if present. Triggers a background refresh when stale
    /// or when the cache lacks usage percentage data.
    pub(super) fn refresh_usage_from_api_cache(&mut self) {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let cache_path = home.join(".claude/statusline-cache.json");

        let mut cache_fresh = false;
        let mut has_pct_data = false;

        if let Ok(content) = std::fs::read_to_string(&cache_path)
            && let Ok(cache) = serde_json::from_str::<serde_json::Value>(&content)
        {
            // Only overwrite pct values when the cache actually has them.
            // The JS statusline script omits pct fields when the API doesn't
            // return utilization, and overwriting with None would blank the bars.
            if let Some(pct) = cache["data"]["pct5h"].as_f64() {
                self.rate_limit_state.usage_5h_pct = Some(pct);
                has_pct_data = true;
            }
            if let Some(pct) = cache["data"]["pct7d"].as_f64() {
                self.rate_limit_state.usage_7d_pct = Some(pct);
                has_pct_data = true;
            }
            if let Some(reset) = cache["data"]["reset5h"].as_str() {
                self.rate_limit_state.reset_5h = Some(reset.to_string());
            }
            if let Some(reset) = cache["data"]["reset7d"].as_str() {
                self.rate_limit_state.reset_7d = Some(reset.to_string());
            }

            let timestamp = cache["timestamp"].as_f64().unwrap_or(0.0);
            #[expect(
                clippy::cast_precision_loss,
                reason = "millisecond epoch fits in f64 for decades"
            )]
            let age_ms = (chrono::Utc::now().timestamp_millis() as f64) - timestamp;
            cache_fresh = age_ms < 120_000.0;
        }

        // Fetch when cache is stale OR when it exists but lacks percentage data.
        // The JS statusline sometimes writes the cache without pct fields (e.g.
        // when the API omits utilization), so timestamp alone isn't sufficient.
        let needs_fetch = !cache_fresh || !has_pct_data;
        if needs_fetch && !self.usage_fetch_in_progress.load(Ordering::SeqCst) {
            self.spawn_usage_fetch();
        }
    }

    pub fn selected_project(&self) -> Option<&Project> {
        self.projects.get(self.project_index)
    }

    /// Returns the session linked to the currently selected task, if any.
    pub fn session_for_selected_task(&self) -> Option<&Session> {
        let task = self.visible_tasks().into_iter().nth(self.task_index)?;
        let sid = task.session_id.as_deref()?;
        self.sessions.iter().find(|s| s.id == sid)
    }

    /// Recompute the cached visible task indices. Must be called after any change
    /// to `self.tasks`, `self.task_filter`, or task sort order.
    pub fn recompute_visible_tasks(&mut self) {
        let filter_lower = self.task_filter.to_lowercase();
        let mut indices: Vec<usize> = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                filter_lower.is_empty() || t.title.to_lowercase().contains(&filter_lower)
            })
            .map(|(i, _)| i)
            .collect();
        indices.sort_by(|&a, &b| {
            self.tasks[a]
                .status
                .sort_priority()
                .cmp(&self.tasks[b].status.sort_priority())
                .then_with(|| self.tasks[a].sort_order.cmp(&self.tasks[b].sort_order))
        });
        self.cached_visible_indices = indices;
    }

    /// Returns all tasks for the selected project, optionally filtered
    /// by the current search term (`task_filter`). Uses case-insensitive title matching.
    /// Tasks are sorted by status priority, then by `sort_order` within each status group.
    /// Done tasks appear last.
    pub fn visible_tasks(&self) -> Vec<&Task> {
        self.cached_visible_indices
            .iter()
            .map(|&i| &self.tasks[i])
            .collect()
    }

    /// Number of visible tasks (avoids allocating a Vec).
    pub fn visible_task_count(&self) -> usize {
        self.cached_visible_indices.len()
    }

    /// Get a single visible task by display index (avoids allocating a Vec).
    pub fn visible_task_at(&self, index: usize) -> Option<&Task> {
        self.cached_visible_indices
            .get(index)
            .map(|&i| &self.tasks[i])
    }
}
