use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use crate::store::{ClaudeStatus, TaskStatus};

use super::app::{App, Focus, InputMode, View};

pub fn draw(frame: &mut Frame, app: &App) {
    match app.view {
        View::Active => draw_active(frame, app),
        View::History => draw_history(frame, app),
        View::Skills => draw_skills(frame, app),
    }

    // Floating panel overlays
    match app.input_mode {
        InputMode::CommandPalette => draw_command_palette(frame, app),
        InputMode::NewTask => draw_task_form_panel(frame, app, " New Task "),
        InputMode::EditTask => draw_task_form_panel(frame, app, " Edit Task "),
        InputMode::NewProject => draw_new_project_panel(frame, app),
        InputMode::NewSession => draw_new_session_panel(frame, app),
        InputMode::HelpOverlay => draw_help_overlay(frame, app),
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

    // Top bar
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
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
            "Tab:cycle  a:project  n:task  s:session  l:launch  q:quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), outer[0]);

    // Main area: left panel | right panel
    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[1]);

    // Left: project list
    draw_projects(frame, app, main[0]);

    // Right: usage bars (top) + session detail (middle) + task queue (bottom)
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(if app.rate_limit_state.is_rate_limited {
                6
            } else {
                4
            }),
            Constraint::Percentage(35),
            Constraint::Min(4),
        ])
        .split(main[1]);

    draw_usage_bars(frame, app, right[0]);
    draw_session_detail(frame, app, right[1]);
    draw_task_queue(frame, app, right[2]);

    // Status bar
    let status = if let Some(ref msg) = app.toast_message {
        let color = match app.toast_style {
            super::app::ToastStyle::Info => Color::Cyan,
            super::app::ToastStyle::Success => Color::Green,
            super::app::ToastStyle::Error => Color::Red,
        };
        Line::from(Span::styled(
            format!(" {msg} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
    } else if app.input_mode == InputMode::ConfirmDelete {
        Line::from(vec![
            Span::styled(
                format!(" Delete '{}'? ", app.confirm_target),
                Style::default().fg(Color::Red),
            ),
            Span::styled(
                "(y: confirm, Esc: cancel)",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else if app.input_mode == InputMode::TaskFilter {
        Line::from(vec![
            Span::styled(" /", Style::default().fg(Color::Yellow)),
            Span::raw(&app.task_filter),
            Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
            Span::styled(
                "  Enter:apply  Esc:clear",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else {
        let needs_attention = app
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::InReview)
            .count();
        if needs_attention > 0 {
            Line::from(vec![Span::styled(
                format!(" {needs_attention} task(s) need your attention "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )])
        } else {
            let hints = match app.focus {
                Focus::Projects => " a:add  x:remove  n:task  s:session  j/k:nav  ?:help",
                Focus::Sessions => " Enter:goto  d:teardown  s:new  j/k:nav  ?:help",
                Focus::Tasks => {
                    " n:new  e:edit  l:launch  r:review  x:del  /:filter  J/K:reorder  ?:help"
                }
            };
            Line::from(Span::styled(hints, Style::default().fg(Color::DarkGray)))
        }
    };
    frame.render_widget(Paragraph::new(status), outer[2]);
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
    let focused = app.focus == Focus::Sessions;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Session Detail ")
        .borders(Borders::ALL)
        .border_style(border_style);

    if app.sessions.is_empty() {
        let msg = Paragraph::new("  No active sessions")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    if let Some(session) = app.selected_session() {
        let status_color = match session.claude_status {
            ClaudeStatus::Working => Color::Green,
            ClaudeStatus::WaitingForInput => Color::Yellow,
            ClaudeStatus::Error => Color::Red,
            ClaudeStatus::Done => Color::Blue,
            ClaudeStatus::Idle => Color::DarkGray,
        };

        // Find the current task for this session
        let current_task = app.tasks.iter().find(|t| {
            t.session_id.as_deref() == Some(&session.id) && t.status == TaskStatus::InProgress
        });

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

        lines.push(Line::from(vec![
            Span::styled("  Last activity: ", Style::default().fg(Color::DarkGray)),
            Span::styled(&session.last_activity_at, Style::default().fg(Color::White)),
        ]));

        if let Some(task) = current_task {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("  Task: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&task.title, Style::default().fg(Color::White)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  Mode: ", Style::default().fg(Color::DarkGray)),
                Span::styled(task.mode.as_str(), Style::default().fg(Color::White)),
            ]));
        }

        let detail = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        frame.render_widget(detail, area);
    } else {
        let msg = Paragraph::new("  Select a session")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
    }
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
            spans.push(Span::styled(&task.title, Style::default().fg(Color::White)));
            spans.push(Span::styled(
                format!("  {}", task.status.as_str()),
                status_style,
            ));

            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_history(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(size);

    // Title bar
    let title = Line::from(vec![
        Span::styled(
            " claustre — history ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("                              "),
        Span::styled(
            "Tab:cycle view  q:quit",
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), outer[0]);

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[1]);

    // Left: project list (simplified)
    draw_history_projects(frame, app, main[0]);

    // Right: stats (top) + completed tasks (bottom)
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(main[1]);

    draw_project_stats(frame, app, right[0]);
    draw_completed_tasks(frame, app, right[1]);

    // Status bar
    let status = if let Some(ref msg) = app.toast_message {
        let color = match app.toast_style {
            super::app::ToastStyle::Info => Color::Cyan,
            super::app::ToastStyle::Success => Color::Green,
            super::app::ToastStyle::Error => Color::Red,
        };
        Line::from(Span::styled(
            format!(" {msg} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(
            " j/k:navigate  Tab:cycle view",
            Style::default().fg(Color::DarkGray),
        ))
    };
    frame.render_widget(Paragraph::new(status), outer[2]);
}

fn draw_history_projects(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Projects ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let items: Vec<ListItem> = app
        .projects
        .iter()
        .enumerate()
        .map(|(i, project)| {
            let mut spans = vec![];
            if i == app.project_index {
                spans.push(Span::styled("▸ ", Style::default().fg(Color::Cyan)));
                spans.push(Span::styled(
                    &project.name,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ));
            } else {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    &project.name,
                    Style::default().fg(Color::White),
                ));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_project_stats(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Project Stats ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    if let Some(project) = app.selected_project()
        && let Ok(stats) = app.store.project_stats(&project.id)
    {
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
        return;
    }

    let msg = Paragraph::new("  Select a project")
        .style(Style::default().fg(Color::DarkGray))
        .block(block);
    frame.render_widget(msg, area);
}

fn draw_completed_tasks(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Completed Tasks ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let done_tasks: Vec<&crate::store::Task> = app
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .collect();

    if done_tasks.is_empty() {
        let msg = Paragraph::new("  No completed tasks yet")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = done_tasks
        .iter()
        .map(|task| {
            let time = if let (Some(start), Some(end)) = (&task.started_at, &task.completed_at) {
                format_task_duration(start, end)
            } else {
                String::from("--")
            };

            let tokens = format_tokens(task.input_tokens + task.output_tokens);

            ListItem::new(Line::from(vec![
                Span::styled("  ✓ ", Style::default().fg(Color::Green)),
                Span::styled(&task.title, Style::default().fg(Color::White)),
                Span::styled(
                    format!("  {time}  {tokens}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_skills(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(size);

    let scope_label = if app.skill_scope_global {
        "global"
    } else {
        "project"
    };
    let title = Line::from(vec![
        Span::styled(
            " claustre — skills ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("                              "),
        Span::styled(
            format!("Tab:active  g:scope [{scope_label}]  q:quit"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    frame.render_widget(Paragraph::new(title), outer[0]);

    let main = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(outer[1]);

    if app.input_mode == InputMode::SkillSearch {
        draw_skill_search(frame, app, main[0]);
    } else {
        draw_installed_skills(frame, app, main[0]);
    }

    draw_skill_detail(frame, app, main[1]);

    let status = if let Some(ref msg) = app.toast_message {
        let color = match app.toast_style {
            super::app::ToastStyle::Info => Color::Cyan,
            super::app::ToastStyle::Success => Color::Green,
            super::app::ToastStyle::Error => Color::Red,
        };
        Line::from(Span::styled(
            format!(" {msg} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
    } else {
        match app.input_mode {
            InputMode::SkillSearch => {
                if app.search_results.is_empty() {
                    Line::from(vec![
                        Span::styled(" Search: ", Style::default().fg(Color::Yellow)),
                        Span::raw(&app.input_buffer),
                        Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
                        Span::styled(
                            "  (Enter to search, Esc to cancel)",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(
                            format!(" {} results ", app.search_results.len()),
                            Style::default().fg(Color::Green),
                        ),
                        Span::styled(
                            " j/k:navigate  Enter:install  Esc:back",
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])
                }
            }
            InputMode::SkillAdd => Line::from(vec![
                Span::styled(" Package: ", Style::default().fg(Color::Green)),
                Span::raw(&app.input_buffer),
                Span::styled("\u{2588}", Style::default().fg(Color::Green)),
                Span::styled(
                    "  (owner/repo@skill, Enter to install, Esc to cancel)",
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            _ => {
                if app.skill_status_message.is_empty() {
                    Line::from(Span::styled(
                        " f:find  a:add  x:remove  u:update  g:scope  j/k:navigate",
                        Style::default().fg(Color::DarkGray),
                    ))
                } else {
                    Line::from(Span::styled(
                        format!(" {} ", app.skill_status_message),
                        Style::default().fg(Color::Yellow),
                    ))
                }
            }
        }
    };
    frame.render_widget(Paragraph::new(status), outer[2]);
}

fn draw_installed_skills(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Installed Skills ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    if app.installed_skills.is_empty() {
        let msg = Paragraph::new("  No skills installed.\n  Press 'f' to find or 'a' to add.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let mut items: Vec<ListItem> = Vec::new();
    let mut current_scope: Option<&crate::skills::SkillScope> = None;

    for (i, skill) in app.installed_skills.iter().enumerate() {
        let scope_changed = current_scope != Some(&skill.scope);
        if scope_changed {
            let header = match &skill.scope {
                crate::skills::SkillScope::Global => "── Global ──".to_string(),
                crate::skills::SkillScope::Project(p) => {
                    let name = std::path::Path::new(p)
                        .file_name()
                        .map_or_else(|| p.clone(), |n| n.to_string_lossy().to_string());
                    format!("── {name} ──")
                }
            };
            items.push(ListItem::new(Line::from(Span::styled(
                format!("  {header}"),
                Style::default().fg(Color::DarkGray),
            ))));
            current_scope = Some(&skill.scope);
        }

        let is_selected = i == app.skill_index;
        let style = if is_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        let prefix = if is_selected { "▸ " } else { "  " };
        let prefix_style = if is_selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };

        items.push(ListItem::new(Line::from(vec![
            Span::styled(prefix, prefix_style),
            Span::styled(&skill.name, style),
        ])));
    }

    let list = List::new(items).block(block);
    frame.render_widget(list, area);
}

fn draw_skill_search(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Search Skills ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 2 {
        return;
    }

    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Yellow)),
        Span::raw(&app.input_buffer),
        Span::styled("█", Style::default().fg(Color::Yellow)),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);

    if !app.search_results.is_empty() {
        let results_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        );

        let items: Vec<ListItem> = app
            .search_results
            .iter()
            .enumerate()
            .map(|(i, result)| {
                let is_selected = i == app.skill_index;
                let style = if is_selected {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                let prefix = if is_selected { "▸ " } else { "  " };

                ListItem::new(Line::from(vec![
                    Span::styled(prefix, style),
                    Span::styled(&result.package, style),
                ]))
            })
            .collect();

        frame.render_widget(List::new(items), results_area);
    } else if !app.input_buffer.is_empty() {
        let msg_area = Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(1),
        );
        let msg =
            Paragraph::new("  Press Enter to search").style(Style::default().fg(Color::DarkGray));
        frame.render_widget(msg, msg_area);
    }
}

fn draw_skill_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Skill Detail ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    if app.input_mode == InputMode::SkillSearch
        && !app.search_results.is_empty()
        && let Some(result) = app.search_results.get(app.skill_index)
    {
        let lines = vec![
            Line::from(vec![
                Span::styled("  Repo: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&result.owner_repo, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Skill: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&result.skill_name, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  URL: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&result.url, Style::default().fg(Color::Blue)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("  Install: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    format!("npx skills add {}", result.package),
                    Style::default().fg(Color::Green),
                ),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  Press Enter to install",
                Style::default().fg(Color::Yellow),
            )),
        ];

        let detail = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        frame.render_widget(detail, area);
        return;
    }

    if let Some(skill) = app.installed_skills.get(app.skill_index) {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("  Name: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&skill.name, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled("  Path: ", Style::default().fg(Color::DarkGray)),
                Span::styled(&skill.path, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("  Agents: ", Style::default().fg(Color::DarkGray)),
                Span::styled(skill.agents.join(", "), Style::default().fg(Color::White)),
            ]),
            Line::from(""),
        ];

        for md_line in app.skill_detail_content.lines().take(20) {
            lines.push(Line::from(Span::styled(
                format!("  {md_line}"),
                Style::default().fg(Color::White),
            )));
        }

        if app.skill_detail_content.lines().count() > 20 {
            lines.push(Line::from(Span::styled(
                "  ...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        let detail = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        frame.render_widget(detail, area);
    } else {
        let msg = Paragraph::new("  No skill selected")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
    }
}

fn draw_task_form_panel(frame: &mut Frame, app: &App, title: &str) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));

    // Calculate description text to estimate wrapped line count
    let desc_text = if app.new_task_field == 1 {
        format!("{}\u{2588}", app.input_buffer)
    } else {
        app.new_task_description.clone()
    };

    // inner width = width - 2 (borders), prefix "  Description: " = 15 chars
    let inner_width = width.saturating_sub(2) as usize;
    let desc_total_chars = 15 + desc_text.len();
    let desc_lines = if inner_width > 0 {
        (desc_total_chars.div_ceil(inner_width)).max(1) as u16
    } else {
        1
    };

    // Layout (inner rows): 0=pad, 1=title, 2=pad, 3..3+desc=description,
    //   3+desc=pad, 4+desc=mode, 5+desc=pad, 6+desc=hints
    // Inner height = 7 + desc_lines, outer = inner + 2 (borders)
    let height = (9u16 + desc_lines).min(area.height.saturating_sub(4));
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

    if inner.height < 7 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(Color::DarkGray);
    let highlight = Style::default().fg(Color::Yellow);
    let val_style = Style::default().fg(Color::White);

    // Field 0: Title
    let (label_s, val) = if app.new_task_field == 0 {
        (highlight, format!("{}\u{2588}", app.input_buffer))
    } else {
        (dim, app.new_task_title.clone())
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Title:       ", label_s),
            Span::styled(val, val_style),
        ])),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );

    // Field 1: Description (wraps to multiple lines)
    let (label_s, val) = if app.new_task_field == 1 {
        (highlight, format!("{}\u{2588}", app.input_buffer))
    } else {
        (dim, app.new_task_description.clone())
    };
    let desc_height = desc_lines.min(inner.height.saturating_sub(6));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Description: ", label_s),
            Span::styled(val, val_style),
        ]))
        .wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y + 3, inner.width, desc_height),
    );

    // Shift remaining fields down by extra description lines
    let extra = desc_height.saturating_sub(1);

    // Field 2: Mode
    let label_s = if app.new_task_field == 2 {
        highlight
    } else {
        dim
    };
    let mode_s = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let arrow_hint = if app.new_task_field == 2 {
        "  (←/→ toggle)"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Mode:        ", label_s),
            Span::styled(app.new_task_mode.as_str(), mode_s),
            Span::styled(arrow_hint, dim),
        ])),
        Rect::new(inner.x, inner.y + 5 + extra, inner.width, 1),
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
        Rect::new(inner.x, inner.y + 7 + extra, inner.width, 1),
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
        Rect::new(inner.x, inner.y + hint_y_offset, inner.width, 1),
    );
}

fn draw_new_session_panel(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = 7u16.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" New Session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 3 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(Color::DarkGray);
    let highlight = Style::default().fg(Color::Green);
    let val_style = Style::default().fg(Color::White);

    // Branch name field
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Branch: ", highlight),
            Span::styled(format!("{}\u{2588}", app.input_buffer), val_style),
        ])),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );

    // Hints
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Enter", highlight),
            Span::styled(":create  ", dim),
            Span::styled("Esc", highlight),
            Span::styled(":cancel", dim),
        ])),
        Rect::new(inner.x, inner.y + 3, inner.width, 1),
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

fn usage_bar_line(label: &str, pct: f64, reset: Option<&str>, total_width: usize) -> Line<'static> {
    let pct_clamped = pct.clamp(0.0, 100.0);

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

fn draw_help_overlay(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = 24u16.min(area.height.saturating_sub(4));
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

    let lines: Vec<Line<'_>> = match app.view {
        View::Active => vec![
            help_section("Global"),
            help_line("  Tab", "Cycle views"),
            help_line("  Ctrl+P", "Command palette"),
            help_line("  1/2/3", "Focus projects/sessions/tasks"),
            help_line("  j/k", "Navigate up/down"),
            help_line("  q", "Quit"),
            Line::from(""),
            help_section("Projects"),
            help_line("  a", "Add project"),
            help_line("  x", "Remove project"),
            Line::from(""),
            help_section("Sessions"),
            help_line("  s", "New session"),
            help_line("  Enter", "Go to Zellij tab"),
            help_line("  d", "Teardown session"),
            Line::from(""),
            help_section("Tasks"),
            help_line("  n", "New task"),
            help_line("  e", "Edit task (pending only)"),
            help_line("  l", "Launch task"),
            help_line("  r", "Review (mark done)"),
            help_line("  x", "Delete task (pending only)"),
            help_line("  /", "Search/filter tasks"),
            help_line("  Shift+J/K", "Reorder tasks"),
        ],
        View::History => vec![
            help_line("  j/k", "Navigate projects"),
            help_line("  Tab", "Cycle views"),
            help_line("  q", "Quit"),
        ],
        View::Skills => vec![
            help_line("  j/k", "Navigate skills"),
            help_line("  f", "Find skills"),
            help_line("  a", "Add skill"),
            help_line("  x", "Remove skill"),
            help_line("  u", "Update skills"),
            help_line("  g", "Toggle scope"),
            help_line("  Tab", "Cycle views"),
            help_line("  q", "Quit"),
        ],
    };

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
