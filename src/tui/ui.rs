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

    if app.input_mode == InputMode::CommandPalette {
        draw_command_palette(frame, app);
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
            "Tab:cycle view  n:task  s:session  q:quit",
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

    // Right: session detail (top) + task queue (bottom)
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(main[1]);

    draw_session_detail(frame, app, right[0]);
    draw_task_queue(frame, app, right[1]);

    // Status bar
    let status = if app.input_mode == InputMode::NewTask {
        Line::from(vec![
            Span::styled(" New task: ", Style::default().fg(Color::Yellow)),
            Span::raw(&app.input_buffer),
            Span::styled("█", Style::default().fg(Color::Yellow)),
            Span::styled(
                "  (Enter to create, Esc to cancel)",
                Style::default().fg(Color::DarkGray),
            ),
        ])
    } else if app.input_mode == InputMode::NewSession {
        Line::from(vec![
            Span::styled(" Branch name: ", Style::default().fg(Color::Green)),
            Span::raw(&app.input_buffer),
            Span::styled("█", Style::default().fg(Color::Green)),
            Span::styled(
                "  (Enter to create session, Esc to cancel)",
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
                format!(" ◐ {needs_attention} task(s) need your attention "),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )])
        } else {
            Line::from(Span::styled(
                " 1:projects  2:sessions  3:tasks  j/k:navigate",
                Style::default().fg(Color::DarkGray),
            ))
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
        let msg = Paragraph::new("  No projects yet.\n  Use `claustre add-project`")
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

    let block = Block::default()
        .title(" Task Queue ")
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
    let status = Line::from(Span::styled(
        " j/k:navigate  Tab:cycle view",
        Style::default().fg(Color::DarkGray),
    ));
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

    let status = match app.input_mode {
        InputMode::SkillSearch => {
            if app.search_results.is_empty() {
                Line::from(vec![
                    Span::styled(" Search: ", Style::default().fg(Color::Yellow)),
                    Span::raw(&app.input_buffer),
                    Span::styled("█", Style::default().fg(Color::Yellow)),
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
            Span::styled("█", Style::default().fg(Color::Green)),
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

fn format_tokens(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
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
