use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::store::{ClaudeStatus, TaskStatus};

use super::app::{App, Focus, InputMode, ToastStyle};

/// Returns an animated spinner character that cycles every 1s (one tick).
fn spinner_char() -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let idx = (ms / 250) as usize % FRAMES.len();
    FRAMES[idx]
}

/// If a toast is active, return a styled `Line` for it; otherwise `None`.
fn toast_line(app: &App) -> Option<Line<'static>> {
    let msg = app.toast_message.as_ref()?;
    let color = match app.toast_style {
        ToastStyle::Info => Color::Cyan,
        ToastStyle::Success => Color::Green,
        ToastStyle::Error => Color::Red,
    };
    Some(Line::from(Span::styled(
        format!(" {msg} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )))
}

pub fn draw(frame: &mut Frame, app: &App) {
    draw_active(frame, app);

    // Floating panel overlays
    match app.input_mode {
        InputMode::CommandPalette => draw_command_palette(frame, app),
        InputMode::NewTask => draw_task_form_panel(frame, app, " New Task "),
        InputMode::EditTask => draw_task_form_panel(frame, app, " Edit Task "),
        InputMode::NewProject => draw_new_project_panel(frame, app),
        InputMode::HelpOverlay => draw_help_overlay(frame, app),
        InputMode::SubtaskPanel => draw_subtask_panel(frame, app),
        _ => {}
    }
}

fn draw_command_palette(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Center the palette
    let width = 50u16.min(area.width.saturating_sub(4));
    let height = (app.palette_filtered.len() as u16 + 3).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height / 5;
    let palette_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, palette_area);

    let block = Block::default()
        .title(" Command Palette ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let inner = block.inner(palette_area);
    frame.render_widget(block, palette_area);

    if inner.height < 2 {
        return;
    }

    // Search input
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Cyan)),
        Span::raw(&app.input_buffer),
        Span::styled("█", Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);

    // Items
    let items_area = Rect::new(
        inner.x,
        inner.y + 1,
        inner.width,
        inner.height.saturating_sub(1),
    );
    let items: Vec<ListItem> = app
        .palette_filtered
        .iter()
        .enumerate()
        .map(|(i, &idx)| {
            let item = &app.palette_items[idx];
            let style = if i == app.palette_index {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let prefix = if i == app.palette_index { "▸ " } else { "  " };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(&item.label, style),
            ]))
        })
        .collect();

    frame.render_widget(List::new(items), items_area);
}

fn draw_active(frame: &mut Frame, app: &App) {
    let size = frame.area();

    // Check if we need an extra status line above the hints
    let needs_attention = app
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::InReview)
        .count();
    let has_status_line = app.toast_message.is_some()
        || (needs_attention > 0
            && app.input_mode != InputMode::ConfirmDelete
            && app.input_mode != InputMode::TaskFilter);
    let bottom_height: u16 = if has_status_line { 2 } else { 1 };

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title bar
            Constraint::Min(0),    // main area
            Constraint::Length(bottom_height),
        ])
        .split(size);

    // Title bar
    let title = Line::from(vec![
        Span::styled(
            " claustre ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("                                        "),
        Span::styled(
            "a:project  n:task  l:launch  i:skills  q:quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), outer[0]);

    // Main area: left column (30%) | right column (70%)
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(outer[1]);

    // Left column: projects (top 60%) | stats (bottom 40%)
    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(main[0]);

    draw_projects(frame, app, left[0]);
    draw_project_stats(frame, app, left[1]);

    // Right column: tasks (top, flexible) | session detail (mid 35%) | usage (bottom)
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Percentage(35),
            Constraint::Length(if app.rate_limit_state.is_rate_limited {
                6
            } else {
                4
            }),
        ])
        .split(main[1]);

    draw_task_queue(frame, app, right[0]);
    draw_session_detail(frame, app, right[1]);
    draw_usage_bars(frame, app, right[2]);

    // Bottom area: interactive modes take over fully, otherwise status line + hints
    if app.input_mode == InputMode::ConfirmDelete {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    format!(" Delete '{}'? ", app.confirm_target),
                    Style::default().fg(Color::Red),
                ),
                Span::styled(
                    "(y: confirm, Esc: cancel)",
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            outer[2],
        );
    } else if app.input_mode == InputMode::TaskFilter {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" /", Style::default().fg(Color::Yellow)),
                Span::raw(&app.task_filter),
                Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "  Enter:apply  Esc:clear",
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            outer[2],
        );
    } else if has_status_line {
        // Split bottom into status line + hints line
        let bottom = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(outer[2]);

        // Status line: toast takes priority, then attention count
        let status = if let Some(line) = toast_line(app) {
            line
        } else {
            Line::from(vec![Span::styled(
                format!(" {needs_attention} task(s) need your attention "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )])
        };
        frame.render_widget(Paragraph::new(status), bottom[0]);

        // Hints always visible
        let hints = match app.focus {
            Focus::Projects => " a:add  d:delete  n:task  i:skills  j/k:nav  ?:help",
            Focus::Tasks => {
                " n:new  e:edit  s:subtasks  l:launch  r:review  o:PR  d:del  /:filter  J/K:reorder  ?:help"
            }
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hints,
                Style::default().fg(Color::DarkGray),
            ))),
            bottom[1],
        );
    } else {
        // Just hints
        let hints = match app.focus {
            Focus::Projects => " a:add  d:delete  n:task  i:skills  j/k:nav  ?:help",
            Focus::Tasks => {
                " n:new  e:edit  s:subtasks  l:launch  r:review  o:PR  d:del  /:filter  J/K:reorder  ?:help"
            }
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hints,
                Style::default().fg(Color::DarkGray),
            ))),
            outer[2],
        );
    }
}

fn draw_projects(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Projects;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Projects ")
        .borders(Borders::ALL)
        .border_style(border_style);

    if app.projects.is_empty() {
        let msg = Paragraph::new("  No projects yet.\n  Press 'a' to add one.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let empty_summary = super::app::ProjectSummary::default();

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
                spans.push(Span::styled("▸ ", Style::default().fg(Color::Cyan)));
            } else {
                spans.push(Span::raw("  "));
            }

            spans.push(Span::styled(
                &project.name,
                if i == app.project_index {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            ));

            spans.push(Span::styled(
                format!(" [{session_count}]"),
                Style::default().fg(Color::DarkGray),
            ));

            if summary.has_review {
                spans.push(Span::styled(
                    " ←!",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }

            // Show session statuses under the project
            let mut lines = vec![Line::from(spans)];
            for session in &summary.active_sessions {
                let status_style = match session.claude_status {
                    ClaudeStatus::Working => Style::default().fg(Color::Green),
                    ClaudeStatus::WaitingForInput => Style::default().fg(Color::Yellow),
                    ClaudeStatus::Error => Style::default().fg(Color::Red),
                    ClaudeStatus::Done => Style::default().fg(Color::Blue),
                    ClaudeStatus::Idle => Style::default().fg(Color::DarkGray),
                };
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(session.claude_status.symbol(), status_style),
                    Span::raw(" "),
                    Span::styled(session.claude_status.as_str(), status_style),
                ]));
            }

            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_session_detail(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = Style::default().fg(Color::DarkGray);

    let block = Block::default()
        .title(" Session Detail ")
        .borders(Borders::ALL)
        .border_style(border_style);

    if app.visible_tasks().is_empty() {
        let msg = Paragraph::new("  No tasks")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let Some(session) = app.session_for_selected_task() else {
        let msg = Paragraph::new("  No session \u{2014} press l to launch")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    };

    let status_color = match session.claude_status {
        ClaudeStatus::Working => Color::Green,
        ClaudeStatus::WaitingForInput => Color::Yellow,
        ClaudeStatus::Error => Color::Red,
        ClaudeStatus::Done => Color::Blue,
        ClaudeStatus::Idle => Color::DarkGray,
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Branch: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.branch_name, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Status: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                session.claude_status.symbol(),
                Style::default().fg(status_color),
            ),
            Span::raw(" "),
            Span::styled(
                session.claude_status.as_str(),
                Style::default().fg(status_color),
            ),
        ]),
    ];

    if !session.status_message.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("  Message: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("\"{}\"", &session.status_message),
                Style::default().fg(Color::White),
            ),
        ]));
    }

    lines.push(Line::from(vec![
        Span::styled("  Files: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "{} changed (+{} -{})",
                session.files_changed, session.lines_added, session.lines_removed
            ),
            Style::default().fg(Color::White),
        ),
    ]));

    // Show token usage from the selected task
    if let Some(task) = app.visible_tasks().into_iter().nth(app.task_index) {
        let total_tokens = task.input_tokens + task.output_tokens;
        if total_tokens > 0 {
            lines.push(Line::from(vec![
                Span::styled("  Tokens: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!(
                        "{} in / {} out  (${:.2})",
                        format_tokens(task.input_tokens),
                        format_tokens(task.output_tokens),
                        task.cost,
                    ),
                    Style::default().fg(Color::White),
                ),
            ]));
        }
    }

    if session.claude_status != ClaudeStatus::Working {
        lines.push(Line::from(vec![
            Span::styled("  Last activity: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.last_activity_at, Style::default().fg(Color::White)),
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
            Style::default().fg(Color::DarkGray),
        )]));
        for item in &session.claude_progress {
            let (symbol, color) = match item.status.as_str() {
                "completed" => ("\u{2713}", Color::Green),
                "in_progress" => ("\u{25cf}", Color::Yellow),
                _ => ("\u{2610}", Color::DarkGray),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("    {symbol} "), Style::default().fg(color)),
                Span::styled(&item.subject, Style::default().fg(Color::White)),
            ]));
        }
    }

    // Show PR URL from the selected task
    if let Some(task) = app.visible_tasks().into_iter().nth(app.task_index)
        && let Some(ref url) = task.pr_url
    {
        lines.push(Line::from(vec![
            Span::styled("  PR: ", Style::default().fg(Color::DarkGray)),
            Span::styled(url, Style::default().fg(Color::Magenta)),
        ]));
    }

    let detail = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });
    frame.render_widget(detail, area);
}

fn draw_task_queue(frame: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Tasks;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let title = if app.task_filter.is_empty() {
        " Task Queue ".to_string()
    } else {
        format!(" Task Queue [/{}] ", app.task_filter)
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let visible_tasks = app.visible_tasks();

    if visible_tasks.is_empty() {
        let msg = Paragraph::new("  No active tasks. Press 'n' to create one.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = visible_tasks
        .iter()
        .enumerate()
        .map(|(i, task)| {
            let is_done = task.status == TaskStatus::Done;

            let status_style = match task.status {
                TaskStatus::Pending => Style::default().fg(Color::DarkGray),
                TaskStatus::InProgress => Style::default().fg(Color::Green),
                TaskStatus::InReview => Style::default().fg(Color::Yellow),
                TaskStatus::Done => Style::default().fg(Color::Blue),
                TaskStatus::Error => Style::default().fg(Color::Red),
            };

            let mut spans = vec![];

            if i == app.task_index && focused {
                spans.push(Span::styled("▸ ", Style::default().fg(Color::Cyan)));
            } else {
                spans.push(Span::raw("  "));
            }

            spans.push(Span::styled(task.status.symbol(), status_style));
            spans.push(Span::raw(" "));
            if app.pending_titles.contains(&task.id) {
                spans.push(Span::styled(
                    spinner_char(),
                    Style::default().fg(Color::Yellow),
                ));
                spans.push(Span::raw(" "));
            }

            if is_done {
                spans.push(Span::styled(
                    &task.title,
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(&task.title, Style::default().fg(Color::White)));
            }

            // Skip subtask counts for done tasks (noise for completed work)
            if !is_done {
                if let Some(&(total, done)) = app.subtask_counts.get(&task.id) {
                    spans.push(Span::styled(
                        format!(" ({done}/{total})"),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }

            if is_done {
                spans.push(Span::styled(
                    format!("  {}", task.status.as_str()),
                    Style::default().fg(Color::DarkGray),
                ));
            } else {
                spans.push(Span::styled(
                    format!("  {}", task.status.as_str()),
                    status_style,
                ));
            }

            // Skip PR badge for done tasks (or dim it)
            if !is_done && task.pr_url.is_some() {
                spans.push(Span::styled("  PR", Style::default().fg(Color::Magenta)));
            }

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_project_stats(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Stats ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let Some(ref stats) = app.project_stats else {
        let msg = Paragraph::new("  No project selected")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    };

    let lines = vec![
        Line::from(vec![
            Span::styled("  Total tasks:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                stats.total_tasks.to_string(),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Completed:     ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                stats.completed_tasks.to_string(),
                Style::default().fg(Color::Green),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Sessions run:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                stats.total_sessions.to_string(),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Total time:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(stats.formatted_time(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::styled("  Tokens used:   ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format_tokens(stats.total_tokens()),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Avg task time: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                stats.formatted_avg_task_time(),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Total cost:    ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("${:.2}", stats.total_cost),
                Style::default().fg(Color::White),
            ),
        ]),
    ];

    let detail = Paragraph::new(lines).block(block);
    frame.render_widget(detail, area);
}

fn draw_task_form_panel(frame: &mut Frame, app: &App, title: &str) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));

    // Calculate prompt text to estimate wrapped line count
    let prompt_text = if app.new_task_field == 0 {
        format!("{}\u{2588}", app.input_buffer)
    } else {
        app.new_task_description.clone()
    };

    // inner width = width - 2 (borders), prefix "  Prompt: " = 10 chars
    let inner_width = width.saturating_sub(2) as usize;
    let prompt_total_chars = 10 + prompt_text.len();
    let prompt_lines = if inner_width > 0 {
        (prompt_total_chars.div_ceil(inner_width)).max(1) as u16
    } else {
        1
    };

    // Layout (inner rows): 0=pad, 1..1+prompt=prompt, 1+prompt=pad, 2+prompt=mode, 3+prompt=pad, 4+prompt=hints
    // Inner height = 5 + prompt_lines, outer = inner + 2 (borders)
    let height = (7u16 + prompt_lines).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 5 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(Color::DarkGray);
    let highlight = Style::default().fg(Color::Yellow);
    let val_style = Style::default().fg(Color::White);

    // Field 0: Prompt (wraps to multiple lines)
    let (label_s, val) = if app.new_task_field == 0 {
        (highlight, format!("{}\u{2588}", app.input_buffer))
    } else {
        (dim, app.new_task_description.clone())
    };
    let prompt_height = prompt_lines.min(inner.height.saturating_sub(4));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Prompt: ", label_s),
            Span::styled(val, val_style),
        ]))
        .wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y + 1, inner.width, prompt_height),
    );

    // Shift remaining fields down by extra prompt lines
    let extra = prompt_height.saturating_sub(1);

    // Field 1: Mode
    let label_s = if app.new_task_field == 1 {
        highlight
    } else {
        dim
    };
    let mode_s = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let arrow_hint = if app.new_task_field == 1 {
        "  (\u{2190}/\u{2192} toggle)"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Mode:   ", label_s),
            Span::styled(app.new_task_mode.as_str(), mode_s),
            Span::styled(arrow_hint, dim),
        ])),
        Rect::new(inner.x, inner.y + 3 + extra, inner.width, 1),
    );

    // Hints
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Tab", highlight),
            Span::styled(":field  ", dim),
            Span::styled("Enter", highlight),
            Span::styled(":create  ", dim),
            Span::styled("Esc", highlight),
            Span::styled(":cancel", dim),
        ])),
        Rect::new(inner.x, inner.y + 5 + extra, inner.width, 1),
    );
}

fn draw_new_project_panel(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));

    // Dynamic height: base 9, plus dropdown rows if visible
    let dropdown_rows = if app.show_path_suggestions {
        (app.path_suggestions.len().min(8) as u16) + 1 // +1 for separator
    } else {
        0
    };
    let height = (9u16 + dropdown_rows).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Add Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 5 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(Color::DarkGray);
    let highlight = Style::default().fg(Color::Magenta);
    let val_style = Style::default().fg(Color::White);

    // Field 0: Name
    let (label_s, val) = if app.new_project_field == 0 {
        (highlight, format!("{}\u{2588}", app.input_buffer))
    } else {
        (dim, app.new_project_name.clone())
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Name: ", label_s),
            Span::styled(val, val_style),
        ])),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );

    // Field 1: Path
    let (label_s, val) = if app.new_project_field == 1 {
        (highlight, format!("{}\u{2588}", app.input_buffer))
    } else {
        (dim, app.new_project_path.clone())
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Path: ", label_s),
            Span::styled(val, val_style),
        ])),
        Rect::new(inner.x, inner.y + 3, inner.width, 1),
    );

    // Path suggestion dropdown
    let mut hint_y_offset = 5;
    if app.show_path_suggestions {
        let visible_count = app.path_suggestions.len().min(8);
        let separator_y = inner.y + 4;

        if separator_y < inner.y + inner.height {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled("  ─── suggestions ───", dim))),
                Rect::new(inner.x, separator_y, inner.width, 1),
            );
        }

        for (i, suggestion) in app.path_suggestions.iter().take(visible_count).enumerate() {
            let row_y = separator_y + 1 + i as u16;
            if row_y >= inner.y + inner.height {
                break;
            }

            let is_selected = i == app.path_suggestion_index;
            let style = if is_selected {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            let prefix = if is_selected { "  \u{25b8} " } else { "    " };

            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(format!("{suggestion}/"), style),
                ])),
                Rect::new(inner.x, row_y, inner.width, 1),
            );
        }

        if app.path_suggestions.len() > 8 {
            let more_y = separator_y + 1 + visible_count as u16;
            if more_y < inner.y + inner.height {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("    ... +{} more", app.path_suggestions.len() - 8),
                        dim,
                    ))),
                    Rect::new(inner.x, more_y, inner.width, 1),
                );
            }
        }

        hint_y_offset = 5 + dropdown_rows;
    }

    // Hints — context-sensitive based on whether suggestions are visible
    let hints = if app.show_path_suggestions {
        Line::from(vec![
            Span::styled("  Tab", highlight),
            Span::styled(":complete  ", dim),
            Span::styled("↑↓", highlight),
            Span::styled(":navigate  ", dim),
            Span::styled("Enter", highlight),
            Span::styled(":accept  ", dim),
            Span::styled("Esc", highlight),
            Span::styled(":close", dim),
        ])
    } else {
        Line::from(vec![
            Span::styled("  Tab", highlight),
            Span::styled(":field  ", dim),
            Span::styled("Enter", highlight),
            Span::styled(":create  ", dim),
            Span::styled("Esc", highlight),
            Span::styled(":cancel", dim),
        ])
    };
    frame.render_widget(
        Paragraph::new(hints),
        Rect::new(inner.x, inner.y + hint_y_offset, inner.width, 1),
    );
}

fn draw_usage_bars(frame: &mut Frame, app: &App, area: Rect) {
    let state = &app.rate_limit_state;

    let block = Block::default()
        .title(" Usage ")
        .borders(Borders::ALL)
        .border_style(if state.is_rate_limited {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        });

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width < 20 {
        return;
    }

    let mut lines = vec![];

    if state.is_rate_limited {
        let limit_label = state.limit_type.as_deref().unwrap_or("?");
        lines.push(Line::from(vec![Span::styled(
            format!("  \u{26a0} RATE LIMITED ({limit_label})"),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )]));

        if let Some(ref reset_at) = state.reset_at {
            // Show just HH:MM from the timestamp
            let display_time = reset_at
                .find('T')
                .map_or(reset_at.as_str(), |i| &reset_at[i + 1..]);
            let display_time = display_time.trim_end_matches('Z');
            let display_time = &display_time[..display_time.len().min(5)];
            lines.push(Line::from(vec![
                Span::styled("  Resumes: ", Style::default().fg(Color::DarkGray)),
                Span::styled(display_time.to_string(), Style::default().fg(Color::Yellow)),
            ]));
        }
    }

    // 5h bar
    lines.push(usage_bar_line(
        "5h",
        state.usage_5h_pct,
        state.reset_5h.as_deref(),
        inner.width as usize,
    ));

    // 7d bar
    lines.push(usage_bar_line(
        "7d",
        state.usage_7d_pct,
        state.reset_7d.as_deref(),
        inner.width as usize,
    ));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn usage_bar_line(
    label: &str,
    pct: Option<f64>,
    reset: Option<&str>,
    total_width: usize,
) -> Line<'static> {
    let Some(pct_raw) = pct else {
        // No data yet — show a placeholder
        return Line::from(vec![
            Span::styled(format!("  {label}: "), Style::default().fg(Color::DarkGray)),
            Span::styled("--", Style::default().fg(Color::DarkGray)),
        ]);
    };

    let pct_clamped = pct_raw.clamp(0.0, 100.0);

    // "  5h: " = 6, " XX%" = 4, " (reset Xd Xh)" worst case ~16
    let reset_suffix = reset.map_or_else(String::new, |r| format!(" \u{21bb}{r}"));
    let overhead = 6 + 5 + reset_suffix.len();
    let bar_width = total_width.saturating_sub(overhead);

    let filled = ((pct_clamped / 100.0) * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_color = if pct_clamped > 90.0 {
        Color::Red
    } else if pct_clamped >= 70.0 {
        Color::Yellow
    } else {
        Color::Green
    };

    let filled_str: String = "\u{2588}".repeat(filled);
    let empty_str: String = "\u{2591}".repeat(empty);

    let mut spans = vec![
        Span::styled(format!("  {label}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(filled_str, Style::default().fg(bar_color)),
        Span::styled(empty_str, Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {pct_clamped:.0}%"),
            Style::default().fg(Color::White),
        ),
    ];

    if !reset_suffix.is_empty() {
        spans.push(Span::styled(
            reset_suffix,
            Style::default().fg(Color::DarkGray),
        ));
    }

    Line::from(spans)
}

fn format_tokens(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}

fn draw_subtask_panel(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let list_height = app.subtasks.len().min(10) as u16;
    let height = (8u16 + list_height).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Subtasks ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 3 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(Color::DarkGray);
    let highlight = Style::default().fg(Color::Yellow);

    // Render existing subtasks
    let mut y_offset = 0u16;
    for (i, st) in app.subtasks.iter().enumerate() {
        if y_offset >= inner.height.saturating_sub(3) {
            break;
        }
        let status_style = match st.status {
            TaskStatus::Pending => Style::default().fg(Color::DarkGray),
            TaskStatus::InProgress => Style::default().fg(Color::Green),
            TaskStatus::InReview => Style::default().fg(Color::Yellow),
            TaskStatus::Done => Style::default().fg(Color::Blue),
            TaskStatus::Error => Style::default().fg(Color::Red),
        };
        let prefix = if i == app.subtask_index { "▸ " } else { "  " };
        let selector_style = if i == app.subtask_index {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, selector_style),
                Span::styled(st.status.symbol(), status_style),
                Span::raw(" "),
                Span::styled(&st.title, Style::default().fg(Color::White)),
            ])),
            Rect::new(inner.x, inner.y + y_offset, inner.width, 1),
        );
        y_offset += 1;
    }

    if app.subtasks.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("  No subtasks yet", dim)),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        y_offset = 1;
    }

    // Separator
    y_offset += 1;

    // Input line
    if inner.y + y_offset < inner.y + inner.height.saturating_sub(1) {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  > ", highlight),
                Span::styled(
                    format!("{}\u{2588}", app.input_buffer),
                    Style::default().fg(Color::White),
                ),
            ])),
            Rect::new(inner.x, inner.y + y_offset, inner.width, 1),
        );
    }

    // Hints at bottom
    let hints_y = inner.y + inner.height.saturating_sub(1);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Enter", highlight),
            Span::styled(":add  ", dim),
            Span::styled("d", highlight),
            Span::styled(":del  ", dim),
            Span::styled("j/k", highlight),
            Span::styled(":nav  ", dim),
            Span::styled("Esc", highlight),
            Span::styled(":close", dim),
        ])),
        Rect::new(inner.x, hints_y, inner.width, 1),
    );
}

fn draw_help_overlay(frame: &mut Frame, _app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = 27u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Help \u{2014} press ? or Esc to close ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    let lines: Vec<Line<'_>> = vec![
        help_section("Navigation"),
        help_line("  Ctrl+P", "Command palette"),
        help_line("  1/2", "Focus projects/tasks"),
        help_line("  j/k", "Navigate up/down"),
        help_line("  q", "Quit"),
        Line::from(""),
        help_section("Projects"),
        help_line("  a", "Add project"),
        help_line("  d", "Delete project"),
        Line::from(""),
        help_section("Tasks"),
        help_line("  n", "New task"),
        help_line("  e", "Edit task (pending only)"),
        help_line("  s", "Subtasks panel"),
        help_line("  l", "Launch task"),
        help_line("  r", "Review (mark done)"),
        help_line("  o", "Open PR in browser"),
        help_line("  d", "Delete task"),
        help_line("  /", "Search/filter tasks"),
        help_line("  Shift+J/K", "Reorder tasks"),
        Line::from(""),
        help_section("Skills"),
        help_line("  i", "Open skills panel"),
    ];

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn help_section(title: &str) -> Line<'_> {
    Line::from(Span::styled(
        format!("  {title}"),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("  {key:<14}"), Style::default().fg(Color::Cyan)),
        Span::styled(desc, Style::default().fg(Color::White)),
    ])
}

#[expect(dead_code, reason = "retained for future use in task duration display")]
fn format_task_duration(start: &str, end: &str) -> String {
    let start_dt = chrono::DateTime::parse_from_rfc3339(start);
    let end_dt = chrono::DateTime::parse_from_rfc3339(end);

    if let (Ok(s), Ok(e)) = (start_dt, end_dt) {
        let duration = e.signed_duration_since(s);
        let minutes = duration.num_minutes();
        let hours = minutes / 60;
        if hours > 0 {
            format!("{}h{:02}m", hours, minutes % 60)
        } else {
            format!("{minutes}m")
        }
    } else {
        String::from("--")
    }
}
