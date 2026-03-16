//! Sprint board rendering -- Kanban columns showing GitHub issues.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use crate::github::GitHubIssue;

use super::super::app::{App, InputMode};
use super::super::theme::Theme;

/// Draw the sprint board view within the given area.
/// Shows issues grouped into Kanban columns.
pub(super) fn draw_board(frame: &mut Frame, app: &App, area: Rect) {
    let show_filter_bar = app.input_mode == InputMode::BoardFilter || !app.board_filter.is_empty();

    if show_filter_bar {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(1),
                Constraint::Min(0),
            ])
            .split(area);
        draw_board_header(frame, app, layout[0]);
        draw_filter_bar(frame, app, layout[1]);
        draw_board_columns(frame, app, layout[2]);
    } else {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area);
        draw_board_header(frame, app, layout[0]);
        draw_board_columns(frame, app, layout[1]);
    }
}

fn draw_board_header(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;

    // Get project name
    let project_name = app.projects.get(app.project_index).map_or("", |p| &p.name);

    let milestone_text = app
        .board_milestone_filter
        .as_deref()
        .map_or_else(|| "All Issues".to_string(), |m| format!("Sprint: {m}"));

    let total_issues: usize = app.board_issues.iter().map(Vec::len).sum();

    let header = Line::from(vec![
        Span::styled(
            " Sprint Board ",
            Style::default()
                .fg(theme.text_accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" \u{2502} ", Style::default().fg(theme.border_unfocused)),
        Span::styled(project_name, Style::default().fg(theme.text_primary)),
        Span::styled(" \u{2502} ", Style::default().fg(theme.border_unfocused)),
        Span::styled(&milestone_text, Style::default().fg(theme.status_in_review)),
        Span::styled(
            format!(" ({total_issues} issues)"),
            Style::default().fg(theme.text_secondary),
        ),
    ]);

    let hints = Line::from(vec![
        Span::styled("  h/l", Style::default().fg(theme.text_accent)),
        Span::styled(":column  ", Style::default().fg(theme.text_secondary)),
        Span::styled("j/k", Style::default().fg(theme.text_accent)),
        Span::styled(":issue  ", Style::default().fg(theme.text_secondary)),
        Span::styled("Enter", Style::default().fg(theme.text_accent)),
        Span::styled(":create task  ", Style::default().fg(theme.text_secondary)),
        Span::styled("o", Style::default().fg(theme.text_accent)),
        Span::styled(":open  ", Style::default().fg(theme.text_secondary)),
        Span::styled("/", Style::default().fg(theme.text_accent)),
        Span::styled(":filter  ", Style::default().fg(theme.text_secondary)),
        Span::styled("m", Style::default().fg(theme.text_accent)),
        Span::styled(":milestone  ", Style::default().fg(theme.text_secondary)),
        Span::styled("R", Style::default().fg(theme.text_accent)),
        Span::styled(":refresh  ", Style::default().fg(theme.text_secondary)),
        Span::styled("b/Esc", Style::default().fg(theme.text_accent)),
        Span::styled(":back", Style::default().fg(theme.text_secondary)),
    ]);

    frame.render_widget(
        Paragraph::new(header),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(hints),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

fn draw_filter_bar(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let is_editing = app.input_mode == InputMode::BoardFilter;

    let label_style = Style::default().fg(if is_editing {
        theme.text_accent
    } else {
        theme.text_secondary
    });
    let text_style = Style::default().fg(theme.text_primary);

    let filtered_total: usize = app.board_issues.iter().map(Vec::len).sum();
    let all_total: usize = app.board_all_issues.iter().map(Vec::len).sum();

    let mut spans = vec![
        Span::styled(" / ", label_style),
        Span::styled(&app.board_filter, text_style),
    ];

    if is_editing {
        spans.push(Span::styled(
            "\u{258f}",
            Style::default().fg(theme.text_accent),
        ));
    }

    if !app.board_filter.is_empty() {
        spans.push(Span::styled(
            format!("  ({filtered_total}/{all_total})"),
            Style::default().fg(theme.text_secondary),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_board_columns(frame: &mut Frame, app: &App, area: Rect) {
    let theme = &app.theme;
    let col_count = app.board_columns.len();

    if col_count == 0 || area.width < 4 || area.height < 3 {
        return;
    }

    // Show error message if present
    if let Some(ref error) = app.board_error {
        let msg = format!(" Error: {error} ");
        let x = area.x + (area.width.saturating_sub(msg.len() as u16)) / 2;
        let y = area.y + area.height / 2;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(theme.status_error),
            ))),
            Rect::new(x.max(area.x), y.max(area.y), area.width, 1),
        );
        return;
    }

    // Show empty state if no issues found
    let total_issues: usize = app.board_issues.iter().map(Vec::len).sum();
    if total_issues == 0 {
        let msg = " No issues found ";
        let x = area.x + (area.width.saturating_sub(msg.len() as u16)) / 2;
        let y = area.y + area.height / 2;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                msg,
                Style::default().fg(theme.text_secondary),
            ))),
            Rect::new(x.max(area.x), y.max(area.y), area.width, 1),
        );
        return;
    }

    // Split area into equal columns
    let constraints: Vec<Constraint> = (0..col_count)
        .map(|_| Constraint::Ratio(1, col_count as u32))
        .collect();
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(area);

    for (col_idx, col_area) in columns.iter().enumerate() {
        let is_selected_col = col_idx == app.board_column_index;
        let col_name = app.board_columns.get(col_idx).map_or("", String::as_str);
        let empty = Vec::new();
        let issues = app.board_issues.get(col_idx).unwrap_or(&empty);
        let issue_count = issues.len();

        // Column border colour
        let border_color = if is_selected_col {
            theme.border_focused
        } else {
            theme.border_unfocused
        };

        // Column header with count badge
        let status_color = column_status_color(col_idx, col_count, theme);

        let block = Block::default()
            .title(Line::from(vec![
                Span::styled(
                    format!(" {col_name} "),
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{issue_count} "),
                    Style::default().fg(theme.text_secondary),
                ),
            ]))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color));

        let inner = block.inner(*col_area);
        frame.render_widget(block, *col_area);

        // Render issues in this column
        if inner.height == 0 || inner.width < 4 {
            continue;
        }

        // Calculate scroll offset for this column
        let visible_rows = inner.height as usize;
        let scroll_offset = if is_selected_col && app.board_issue_index >= visible_rows {
            app.board_issue_index.saturating_sub(visible_rows - 1)
        } else {
            0
        };

        for (i, issue) in issues.iter().enumerate().skip(scroll_offset) {
            let row = (i - scroll_offset) as u16;
            if row >= inner.height {
                break;
            }

            let is_selected = is_selected_col && i == app.board_issue_index;
            draw_issue_card(
                frame,
                theme,
                issue,
                is_selected,
                Rect::new(inner.x, inner.y + row, inner.width, 1),
            );
        }

        // Show scroll indicator if there are more issues below
        if issues.len() > visible_rows + scroll_offset {
            let remaining = issues.len() - visible_rows - scroll_offset;
            let indicator = format!(" \u{25bc} +{remaining} more ");
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    indicator,
                    Style::default().fg(theme.text_secondary),
                ))),
                Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1),
            );
        }
    }
}

fn draw_issue_card(
    frame: &mut Frame,
    theme: &Theme,
    issue: &GitHubIssue,
    is_selected: bool,
    area: Rect,
) {
    if area.width < 4 {
        return;
    }

    let bg_style = if is_selected {
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme.text_primary)
    };

    let prefix = if is_selected { "\u{25b8} " } else { "  " };
    let number = format!("#{} ", issue.number);

    // Truncate title to fit available width
    let prefix_len = prefix.len() + number.len();
    let max_title_len = (area.width as usize).saturating_sub(prefix_len + 1);
    let title = if issue.title.len() > max_title_len {
        let cut = max_title_len.saturating_sub(3).min(issue.title.len());
        // Find a safe char boundary at or before the cut point
        let boundary = issue.title[..cut]
            .char_indices()
            .map(|(i, _)| i)
            .next_back()
            .unwrap_or(0);
        format!("{}...", &issue.title[..boundary])
    } else {
        issue.title.clone()
    };

    let spans = vec![
        Span::styled(
            prefix,
            if is_selected {
                Style::default().fg(theme.text_accent)
            } else {
                Style::default().fg(theme.text_secondary)
            },
        ),
        Span::styled(number, Style::default().fg(theme.text_secondary)),
        Span::styled(title, bg_style),
    ];

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Map column index to a status colour for the header.
///
/// First column uses the pending/backlog colour, the last column uses the done
/// colour, the second-to-last uses the in-review colour, and everything in
/// between uses the working colour.
fn column_status_color(col_idx: usize, col_count: usize, theme: &Theme) -> ratatui::style::Color {
    if col_count == 0 {
        return theme.text_primary;
    }
    if col_idx == 0 {
        theme.status_pending
    } else if col_idx == col_count - 1 {
        theme.status_done
    } else if col_idx == col_count - 2 {
        theme.status_in_review
    } else {
        theme.status_working
    }
}

/// Draw the milestone filter overlay (centred popup).
pub(super) fn draw_milestone_overlay(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 40u16.min(area.width.saturating_sub(4));
    let height = (app.board_milestones.len() as u16 + 5)
        .min(area.height.saturating_sub(4))
        .max(6);
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Select Sprint/Milestone ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.form_border_project));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 3 {
        return;
    }

    let dim = Style::default().fg(app.theme.text_secondary);
    let highlight = Style::default().fg(app.theme.text_accent);

    // "All" option (no milestone filter)
    let all_selected = app.board_milestone_index == 0;
    let all_style = if all_selected {
        Style::default()
            .fg(app.theme.text_accent)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.theme.text_primary)
    };
    let all_prefix = if all_selected { "\u{25b8} " } else { "  " };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(all_prefix, all_style),
            Span::styled("All Issues", all_style),
        ])),
        Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1),
    );

    // Milestone items
    for (i, ms) in app.board_milestones.iter().enumerate() {
        let row = (i + 1) as u16 + 1;
        if inner.y + row >= inner.y + inner.height - 1 {
            break;
        }
        let is_selected = i + 1 == app.board_milestone_index;
        let style = if is_selected {
            Style::default()
                .fg(app.theme.text_accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(app.theme.text_primary)
        };
        let prefix = if is_selected { "\u{25b8} " } else { "  " };
        let state_badge = if ms.state == "open" {
            "\u{25cf}"
        } else {
            "\u{25cb}"
        };
        let due = ms.due_on.as_deref().map_or(String::new(), |d| {
            format!(" (due {})", &d[..10.min(d.len())])
        });

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(
                    format!("{state_badge} "),
                    if ms.state == "open" {
                        Style::default().fg(app.theme.status_working)
                    } else {
                        Style::default().fg(app.theme.text_secondary)
                    },
                ),
                Span::styled(&ms.title, style),
                Span::styled(due, dim),
            ])),
            Rect::new(inner.x + 1, inner.y + row, inner.width.saturating_sub(2), 1),
        );
    }

    // Hints at the bottom
    let hint_y = inner.y + inner.height - 1;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  j/k", highlight),
            Span::styled(":navigate  ", dim),
            Span::styled("Enter", highlight),
            Span::styled(":select  ", dim),
            Span::styled("Esc", highlight),
            Span::styled(":cancel", dim),
        ])),
        Rect::new(inner.x, hint_y, inner.width, 1),
    );
}
