use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, Paragraph, Wrap},
};

use crate::store::{ClaudeStatus, TaskStatus};

use super::super::app::{App, Focus, InputMode};
use super::spinner_char;
use super::toast_line;
use super::usage::draw_usage_bars;
use super::usage::format_tokens;

pub(super) fn draw_active(frame: &mut Frame, app: &mut App) {
    draw_active_impl(frame, app, frame.area());
}

/// Draw the dashboard in a specific area (used when tab bar is present).
pub(super) fn draw_active_in_area(frame: &mut Frame, app: &mut App, area: Rect) {
    // This delegates to the same rendering logic as draw_active but
    // within a sub-area. For simplicity, we render directly using frame.area()
    // since ratatui handles clipping. The tab bar has already consumed its row.
    draw_active_impl(frame, app, area);
}

fn draw_active_impl(frame: &mut Frame, app: &mut App, size: Rect) {
    // Check if we need a status line above the hints
    let needs_attention = app
        .tasks
        .iter()
        .filter(|t| {
            matches!(
                t.status,
                TaskStatus::InReview | TaskStatus::Conflict | TaskStatus::CiFailed
            )
        })
        .count();
    let has_status_line = app.toast_message.is_some()
        || (needs_attention > 0
            && app.input_mode != InputMode::ConfirmDelete
            && app.input_mode != InputMode::TaskFilter);

    // Always reserve 2 lines for bottom (status + hints) to prevent panel height jitter
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(0),    // main area
            Constraint::Length(2), // status + hints (fixed)
        ])
        .split(size);

    // Title bar with version
    let mut title_spans = vec![Span::styled(
        " claustre ",
        Style::default()
            .fg(app.theme.text_accent)
            .add_modifier(Modifier::BOLD),
    )];
    title_spans.push(Span::styled(
        crate::update::VERSION,
        Style::default().fg(app.theme.text_secondary),
    ));
    if let Some(ref new_ver) = app.updated_version {
        title_spans.push(Span::styled(
            format!("  ⬆ {new_ver} ready — restart to apply"),
            Style::default().fg(app.theme.toast_success),
        ));
    } else if let Some(ref new_ver) = app.available_version {
        title_spans.push(Span::styled(
            format!("  ⬆ {new_ver} available"),
            Style::default().fg(app.theme.accent_secondary),
        ));
    }
    if let Some(ref warning) = app.config_warning {
        title_spans.push(Span::styled(
            format!("  ⚠ {warning}"),
            Style::default().fg(app.theme.accent_secondary),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(title_spans)), outer[0]);

    // Main area: left column (30%) | right column (70%)
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(outer[1]);

    let usage_height: u16 = if app.rate_limit_state.is_rate_limited {
        6
    } else {
        4
    };

    // Both columns share the same 60/40 vertical split so panels align horizontally
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(main[0]);

    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(main[1]);

    // Sub-split the right bottom area into Session Detail and Usage
    let right_bottom = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(usage_height)])
        .split(right_rows[1]);

    draw_projects(frame, app, left[0]);
    draw_project_stats(frame, app, left[1]);
    draw_task_queue(frame, app, right_rows[0]);
    draw_session_detail(frame, app, right_bottom[0]);
    draw_usage_bars(frame, app, right_bottom[1]);

    // Bottom area: always split into status line (row 0) + hints line (row 1)
    let bottom = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(outer[2]);

    if app.input_mode == InputMode::ConfirmDelete {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" Delete '{}'? ", app.confirm_target),
                    Style::default().fg(app.theme.status_error),
                ),
                Span::styled(
                    "(y: confirm, Esc: cancel)",
                    Style::default().fg(app.theme.text_secondary),
                ),
            ])),
            bottom[0],
        );
    } else if app.input_mode == InputMode::TaskFilter {
        let tf_cursor = app.task_filter_cursor.min(app.task_filter.len());
        let (tf_before, tf_after) = app.task_filter.split_at(tf_cursor);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" /", Style::default().fg(app.theme.accent_secondary)),
                Span::raw(tf_before.to_string()),
                Span::styled("\u{2588}", Style::default().fg(app.theme.accent_secondary)),
                Span::raw(tf_after.to_string()),
                Span::styled(
                    "  Enter:apply  Esc:clear",
                    Style::default().fg(app.theme.text_secondary),
                ),
            ])),
            bottom[0],
        );
    } else if has_status_line {
        // Status line: toast takes priority, then attention count
        let status = if let Some(line) = toast_line(app) {
            line
        } else {
            Line::from(vec![Span::styled(
                format!(" {needs_attention} task(s) need your attention "),
                Style::default()
                    .fg(app.theme.accent_secondary)
                    .add_modifier(Modifier::BOLD),
            )])
        };
        frame.render_widget(Paragraph::new(status), bottom[0]);
    }

    // Hints always on the second row
    let hints = match app.focus {
        Focus::Projects => {
            " Enter:select  a:add  d:delete  n:task  i:skills  j/k:nav  l:tasks  ?:help  q:quit"
        }
        Focus::Tasks => {
            " Enter:session  n:new  e:edit  s:sub  v:details  l:launch  k:kill  r:done  o:PR  d:del  i:skills  /:filter  J/K:reorder  ?:help  q:quit"
        }
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hints,
            Style::default().fg(app.theme.text_secondary),
        ))),
        bottom[1],
    );
}

fn draw_projects(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Projects;
    let border_style = if focused {
        app.theme.focused_border()
    } else {
        app.theme.unfocused_border()
    };

    let block = Block::default()
        .title(" Projects ")
        .borders(Borders::ALL)
        .border_style(border_style);

    if app.projects.is_empty() {
        let msg = Paragraph::new("  No projects yet.\n  Press 'a' to add one.")
            .style(Style::default().fg(app.theme.text_secondary))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let empty_summary = super::super::app::ProjectSummary::default();

    let items: Vec<ListItem> = app
        .projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let summary = app
                .project_summaries
                .get(&project.id)
                .unwrap_or(&empty_summary);

            let session_count = summary.active_sessions.len();

            let mut spans = vec![];

            // Selection indicator
            if i == app.project_index {
                spans.push(Span::styled(
                    "▸ ",
                    Style::default().fg(app.theme.selection_indicator),
                ));
            } else {
                spans.push(Span::raw("  "));
            }

            spans.push(Span::styled(
                &project.name,
                if i == app.project_index {
                    Style::default()
                        .fg(app.theme.text_primary)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text_primary)
                },
            ));

            spans.push(Span::styled(
                format!(" [{session_count}]"),
                Style::default().fg(app.theme.text_secondary),
            ));

            // Task status indicators — same symbols and colors as the task panel
            let tc = &summary.task_counts;
            let status_indicators: &[(usize, &str, Style)] = &[
                (
                    tc.working,
                    TaskStatus::Working.symbol(),
                    app.theme.task_status_style(TaskStatus::Working),
                ),
                (
                    tc.interrupted,
                    TaskStatus::Interrupted.symbol(),
                    app.theme.task_status_style(TaskStatus::Interrupted),
                ),
                (
                    tc.in_review,
                    TaskStatus::InReview.symbol(),
                    app.theme.task_status_style(TaskStatus::InReview),
                ),
                (
                    tc.conflict,
                    TaskStatus::Conflict.symbol(),
                    app.theme.task_status_style(TaskStatus::Conflict),
                ),
                (
                    tc.ci_failed,
                    TaskStatus::CiFailed.symbol(),
                    app.theme.task_status_style(TaskStatus::CiFailed),
                ),
                (
                    tc.error,
                    TaskStatus::Error.symbol(),
                    app.theme.task_status_style(TaskStatus::Error),
                ),
                (
                    tc.pending,
                    TaskStatus::Pending.symbol(),
                    app.theme.task_status_style(TaskStatus::Pending),
                ),
                (
                    tc.draft,
                    TaskStatus::Draft.symbol(),
                    app.theme.task_status_style(TaskStatus::Draft),
                ),
            ];
            for &(count, symbol, style) in status_indicators {
                if count > 0 {
                    spans.push(Span::styled(format!(" {symbol}{count}"), style));
                }
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_session_detail(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = app.theme.unfocused_border();

    let block = Block::default()
        .title(" Session Detail ")
        .borders(Borders::ALL)
        .border_style(border_style);

    if app.visible_task_count() == 0 {
        let msg = Paragraph::new("  No tasks")
            .style(Style::default().fg(app.theme.text_secondary))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let Some(session) = app.session_for_selected_task() else {
        let msg = Paragraph::new("  No session \u{2014} press l to launch")
            .style(Style::default().fg(app.theme.text_secondary))
            .block(block);
        frame.render_widget(msg, area);
        return;
    };

    let is_paused = app.paused_sessions.contains(&session.id);
    let is_waiting = app.waiting_sessions.contains(&session.id);
    let (status_symbol, status_label, status_color) = if is_paused || is_waiting {
        // Claude is not actively consuming tokens — show idle instead of working
        let style = app.theme.claude_status_style(ClaudeStatus::Idle);
        let color = style.fg.unwrap_or(app.theme.text_secondary);
        (
            ClaudeStatus::Idle.symbol(),
            ClaudeStatus::Idle.as_str(),
            color,
        )
    } else {
        let style = app.theme.claude_status_style(session.claude_status);
        let color = style.fg.unwrap_or(app.theme.text_secondary);
        (
            session.claude_status.symbol(),
            session.claude_status.as_str(),
            color,
        )
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Branch: ", Style::default().fg(app.theme.text_secondary)),
            Span::styled(
                &session.branch_name,
                Style::default().fg(app.theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Status: ", Style::default().fg(app.theme.text_secondary)),
            Span::styled(status_symbol, Style::default().fg(status_color)),
            Span::raw(" "),
            Span::styled(status_label, Style::default().fg(status_color)),
        ]),
    ];

    if !session.status_message.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  Message: ", Style::default().fg(app.theme.text_secondary)),
            Span::styled(
                format!("\"{}\"", &session.status_message),
                Style::default().fg(app.theme.text_primary),
            ),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("  Files: ", Style::default().fg(app.theme.text_secondary)),
        Span::styled(
            format!(
                "{} changed (+{} -{})",
                session.files_changed, session.lines_added, session.lines_removed
            ),
            Style::default().fg(app.theme.text_primary),
        ),
    ]));

    // Fetch the selected task once for token usage and PR URL display.
    let selected_task = app.visible_tasks().into_iter().nth(app.task_index);

    // Show token usage from the selected task
    if let Some(task) = &selected_task {
        let total_tokens = task.input_tokens + task.output_tokens;
        if total_tokens > 0 {
            lines.push(Line::from(vec![
                Span::styled("  Tokens: ", Style::default().fg(app.theme.text_secondary)),
                Span::styled(
                    format!(
                        "{} in / {} out",
                        format_tokens(task.input_tokens),
                        format_tokens(task.output_tokens),
                    ),
                    Style::default().fg(app.theme.text_primary),
                ),
            ]));
        }
    }

    if is_paused || is_waiting || session.claude_status != ClaudeStatus::Working {
        lines.push(Line::from(vec![
            Span::styled(
                "  Last activity: ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                &session.last_activity_at,
                Style::default().fg(app.theme.text_primary),
            ),
        ]));
    }

    // Show Claude's internal task progress
    if !session.claude_progress.is_empty() {
        let completed = session
            .claude_progress
            .iter()
            .filter(|p| p.status == "completed")
            .count();
        let total = session.claude_progress.len();
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            format!("  Progress: ({completed}/{total})"),
            Style::default().fg(app.theme.text_secondary),
        )]));
        for item in &session.claude_progress {
            let (symbol, color) = match item.status.as_str() {
                "completed" => ("\u{2713}", app.theme.status_working),
                "in_progress" => ("\u{25cf}", app.theme.accent_secondary),
                _ => ("\u{2610}", app.theme.text_secondary),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("    {symbol} "), Style::default().fg(color)),
                Span::styled(&item.subject, Style::default().fg(app.theme.text_primary)),
            ]));
        }
    }

    // Show PR URL from the selected task
    if let Some(task) = &selected_task
        && let Some(ref url) = task.pr_url
    {
        lines.push(Line::from(vec![
            Span::styled("  PR: ", Style::default().fg(app.theme.text_secondary)),
            Span::styled(url, Style::default().fg(app.theme.pr_link)),
        ]));
    }

    let detail = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

fn draw_task_queue(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Tasks;
    let border_style = if focused {
        app.theme.focused_border()
    } else {
        app.theme.unfocused_border()
    };

    // Build items in a block so visible_tasks borrow is dropped before we mutate task_list_state
    let (items, visible_count, title) = {
        let visible_tasks = app.visible_tasks();
        let count = visible_tasks.len();

        let title = if app.task_filter.is_empty() {
            if count > 0 {
                // Show scroll position when there are tasks
                let pos = app.task_index + 1;
                format!(" Task Queue ({pos}/{count}) ")
            } else {
                " Task Queue ".to_string()
            }
        } else {
            format!(" Task Queue [/{}] ({count}) ", app.task_filter)
        };

        let items: Vec<ListItem> = visible_tasks
            .iter()
            .map(|task| {
                // Detect if this working task's session is paused or waiting for user input
                let session_id = task.session_id.as_deref();
                let is_paused = task.status == TaskStatus::Working
                    && session_id.is_some_and(|sid| app.paused_sessions.contains(sid));
                let is_waiting = task.status == TaskStatus::Working
                    && session_id.is_some_and(|sid| app.waiting_sessions.contains(sid));

                let (status_symbol, status_label, status_style) = if is_paused || is_waiting {
                    // Claude is not actively consuming tokens — show idle instead of working
                    (
                        ClaudeStatus::Idle.symbol(),
                        ClaudeStatus::Idle.as_str(),
                        app.theme.claude_status_style(ClaudeStatus::Idle),
                    )
                } else {
                    (
                        task.status.symbol(),
                        task.status.as_str(),
                        app.theme.task_status_style(task.status),
                    )
                };

                let mut spans = vec![];

                spans.push(Span::styled(status_symbol, status_style));
                spans.push(Span::raw(" "));
                if app.pending_titles.contains(&task.id) {
                    spans.push(Span::styled(
                        spinner_char(),
                        Style::default().fg(app.theme.spinner),
                    ));
                    spans.push(Span::raw(" "));
                }

                let title_color = if task.status == TaskStatus::Done {
                    app.theme.text_secondary
                } else {
                    app.theme.text_primary
                };
                spans.push(Span::styled(
                    task.title.clone(),
                    Style::default().fg(title_color),
                ));

                if let Some(&(total, done)) = app.subtask_counts.get(&task.id) {
                    spans.push(Span::styled(
                        format!(" ({done}/{total})"),
                        Style::default().fg(app.theme.text_secondary),
                    ));
                }

                spans.push(Span::styled(format!("  {status_label}"), status_style));

                if let Some(ci) = task.ci_status {
                    let ci_style = app.theme.ci_status_style(ci);
                    spans.push(Span::styled(format!("  {} CI", ci.symbol()), ci_style));
                }

                if task.pr_url.is_some() {
                    spans.push(Span::styled("  PR", Style::default().fg(app.theme.pr_link)));
                }

                ListItem::new(Line::from(spans))
            })
            .collect();

        (items, count, title)
    };
    // visible_tasks borrow is now dropped — safe to mutate app

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    if visible_count == 0 {
        let msg = Paragraph::new("  No tasks. Press 'n' to create one.")
            .style(Style::default().fg(app.theme.text_secondary))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    // Sync list state with current selection
    app.task_list_state.select(Some(app.task_index));

    let highlight_symbol = if focused { "▸ " } else { "  " };
    let list = List::new(items)
        .block(block)
        .highlight_symbol(highlight_symbol)
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(list, area, &mut app.task_list_state);
}

fn draw_project_stats(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Stats ")
        .borders(Borders::ALL)
        .border_style(app.theme.unfocused_border());

    let Some(ref stats) = app.project_stats else {
        let msg = Paragraph::new("  No project selected")
            .style(Style::default().fg(app.theme.text_secondary))
            .block(block);
        frame.render_widget(msg, area);
        return;
    };

    let repo_path = app
        .selected_project()
        .map_or_else(String::new, |p| p.repo_path.clone());

    let mut lines = vec![
        Line::from(vec![Span::styled(
            format!("  {repo_path}"),
            Style::default().fg(app.theme.accent_primary),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  Total tasks:   ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                stats.total_tasks.to_string(),
                Style::default().fg(app.theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Completed:     ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                stats.completed_tasks.to_string(),
                Style::default().fg(app.theme.status_working),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Sessions run:  ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                stats.total_sessions.to_string(),
                Style::default().fg(app.theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Total time:    ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                stats.formatted_time(),
                Style::default().fg(app.theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Tokens used:   ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                format_tokens(stats.total_tokens()),
                Style::default().fg(app.theme.text_primary),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "  Avg task time: ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                stats.formatted_avg_task_time(),
                Style::default().fg(app.theme.text_primary),
            ),
        ]),
    ];

    if !app.external_sessions.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled(
            format!("  ── External ({}) ──", app.external_sessions.len()),
            Style::default().fg(app.theme.text_secondary),
        )]));
        for ext in &app.external_sessions {
            let branch = ext.git_branch.as_deref().unwrap_or("—");
            let age = format_relative_time(ext.ended_at.as_deref());
            lines.push(Line::from(vec![
                Span::styled("  ● ", Style::default().fg(app.theme.status_working)),
                Span::styled(
                    &ext.project_name,
                    Style::default().fg(app.theme.text_primary),
                ),
                Span::styled(
                    format!("  {branch}"),
                    Style::default().fg(app.theme.text_secondary),
                ),
                Span::styled(
                    format!("  {age}"),
                    Style::default().fg(app.theme.text_secondary),
                ),
            ]));
        }
    }

    let detail = Paragraph::new(lines).block(block);
    frame.render_widget(detail, area);
}

/// Format an ISO timestamp as a human-readable relative time (e.g. "2m ago", "1h ago").
fn format_relative_time(timestamp: Option<&str>) -> String {
    let Some(ts) = timestamp else {
        return "—".to_string();
    };
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return "—".to_string();
    };
    let now = chrono::Utc::now();
    let elapsed = now.signed_duration_since(dt);
    let secs = elapsed.num_seconds();
    if secs < 0 {
        "now".to_string()
    } else if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
