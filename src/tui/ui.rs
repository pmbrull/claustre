use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, HighlightSpacing, List, ListItem, Paragraph, Wrap},
};

use crate::pty::{LayoutNode, PaneId, SplitDirection, TerminalWidget};
use crate::store::{ClaudeStatus, TaskStatus};

use super::app::{App, Focus, InputMode, Tab};
use super::form::{format_with_cursor, measure_wrapped_height, render_hints, render_modal};
use super::theme::Theme;

/// Returns an animated spinner character that cycles based on wall clock time.
fn spinner_char() -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const FRAME_MS: u128 = 250;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let idx = (ms / FRAME_MS) as usize % FRAMES.len();
    FRAMES[idx]
}

/// If a toast is active, return a styled `Line` for it; otherwise `None`.
fn toast_line(app: &App) -> Option<Line<'static>> {
    let msg = app.toast_message.as_ref()?;
    Some(Line::from(Span::styled(
        format!(" {msg} "),
        app.theme.toast_style(app.toast_style),
    )))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    // If on a session tab, render the terminal view
    if app.active_tab > 0 {
        draw_session_tab(frame, app);
        return;
    }

    // Tab bar (only show if there are session tabs)
    if app.tabs.len() > 1 {
        let size = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(size);
        draw_tab_bar(frame, app, outer[0]);
        // Render the dashboard in the remaining area
        let sub_frame_area = outer[1];
        draw_active_in_area(frame, app, sub_frame_area);
    } else {
        draw_active(frame, app);
    }

    // Floating panel overlays
    match app.input_mode {
        InputMode::CommandPalette => draw_command_palette(frame, app),
        InputMode::NewTask => draw_task_form_panel(frame, app, " New Task "),
        InputMode::EditTask => draw_task_form_panel(frame, app, " Edit Task "),
        InputMode::NewProject => draw_new_project_panel(frame, app),
        InputMode::HelpOverlay => draw_help_overlay(frame, app),
        InputMode::SubtaskPanel => draw_subtask_panel(frame, app),
        InputMode::SkillPanel => draw_skill_panel(frame, app),
        InputMode::SkillSearch | InputMode::SkillAdd => {
            draw_skill_panel(frame, app);
            if app.input_mode == InputMode::SkillSearch {
                draw_skill_search_overlay(frame, app);
            } else {
                draw_skill_add_overlay(frame, app);
            }
        }
        _ => {}
    }
}

/// Minimum display width for a tab label (including padding spaces).
const MIN_TAB_WIDTH: usize = 12;
/// Width of the separator between tabs: " | ".
const TAB_SEPARATOR_WIDTH: usize = 3;
/// Width of a single overflow indicator: " ◀" or "▶ ".
const OVERFLOW_INDICATOR_WIDTH: usize = 2;

/// A single entry in the computed tab layout.
pub struct TabLayoutEntry {
    /// Index into `App::tabs`.
    pub tab_index: usize,
    /// The (possibly truncated) display label.
    pub display_label: String,
    /// Horizontal start position within the tab bar.
    pub x_start: u16,
    /// Display width of this entry.
    pub width: u16,
}

/// Result of computing which tabs are visible and how they should be displayed.
pub struct TabLayout {
    pub entries: Vec<TabLayoutEntry>,
    /// True if there are hidden tabs to the left.
    pub has_left_overflow: bool,
    /// True if there are hidden tabs to the right.
    pub has_right_overflow: bool,
}

/// Truncate `s` to at most `max_chars` characters, appending `…` if truncated.
fn truncate_label(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    // Reserve 1 char for the ellipsis
    let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}\u{2026}")
}

/// Compute the tab layout: which tabs to show, their display labels, and positions.
///
/// Algorithm:
/// 1. If all full labels fit, use them as-is.
/// 2. Otherwise, truncate labels proportionally to fit.
/// 3. If even minimum-width labels don't all fit, show a sliding window around
///    the active tab with overflow indicators.
pub fn compute_tab_layout(tabs: &[Tab], active_tab: usize, available_width: u16) -> TabLayout {
    let width = available_width as usize;
    let tab_count = tabs.len();

    if tab_count == 0 {
        return TabLayout {
            entries: vec![],
            has_left_overflow: false,
            has_right_overflow: false,
        };
    }

    // Collect full labels (with padding spaces)
    let full_labels: Vec<String> = tabs
        .iter()
        .map(|tab| match tab {
            Tab::Dashboard => " Dashboard ".to_string(),
            Tab::Session { label, .. } => format!(" {label} "),
        })
        .collect();

    let total_separator_width = if tab_count > 1 {
        (tab_count - 1) * TAB_SEPARATOR_WIDTH
    } else {
        0
    };
    let total_full_width: usize =
        full_labels.iter().map(|l| l.chars().count()).sum::<usize>() + total_separator_width;

    // Case 1: Everything fits
    if total_full_width <= width {
        return build_layout(&full_labels, 0, tab_count, width);
    }

    // Case 2: Truncate labels to fit
    let available_for_labels = width.saturating_sub(total_separator_width);
    let max_per_tab = available_for_labels / tab_count;

    if max_per_tab >= MIN_TAB_WIDTH {
        let truncated: Vec<String> = full_labels
            .iter()
            .map(|l| truncate_label(l, max_per_tab))
            .collect();
        return build_layout(&truncated, 0, tab_count, width);
    }

    // Case 3: Sliding window - even minimum truncation doesn't fit all tabs
    // Find how many tabs we can show at MIN_TAB_WIDTH
    let indicator_space = OVERFLOW_INDICATOR_WIDTH * 2; // Reserve space for both indicators
    let usable_width = width.saturating_sub(indicator_space);
    let space_per_tab_with_sep = MIN_TAB_WIDTH + TAB_SEPARATOR_WIDTH;
    // At least 1 tab (the active one)
    let max_visible = if space_per_tab_with_sep > 0 {
        ((usable_width + TAB_SEPARATOR_WIDTH) / space_per_tab_with_sep).max(1)
    } else {
        1
    };

    // Center the window around the active tab
    let half = max_visible / 2;
    let mut start = active_tab.saturating_sub(half);
    let end = (start + max_visible).min(tab_count);
    // Adjust if we hit the right edge
    if end == tab_count && end - start < max_visible {
        start = end.saturating_sub(max_visible);
    }

    let has_left = start > 0;
    let has_right = end < tab_count;

    // Recalculate available width accounting for actual indicators needed
    let left_indicator = if has_left {
        OVERFLOW_INDICATOR_WIDTH
    } else {
        0
    };
    let right_indicator = if has_right {
        OVERFLOW_INDICATOR_WIDTH
    } else {
        0
    };
    let visible_count = end - start;
    let visible_separators = if visible_count > 1 {
        (visible_count - 1) * TAB_SEPARATOR_WIDTH
    } else {
        0
    };
    let label_budget = width
        .saturating_sub(left_indicator)
        .saturating_sub(right_indicator)
        .saturating_sub(visible_separators);
    let per_tab = (label_budget / visible_count).max(MIN_TAB_WIDTH);

    let truncated: Vec<String> = full_labels[start..end]
        .iter()
        .map(|l| truncate_label(l, per_tab))
        .collect();

    let mut entries = Vec::with_capacity(visible_count);
    let mut x = left_indicator as u16;
    for (i, label) in truncated.into_iter().enumerate() {
        let w = label.chars().count() as u16;
        entries.push(TabLayoutEntry {
            tab_index: start + i,
            display_label: label,
            x_start: x,
            width: w,
        });
        x += w;
        if i + 1 < visible_count {
            x += TAB_SEPARATOR_WIDTH as u16;
        }
    }

    TabLayout {
        entries,
        has_left_overflow: has_left,
        has_right_overflow: has_right,
    }
}

/// Build a `TabLayout` from a slice of labels (no overflow indicators).
fn build_layout(labels: &[String], start: usize, end: usize, _width: usize) -> TabLayout {
    let count = end - start;
    let mut entries = Vec::with_capacity(count);
    let mut x: u16 = 0;
    for (i, label) in labels[start..end].iter().enumerate() {
        let w = label.chars().count() as u16;
        entries.push(TabLayoutEntry {
            tab_index: start + i,
            display_label: label.clone(),
            x_start: x,
            width: w,
        });
        x += w;
        if i + 1 < count {
            x += TAB_SEPARATOR_WIDTH as u16;
        }
    }
    TabLayout {
        entries,
        has_left_overflow: false,
        has_right_overflow: false,
    }
}

/// Draw the tab bar across the top showing dashboard + session tabs.
fn draw_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let layout = compute_tab_layout(&app.tabs, app.active_tab, area.width);
    let mut spans = Vec::new();

    if layout.has_left_overflow {
        spans.push(Span::styled("\u{25c0} ", app.theme.tab_inactive_style()));
    }

    for (i, entry) in layout.entries.iter().enumerate() {
        let style = if entry.tab_index == app.active_tab {
            app.theme.tab_active_style()
        } else {
            app.theme.tab_inactive_style()
        };
        spans.push(Span::styled(&entry.display_label, style));
        if i + 1 < layout.entries.len() {
            spans.push(Span::raw(" | "));
        }
    }

    if layout.has_right_overflow {
        spans.push(Span::styled(" \u{25b6}", app.theme.tab_inactive_style()));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Draw the session terminal view with a dynamic pane layout tree.
fn draw_session_tab(frame: &mut Frame, app: &App) {
    let size = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(0),    // terminal area
            Constraint::Length(1), // hint bar
        ])
        .split(size);

    draw_tab_bar(frame, app, outer[0]);

    if let Some(Tab::Session {
        terminals, label, ..
    }) = app.tabs.get(app.active_tab)
    {
        render_layout_node(
            &terminals.layout,
            terminals,
            label,
            &app.theme,
            frame,
            outer[1],
        );
    }

    // Hint bar
    render_hints(
        frame,
        outer[2],
        &[
            ("  Ctrl+D", ": dashboard  "),
            ("Ctrl+H/L", ": switch pane  "),
            ("Ctrl+J/K", ": switch tab  "),
            ("Ctrl+B/N", ": split  "),
            ("Ctrl+W", ": close  "),
        ],
        Style::default().fg(app.theme.accent_secondary),
        Style::default(),
    );
}

/// Recursively render a layout node tree into the given area.
fn render_layout_node(
    node: &LayoutNode,
    terminals: &crate::pty::SessionTerminals,
    session_label: &str,
    theme: &super::theme::Theme,
    frame: &mut Frame,
    area: Rect,
) {
    match node {
        LayoutNode::Pane(id) => {
            render_single_pane(*id, terminals, session_label, theme, frame, area);
        }
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let dir = match direction {
                SplitDirection::Horizontal => Direction::Horizontal,
                SplitDirection::Vertical => Direction::Vertical,
            };
            let chunks = Layout::default()
                .direction(dir)
                .constraints([
                    Constraint::Percentage(*ratio),
                    Constraint::Percentage(100 - *ratio),
                ])
                .split(area);

            render_layout_node(first, terminals, session_label, theme, frame, chunks[0]);
            render_layout_node(second, terminals, session_label, theme, frame, chunks[1]);
        }
    }
}

/// Render a single terminal pane with border and title.
fn render_single_pane(
    id: PaneId,
    terminals: &crate::pty::SessionTerminals,
    session_label: &str,
    theme: &super::theme::Theme,
    frame: &mut Frame,
    area: Rect,
) {
    let Some(term) = terminals.terminal(id) else {
        return;
    };

    let is_focused = terminals.focused == id;
    let is_claude = id == terminals.claude_pane_id;

    let base_label = if is_claude {
        session_label.to_string()
    } else {
        terminals.label(id).to_string()
    };

    let scrollback = term.scrollback();
    let title = if scrollback > 0 {
        format!(" {base_label} [+{scrollback} lines] ")
    } else {
        format!(" {base_label} ")
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if is_focused {
            theme.focused_border()
        } else {
            theme.unfocused_border()
        });

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sel = terminals.selection.as_ref().filter(|s| s.pane == id);

    frame.render_widget(
        TerminalWidget::new(term.screen(), is_focused).with_selection(sel),
        inner,
    );
}

/// Draw the dashboard in a specific area (used when tab bar is present).
fn draw_active_in_area(frame: &mut Frame, app: &mut App, area: Rect) {
    // This delegates to the same rendering logic as draw_active but
    // within a sub-area. For simplicity, we render directly using frame.area()
    // since ratatui handles clipping. The tab bar has already consumed its row.
    draw_active_impl(frame, app, area);
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
        .border_style(Style::default().fg(app.theme.accent_primary));

    let inner = block.inner(palette_area);
    frame.render_widget(block, palette_area);

    if inner.height < 2 {
        return;
    }

    // Search input
    let input_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let cursor_pos = app.input_cursor.min(app.input_buffer.len());
    let (before, after) = app.input_buffer.split_at(cursor_pos);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(app.theme.accent_primary)),
        Span::raw(before.to_string()),
        Span::styled("\u{2588}", Style::default().fg(app.theme.accent_primary)),
        Span::raw(after.to_string()),
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
                    .fg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.text_primary)
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

fn draw_active(frame: &mut Frame, app: &mut App) {
    draw_active_impl(frame, app, frame.area());
}

fn draw_active_impl(frame: &mut Frame, app: &mut App, size: Rect) {
    // Check if we need a status line above the hints
    let needs_attention = app
        .tasks
        .iter()
        .filter(|t| t.status == TaskStatus::InReview)
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

    // Title bar
    let title = Line::from(vec![Span::styled(
        " claustre ",
        Style::default()
            .fg(app.theme.text_accent)
            .add_modifier(Modifier::BOLD),
    )]);
    frame.render_widget(Paragraph::new(title), outer[0]);

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
            " Enter:session  n:new  e:edit  s:sub  l:launch  k:kill  r:done  o:PR  d:del  i:skills  /:filter  J/K:reorder  ?:help  q:quit"
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

            // Show session statuses under the project
            let mut lines = vec![Line::from(spans)];
            for session in &summary.active_sessions {
                let is_paused = app.paused_sessions.contains(&session.id);
                let (symbol, label, status_style) = if is_paused {
                    ("\u{23f8}", "paused", app.theme.paused_style())
                } else {
                    (
                        session.claude_status.symbol(),
                        session.claude_status.as_str(),
                        app.theme.claude_status_style(session.claude_status),
                    )
                };
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(symbol, status_style),
                    Span::raw(" "),
                    Span::styled(label, status_style),
                ]));
            }

            ListItem::new(lines)
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

    if app.visible_tasks().is_empty() {
        let msg = Paragraph::new("  No tasks")
            .style(Style::default().fg(app.theme.text_secondary))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let Some(session) = app.session_for_selected_task() else {
        let hint = if app
            .visible_tasks()
            .get(app.task_index)
            .is_some_and(|t| t.status == TaskStatus::Done)
        {
            "  Completed (no session data)"
        } else {
            "  No session \u{2014} press l to launch"
        };
        let msg = Paragraph::new(hint)
            .style(Style::default().fg(app.theme.text_secondary))
            .block(block);
        frame.render_widget(msg, area);
        return;
    };

    let is_paused = app.paused_sessions.contains(&session.id);
    let (status_symbol, status_label, status_color) = if is_paused {
        ("\u{23f8}", "paused", app.theme.status_paused)
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

    // Show token usage from the selected task
    if let Some(task) = app.visible_tasks().into_iter().nth(app.task_index) {
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

    if is_paused || session.claude_status != ClaudeStatus::Working {
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
    if let Some(task) = app.visible_tasks().into_iter().nth(app.task_index)
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
                let pos = if count > 0 { app.task_index + 1 } else { 0 };
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
                let is_done = task.status == TaskStatus::Done;

                // Detect if this working task's session is paused (waiting for user permission)
                let is_paused = task.status == TaskStatus::Working
                    && task
                        .session_id
                        .as_deref()
                        .is_some_and(|sid| app.paused_sessions.contains(sid));

                let (status_symbol, status_label, status_style) = if is_paused {
                    ("\u{23f8}", "paused", app.theme.paused_style())
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

                if is_done {
                    spans.push(Span::styled(
                        task.title.clone(),
                        Style::default().fg(app.theme.text_secondary),
                    ));
                } else {
                    spans.push(Span::styled(
                        task.title.clone(),
                        Style::default().fg(app.theme.text_primary),
                    ));
                }

                // Skip subtask counts for done tasks (noise for completed work)
                if !is_done {
                    if let Some(&(total, done)) = app.subtask_counts.get(&task.id) {
                        spans.push(Span::styled(
                            format!(" ({done}/{total})"),
                            Style::default().fg(app.theme.text_secondary),
                        ));
                    }
                }

                if is_done {
                    spans.push(Span::styled(
                        format!("  {}", task.status.as_str()),
                        Style::default().fg(app.theme.text_secondary),
                    ));
                } else {
                    spans.push(Span::styled(format!("  {status_label}"), status_style));
                }

                if task.pr_url.is_some() {
                    let pr_color = if is_done {
                        app.theme.text_secondary
                    } else {
                        app.theme.pr_link
                    };
                    spans.push(Span::styled("  PR", Style::default().fg(pr_color)));
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
        let msg = Paragraph::new("  No active tasks. Press 'n' to create one.")
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

    let lines = vec![
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

    let detail = Paragraph::new(lines).block(block);
    frame.render_widget(detail, area);
}

fn draw_task_form_panel(frame: &mut Frame, app: &App, title: &str) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));

    // Calculate prompt text and measure wrapped line count using ratatui's own
    // word-wrapping so the panel height always matches the rendered text.
    let prompt_text = if app.new_task_field == 0 {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else {
        app.new_task_description.clone()
    };

    let inner_width = width.saturating_sub(2);
    let prompt_lines = if inner_width > 0 {
        Paragraph::new(Line::from(vec![
            Span::raw("  Prompt: "),
            Span::raw(&prompt_text),
        ]))
        .wrap(Wrap { trim: false })
        .line_count(inner_width)
        .max(1) as u16
    } else {
        1
    };

    // Subtask section height (always visible)
    let list_rows = app.new_task_subtasks.len().min(10) as u16;

    // Measure subtask input text wrapping
    let st_input_text = if app.new_task_field == 2 {
        format!(
            "  > {}",
            format_with_cursor(&app.input_buffer, app.input_cursor)
        )
    } else {
        "  > ".to_string()
    };
    let st_input_lines = if inner_width > 0 {
        Paragraph::new(st_input_text.as_str())
            .wrap(Wrap { trim: false })
            .line_count(inner_width)
            .max(1) as u16
    } else {
        1
    };

    // 1 (header "Subtasks:") + list + 1 (separator) + input lines
    let subtask_rows = 1 + list_rows + 1 + st_input_lines;

    // Layout: pad + prompt + pad + mode + pad + subtask section + hints + pad
    // Base inner height = 7 + prompt_lines + subtask_rows + 1 (pad before subtasks)
    let height = (7u16 + prompt_lines + subtask_rows + 1).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.form_border_task));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 5 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(app.theme.form_dim);
    let highlight = Style::default().fg(app.theme.form_highlight);
    let val_style = Style::default().fg(app.theme.text_primary);

    // Field 0: Prompt (wraps to multiple lines)
    let (label_s, val) = if app.new_task_field == 0 {
        (
            highlight,
            format_with_cursor(&app.input_buffer, app.input_cursor),
        )
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
    let mode_label_s = if app.new_task_field == 1 {
        highlight
    } else {
        dim
    };
    let mode_s = Style::default()
        .fg(app.theme.accent_primary)
        .add_modifier(Modifier::BOLD);
    let arrow_hint = if app.new_task_field == 1 {
        "  (\u{2190}/\u{2192} toggle)"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Mode:   ", mode_label_s),
            Span::styled(app.new_task_mode.as_str(), mode_s),
            Span::styled(arrow_hint, dim),
        ])),
        Rect::new(inner.x, inner.y + 3 + extra, inner.width, 1),
    );

    // Subtask section (always visible)
    let mut cursor_y = inner.y + 5 + extra; // padding after mode

    // Subtask header
    let st_label = if app.new_task_field == 2 {
        highlight
    } else {
        dim
    };
    frame.render_widget(
        Paragraph::new(Span::styled("  Subtasks:", st_label)),
        Rect::new(inner.x, cursor_y, inner.width, 1),
    );
    cursor_y += 1;

    // Subtask list
    let is_editing = app.editing_subtask_index.is_some();
    if app.new_task_subtasks.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("    (none yet)", dim)),
            Rect::new(inner.x, cursor_y, inner.width, 1),
        );
        cursor_y += 1;
    } else {
        for (i, desc) in app.new_task_subtasks.iter().take(10).enumerate() {
            if cursor_y >= inner.y + inner.height.saturating_sub(2) {
                break;
            }
            let is_sel = i == app.new_task_subtask_index && app.new_task_field == 2;
            let being_edited = app.editing_subtask_index == Some(i);
            let prefix = if being_edited {
                "  \u{270e} "
            } else if is_sel {
                "  \u{25b8} "
            } else {
                "    "
            };
            let st_style = if being_edited {
                Style::default().fg(app.theme.form_highlight)
            } else if is_sel {
                Style::default().fg(app.theme.selection_indicator)
            } else {
                val_style
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(prefix, st_style),
                    Span::styled(desc, st_style),
                ])),
                Rect::new(inner.x, cursor_y, inner.width, 1),
            );
            cursor_y += 1;
        }
    }

    // Subtask input line (auto-adjusting)
    let st_input_val = if app.new_task_field == 2 {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else {
        String::new()
    };

    let st_input_prefix = if is_editing { "  \u{270e} " } else { "  > " };

    let st_input_lines = if inner_width > 0 {
        Paragraph::new(Line::from(vec![
            Span::raw(st_input_prefix),
            Span::raw(&st_input_val),
        ]))
        .wrap(Wrap { trim: false })
        .line_count(inner_width)
        .max(1) as u16
    } else {
        1
    };
    let available = inner.y + inner.height.saturating_sub(2);
    let st_input_h = st_input_lines.min(available.saturating_sub(cursor_y));

    if cursor_y < available {
        let input_label_style = if is_editing {
            Style::default().fg(app.theme.form_highlight)
        } else if app.new_task_field == 2 {
            highlight
        } else {
            dim
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(st_input_prefix, input_label_style),
                Span::styled(st_input_val, val_style),
            ]))
            .wrap(Wrap { trim: false }),
            Rect::new(inner.x, cursor_y, inner.width, st_input_h),
        );
        cursor_y += st_input_h;
    }

    // Hints (context-aware based on field and editing state)
    let hints_y = cursor_y + 1;
    if hints_y < inner.y + inner.height {
        let hint_spans = if app.new_task_field == 2 && is_editing {
            // Editing a subtask
            vec![
                Span::styled("  Enter", highlight),
                Span::styled(":save  ", dim),
                Span::styled("Esc", highlight),
                Span::styled(":cancel", dim),
            ]
        } else if app.new_task_field == 2 && !app.new_task_subtasks.is_empty() {
            // Subtask field with items
            vec![
                Span::styled("  Tab", highlight),
                Span::styled(":cycle  ", dim),
                Span::styled("Enter", highlight),
                Span::styled(":edit/add  ", dim),
                Span::styled("d", highlight),
                Span::styled(":del  ", dim),
                Span::styled("Esc", highlight),
                if app.input_mode == InputMode::NewTask {
                    Span::styled(":draft", dim)
                } else {
                    Span::styled(":cancel", dim)
                },
            ]
        } else {
            // Default hints
            let mut spans = vec![
                Span::styled("  Tab", highlight),
                Span::styled(":field  ", dim),
                Span::styled("Enter", highlight),
                Span::styled(":create  ", dim),
                Span::styled("Esc", highlight),
            ];
            if app.input_mode == InputMode::NewTask {
                spans.push(Span::styled(":draft", dim));
            } else {
                spans.push(Span::styled(":cancel", dim));
            }
            spans
        };
        frame.render_widget(
            Paragraph::new(Line::from(hint_spans)),
            Rect::new(inner.x, hints_y, inner.width, 1),
        );
    }
}

fn draw_new_project_panel(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let inner_width = width.saturating_sub(2);

    // Measure wrapped line counts for name and path fields
    let name_text = if app.new_project_field == 0 {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else {
        app.new_project_name.clone()
    };
    let name_lines = if inner_width > 0 {
        Paragraph::new(Line::from(vec![
            Span::raw("  Name: "),
            Span::raw(&name_text),
        ]))
        .wrap(Wrap { trim: false })
        .line_count(inner_width)
        .max(1) as u16
    } else {
        1
    };

    let path_text = if app.new_project_field == 1 {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else {
        app.new_project_path.clone()
    };
    let path_lines = if inner_width > 0 {
        Paragraph::new(Line::from(vec![
            Span::raw("  Path: "),
            Span::raw(&path_text),
        ]))
        .wrap(Wrap { trim: false })
        .line_count(inner_width)
        .max(1) as u16
    } else {
        1
    };

    // Dynamic height: base layout + field line counts + dropdown rows
    let dropdown_rows = if app.show_path_suggestions {
        (app.path_suggestions.len().min(8) as u16) + 1 // +1 for separator
    } else {
        0
    };
    // Layout: pad(1) + name + pad(1) + path + pad(1) + hints(1) + borders(2) + dropdown
    let height =
        (6u16 + name_lines + path_lines + dropdown_rows).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Add Project ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.form_border_project));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 5 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(app.theme.form_dim);
    let highlight = Style::default().fg(app.theme.form_border_project);
    let val_style = Style::default().fg(app.theme.text_primary);

    // Field 0: Name (auto-adjusting)
    let (label_s, val) = if app.new_project_field == 0 {
        (
            highlight,
            format_with_cursor(&app.input_buffer, app.input_cursor),
        )
    } else {
        (dim, app.new_project_name.clone())
    };
    let name_h = name_lines.min(inner.height.saturating_sub(4));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Name: ", label_s),
            Span::styled(val, val_style),
        ]))
        .wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y + 1, inner.width, name_h),
    );

    let name_extra = name_h.saturating_sub(1);

    // Field 1: Path (auto-adjusting)
    let (label_s, val) = if app.new_project_field == 1 {
        (
            highlight,
            format_with_cursor(&app.input_buffer, app.input_cursor),
        )
    } else {
        (dim, app.new_project_path.clone())
    };
    let path_h = path_lines.min(inner.height.saturating_sub(4 + name_extra));
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Path: ", label_s),
            Span::styled(val, val_style),
        ]))
        .wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y + 3 + name_extra, inner.width, path_h),
    );

    let path_extra = path_h.saturating_sub(1);
    let fields_extra = name_extra + path_extra;

    // Path suggestion dropdown
    let mut hint_y_offset = 5 + fields_extra;
    if app.show_path_suggestions {
        let visible_count = app.path_suggestions.len().min(8);
        let separator_y = inner.y + 4 + fields_extra;

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
                    .fg(app.theme.accent_primary)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.text_primary)
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

        hint_y_offset = 5 + fields_extra + dropdown_rows;
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
    if inner.y + hint_y_offset < inner.y + inner.height {
        frame.render_widget(
            Paragraph::new(hints),
            Rect::new(inner.x, inner.y + hint_y_offset, inner.width, 1),
        );
    }
}

fn draw_usage_bars(frame: &mut Frame, app: &App, area: Rect) {
    let state = &app.rate_limit_state;

    let block = Block::default()
        .title(" Usage ")
        .borders(Borders::ALL)
        .border_style(if state.is_rate_limited {
            Style::default().fg(app.theme.rate_limit_warning)
        } else {
            app.theme.unfocused_border()
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
            Style::default()
                .fg(app.theme.rate_limit_warning)
                .add_modifier(Modifier::BOLD),
        )]));

        if let Some(ref reset_at) = state.reset_at {
            let display_time = reset_at
                .find('T')
                .map_or(reset_at.as_str(), |i| &reset_at[i + 1..]);
            let display_time = display_time.trim_end_matches('Z');
            let display_time = &display_time[..display_time.len().min(5)];
            lines.push(Line::from(vec![
                Span::styled("  Resumes: ", Style::default().fg(app.theme.text_secondary)),
                Span::styled(
                    display_time.to_string(),
                    Style::default().fg(app.theme.accent_secondary),
                ),
            ]));
        }
    }

    // Compute reset suffixes to find the longest, so both bars get equal width
    let format_reset = |r: &str| format!(" \u{21bb}{r}");
    let suffixes: [String; 2] = [
        state
            .reset_5h
            .as_deref()
            .map_or(String::new(), format_reset),
        state
            .reset_7d
            .as_deref()
            .map_or(String::new(), format_reset),
    ];
    let max_reset_len = suffixes[0].len().max(suffixes[1].len());
    let [suffix_hourly, suffix_daily] = suffixes;

    // 5h bar
    lines.push(usage_bar_line(
        "5h",
        state.usage_5h_pct,
        suffix_hourly,
        inner.width as usize,
        max_reset_len,
        &app.theme,
    ));

    // 7d bar
    lines.push(usage_bar_line(
        "7d",
        state.usage_7d_pct,
        suffix_daily,
        inner.width as usize,
        max_reset_len,
        &app.theme,
    ));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn usage_bar_line(
    label: &str,
    pct: Option<f64>,
    reset_suffix: String,
    total_width: usize,
    max_reset_len: usize,
    theme: &super::theme::Theme,
) -> Line<'static> {
    let Some(pct_raw) = pct else {
        // No data yet — show a placeholder
        return Line::from(vec![
            Span::styled(
                format!("  {label}: "),
                Style::default().fg(theme.text_secondary),
            ),
            Span::styled("--", Style::default().fg(theme.text_secondary)),
        ]);
    };

    let pct_clamped = pct_raw.clamp(0.0, 100.0);

    // "  5h: " = 6, " XX%" = 5, plus max reset suffix length
    // Use max_reset_len so both bars have identical bar width
    let overhead = 6 + 5 + max_reset_len;
    let bar_width = total_width.saturating_sub(overhead);

    let filled = ((pct_clamped / 100.0) * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_color = theme.usage_bar_color(pct_clamped);

    let filled_str: String = "\u{2588}".repeat(filled);
    let empty_str: String = "\u{2591}".repeat(empty);

    let mut spans = vec![
        Span::styled(
            format!("  {label}: "),
            Style::default().fg(theme.text_secondary),
        ),
        Span::styled(filled_str, Style::default().fg(bar_color)),
        Span::styled(empty_str, Style::default().fg(theme.text_secondary)),
        Span::styled(
            format!(" {pct_clamped:.0}%"),
            Style::default().fg(theme.text_primary),
        ),
    ];

    if !reset_suffix.is_empty() {
        spans.push(Span::styled(
            reset_suffix,
            Style::default().fg(theme.text_secondary),
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
    let inner_width = width.saturating_sub(2);
    let list_height = app.subtasks.len().min(10) as u16;

    // Measure input text wrapping for auto-adjust
    let input_text = format!(
        "  > {}",
        format_with_cursor(&app.input_buffer, app.input_cursor)
    );
    let input_lines = measure_wrapped_height(&input_text, inner_width);

    // Base: list/placeholder(1) + separator(1) + input + hints(1) + padding(4 for borders+gaps)
    let content_height = list_height.max(1) + 1 + input_lines + 1;
    let height = content_height + 4;

    let inner = render_modal(
        frame,
        " Subtasks ",
        Style::default().fg(app.theme.form_border_task),
        width,
        height,
    );

    if inner.height < 3 || inner.width < 20 {
        return;
    }

    let dim = Style::default().fg(app.theme.form_dim);
    let highlight = Style::default().fg(app.theme.form_highlight);

    // Render existing subtasks
    let mut y_offset = 0u16;
    for (i, st) in app.subtasks.iter().enumerate() {
        if y_offset >= inner.height.saturating_sub(3) {
            break;
        }
        let status_style = app.theme.task_status_style(st.status);
        let prefix = if i == app.subtask_index { "▸ " } else { "  " };
        let selector_style = if i == app.subtask_index {
            Style::default().fg(app.theme.selection_indicator)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, selector_style),
                Span::styled(st.status.symbol(), status_style),
                Span::raw(" "),
                Span::styled(&st.title, Style::default().fg(app.theme.text_primary)),
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

    // Input line (auto-adjusting height based on wrapped text)
    let input_val = format_with_cursor(&app.input_buffer, app.input_cursor);
    let available_for_input = inner.height.saturating_sub(y_offset + 2); // reserve hints + pad
    let input_h = input_lines.min(available_for_input).max(1);
    if inner.y + y_offset < inner.y + inner.height.saturating_sub(1) {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  > ", highlight),
                Span::styled(input_val, Style::default().fg(app.theme.text_primary)),
            ]))
            .wrap(Wrap { trim: false }),
            Rect::new(inner.x, inner.y + y_offset, inner.width, input_h),
        );
        y_offset += input_h;
    }

    // Hints at bottom
    let hints_y = inner.y + y_offset + 1;
    if hints_y < inner.y + inner.height {
        render_hints(
            frame,
            Rect::new(inner.x, hints_y, inner.width, 1),
            &[
                ("  Enter", ":add  "),
                ("d", ":del  "),
                ("j/k", ":nav  "),
                ("Esc", ":close"),
            ],
            highlight,
            dim,
        );
    }
}

fn draw_skill_panel(frame: &mut Frame, app: &App) {
    let scope_label = if app.skill_scope_global {
        "global"
    } else {
        "project"
    };
    let inner = render_modal(
        frame,
        &format!(" Skills [{scope_label}] "),
        Style::default().fg(app.theme.accent_primary),
        80,
        20,
    );

    if inner.height < 3 || inner.width < 30 {
        return;
    }

    // Split inner into left (skill list 40%) and right (detail 60%)
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(inner);

    // Reserve 1 row for hints at the bottom
    let list_area = Rect::new(
        halves[0].x,
        halves[0].y,
        halves[0].width,
        halves[0].height.saturating_sub(1),
    );
    let detail_area = Rect::new(
        halves[1].x,
        halves[1].y,
        halves[1].width,
        halves[1].height.saturating_sub(1),
    );

    // LEFT: Skill list
    if app.installed_skills.is_empty() {
        let msg = Paragraph::new("  No skills installed.\n  Press 'f' to find\n  or 'a' to add.");
        frame.render_widget(
            msg.style(Style::default().fg(app.theme.text_secondary)),
            list_area,
        );
    } else {
        let items: Vec<ListItem> = app
            .installed_skills
            .iter()
            .enumerate()
            .map(|(i, skill)| {
                let is_selected = i == app.skill_index;
                let style = if is_selected {
                    Style::default()
                        .fg(app.theme.text_primary)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text_primary)
                };
                let prefix = if is_selected { "\u{25b8} " } else { "  " };
                let prefix_style = if is_selected {
                    Style::default().fg(app.theme.selection_indicator)
                } else {
                    Style::default()
                };
                ListItem::new(Line::from(vec![
                    Span::styled(prefix, prefix_style),
                    Span::styled(&skill.name, style),
                ]))
            })
            .collect();
        let list = List::new(items);
        frame.render_widget(list, list_area);
    }

    // RIGHT: Skill detail
    if let Some(skill) = app.installed_skills.get(app.skill_index) {
        let max_lines = detail_area.height.saturating_sub(3) as usize;
        let mut lines = vec![
            Line::from(vec![
                Span::styled("  Name: ", Style::default().fg(app.theme.text_secondary)),
                Span::styled(&skill.name, Style::default().fg(app.theme.text_accent)),
            ]),
            Line::from(vec![
                Span::styled("  Agents: ", Style::default().fg(app.theme.text_secondary)),
                Span::styled(
                    skill.agents.join(", "),
                    Style::default().fg(app.theme.text_primary),
                ),
            ]),
            Line::from(""),
        ];

        for md_line in app.skill_detail_content.lines().take(max_lines) {
            lines.push(Line::from(Span::styled(
                format!("  {md_line}"),
                Style::default().fg(app.theme.text_primary),
            )));
        }
        if app.skill_detail_content.lines().count() > max_lines {
            lines.push(Line::from(Span::styled(
                "  ...",
                Style::default().fg(app.theme.text_secondary),
            )));
        }

        let detail = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(detail, detail_area);
    }

    // Hints at the bottom of the panel
    let hints_y = inner.y + inner.height.saturating_sub(1);
    render_hints(
        frame,
        Rect::new(inner.x, hints_y, inner.width, 1),
        &[
            (" f", ":find  "),
            ("a", ":add  "),
            ("x", ":remove  "),
            ("u", ":update  "),
            ("g", ":global/project  "),
            ("Esc", ":close"),
        ],
        Style::default().fg(app.theme.text_accent),
        Style::default().fg(app.theme.text_secondary),
    );
}

fn draw_skill_search_overlay(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 50u16.min(area.width.saturating_sub(4));
    let inner_width = width.saturating_sub(2);
    let result_rows = app.search_results.len().min(8) as u16;
    let has_status = !app.skill_status_message.is_empty();
    let status_row = u16::from(has_status);

    // Measure input wrapping for auto-adjust
    let input_text = format!(
        "> {}",
        format_with_cursor(&app.input_buffer, app.input_cursor)
    );
    let input_lines = measure_wrapped_height(&input_text, inner_width);

    // input lines + optional status + results + hints = rows inside borders
    let height = (3 + input_lines + status_row + result_rows).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height / 5;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Find Skills ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.form_highlight));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 2 {
        return;
    }

    // Search input (auto-adjusting)
    let input_h = input_lines.min(inner.height.saturating_sub(1));
    let ss_cursor = app.input_cursor.min(app.input_buffer.len());
    let (ss_before, ss_after) = app.input_buffer.split_at(ss_cursor);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(app.theme.form_highlight)),
        Span::raw(ss_before.to_string()),
        Span::styled("\u{2588}", Style::default().fg(app.theme.form_highlight)),
        Span::raw(ss_after.to_string()),
    ]);
    frame.render_widget(
        Paragraph::new(input_line).wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y, inner.width, input_h),
    );

    // Status message (shown after search completes)
    let mut next_y = inner.y + input_h;
    if has_status {
        let color = if app.skill_status_message.starts_with("Search failed")
            || app.skill_status_message.starts_with("Install failed")
        {
            app.theme.toast_error
        } else {
            app.theme.text_secondary
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                &app.skill_status_message,
                Style::default().fg(color),
            )),
            Rect::new(inner.x, next_y, inner.width, 1),
        );
        next_y += 1;
    }

    // Search results
    if !app.search_results.is_empty() {
        let items_area = Rect::new(
            inner.x,
            next_y,
            inner.width,
            inner.height.saturating_sub(next_y - inner.y + 1),
        );
        let items: Vec<ListItem> = app
            .search_results
            .iter()
            .enumerate()
            .map(|(i, result)| {
                let style = if i == app.skill_index {
                    Style::default()
                        .fg(app.theme.text_accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text_primary)
                };
                let line = if result.installs.is_empty() {
                    Line::from(Span::styled(&result.package, style))
                } else {
                    Line::from(vec![
                        Span::styled(&result.package, style),
                        Span::styled(
                            format!("  {}", result.installs),
                            Style::default().fg(app.theme.text_secondary),
                        ),
                    ])
                };
                ListItem::new(line)
            })
            .collect();
        frame.render_widget(List::new(items), items_area);
    }

    // Hints at bottom
    let hints_y = inner.y + inner.height.saturating_sub(1);
    let key_style = Style::default().fg(app.theme.form_highlight);
    let desc_style = Style::default().fg(app.theme.text_secondary);
    let mut hint_spans = vec![
        Span::styled("Enter", key_style),
        Span::styled(":search/install  ", desc_style),
    ];
    if !app.search_results.is_empty() {
        hint_spans.push(Span::styled("j/k", key_style));
        hint_spans.push(Span::styled(":navigate  ", desc_style));
    }
    hint_spans.push(Span::styled("Esc", key_style));
    hint_spans.push(Span::styled(":back", desc_style));
    frame.render_widget(
        Paragraph::new(Line::from(hint_spans)),
        Rect::new(inner.x, hints_y, inner.width, 1),
    );
}

fn draw_skill_add_overlay(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let width = 50u16.min(area.width.saturating_sub(4));
    let inner_width = width.saturating_sub(2);

    // Measure input wrapping for auto-adjust
    let input_text = format!(
        "> {}",
        format_with_cursor(&app.input_buffer, app.input_cursor)
    );
    let input_lines = measure_wrapped_height(&input_text, inner_width);

    let height = (4u16 + input_lines).min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = area.height / 5;
    let panel_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel_area);

    let block = Block::default()
        .title(" Add Skill (owner/repo@skill) ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.form_highlight));
    let inner = block.inner(panel_area);
    frame.render_widget(block, panel_area);

    if inner.height < 2 {
        return;
    }

    // Package input (auto-adjusting)
    let input_h = input_lines.min(inner.height.saturating_sub(1));
    let sa_cursor = app.input_cursor.min(app.input_buffer.len());
    let (sa_before, sa_after) = app.input_buffer.split_at(sa_cursor);
    let input_line = Line::from(vec![
        Span::styled("> ", Style::default().fg(app.theme.form_highlight)),
        Span::raw(sa_before.to_string()),
        Span::styled("\u{2588}", Style::default().fg(app.theme.form_highlight)),
        Span::raw(sa_after.to_string()),
    ]);
    frame.render_widget(
        Paragraph::new(input_line).wrap(Wrap { trim: false }),
        Rect::new(inner.x, inner.y, inner.width, input_h),
    );

    // Hints at bottom
    let hints_y = inner.y + inner.height.saturating_sub(1);
    render_hints(
        frame,
        Rect::new(inner.x, hints_y, inner.width, 1),
        &[("Enter", ":install  "), ("Esc", ":back")],
        Style::default().fg(app.theme.form_highlight),
        Style::default().fg(app.theme.text_secondary),
    );
}

fn draw_help_overlay(frame: &mut Frame, app: &App) {
    let theme = &app.theme;
    let inner = render_modal(
        frame,
        " Help \u{2014} press ? or Esc to close ",
        Style::default().fg(theme.accent_primary),
        60,
        35,
    );

    let mut lines: Vec<Line<'_>> = Vec::new();
    let groups = app.keymap.help_entries();

    for (i, (section_title, entries)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(help_section(section_title, theme));
        for entry in entries {
            lines.push(help_line(entry.label, entry.description, theme));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn help_section<'a>(title: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(Span::styled(
        format!("  {title}"),
        Style::default()
            .fg(theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    ))
}

fn help_line<'a>(key: &'a str, desc: &'a str, theme: &Theme) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<14}"),
            Style::default().fg(theme.text_accent),
        ),
        Span::styled(desc, Style::default().fg(theme.text_primary)),
    ])
}
