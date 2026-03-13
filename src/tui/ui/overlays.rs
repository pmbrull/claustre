use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};

use super::super::app::App;
use super::super::form::{format_with_cursor, measure_wrapped_height, render_hints, render_modal};
use super::super::theme::Theme;
use super::usage::format_tokens;

pub(super) fn draw_command_palette(frame: &mut Frame, app: &App) {
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

pub(super) fn draw_subtask_panel(frame: &mut Frame, app: &App) {
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

pub(super) fn draw_skill_panel(frame: &mut Frame, app: &App) {
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

pub(super) fn draw_skill_search_overlay(frame: &mut Frame, app: &App) {
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
                let is_cursor = i == app.skill_index;
                let is_selected = app.selected_search_indices.contains(&i);
                let style = if is_cursor {
                    Style::default()
                        .fg(app.theme.text_accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.theme.text_primary)
                };
                let checkbox = if is_selected { "[x] " } else { "[ ] " };
                let checkbox_style = if is_selected {
                    Style::default().fg(app.theme.selection_indicator)
                } else {
                    Style::default().fg(app.theme.text_secondary)
                };
                let mut spans = vec![
                    Span::styled(checkbox, checkbox_style),
                    Span::styled(&result.package, style),
                ];
                if !result.installs.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", result.installs),
                        Style::default().fg(app.theme.text_secondary),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        frame.render_widget(List::new(items), items_area);
    }

    // Hints at bottom
    let hints_y = inner.y + inner.height.saturating_sub(1);
    let key_style = Style::default().fg(app.theme.form_highlight);
    let desc_style = Style::default().fg(app.theme.text_secondary);
    let selected_count = app.selected_search_indices.len();
    let install_label = if app.search_results.is_empty() {
        ":search  ".to_string()
    } else if selected_count > 0 {
        format!(":install ({selected_count})  ")
    } else {
        ":search/install  ".to_string()
    };
    let mut hint_spans = vec![
        Span::styled("Enter", key_style),
        Span::styled(install_label, desc_style),
    ];
    if !app.search_results.is_empty() {
        hint_spans.push(Span::styled("Space", key_style));
        hint_spans.push(Span::styled(":select  ", desc_style));
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

pub(super) fn draw_skill_add_overlay(frame: &mut Frame, app: &App) {
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

pub(super) fn draw_task_details_panel(frame: &mut Frame, app: &App) {
    let theme = &app.theme;

    let Some(task) = app.visible_tasks().into_iter().nth(app.task_index) else {
        return;
    };

    let inner = render_modal(
        frame,
        " Task Details — press v or Esc to close ",
        Style::default().fg(theme.accent_primary),
        80,
        30,
    );

    let mut lines: Vec<Line<'_>> = Vec::new();

    // Title
    lines.push(Line::from(vec![
        Span::styled(
            "  Title: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(task.title.clone(), Style::default().fg(theme.text_primary)),
    ]));

    // Status
    lines.push(Line::from(vec![
        Span::styled(
            "  Status: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} {}", task.status.symbol(), task.status.as_str()),
            theme.task_status_style(task.status),
        ),
    ]));

    // Mode
    lines.push(Line::from(vec![
        Span::styled(
            "  Mode: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            task.mode.as_str().to_string(),
            Style::default().fg(theme.text_primary),
        ),
    ]));

    // Push mode
    lines.push(Line::from(vec![
        Span::styled(
            "  Push: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            task.push_mode.as_str().to_string(),
            Style::default().fg(theme.text_primary),
        ),
    ]));

    // Review loop
    lines.push(Line::from(vec![
        Span::styled(
            "  Review loop: ",
            Style::default()
                .fg(theme.text_secondary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if task.review_loop { "yes" } else { "no" },
            Style::default().fg(theme.text_primary),
        ),
    ]));

    // Base (PR target branch)
    if let Some(ref base) = task.base {
        lines.push(Line::from(vec![
            Span::styled(
                "  Base: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(base.clone(), Style::default().fg(theme.text_primary)),
        ]));
    }

    // Branch (existing branch to reuse)
    if let Some(ref branch) = task.branch {
        lines.push(Line::from(vec![
            Span::styled(
                "  Branch: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(branch.clone(), Style::default().fg(theme.text_primary)),
        ]));
    }

    // PR URL
    if let Some(ref url) = task.pr_url {
        lines.push(Line::from(vec![
            Span::styled(
                "  PR: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(url.clone(), Style::default().fg(theme.pr_link)),
        ]));
    }

    // Token usage
    let total_tokens = task.input_tokens + task.output_tokens;
    if total_tokens > 0 {
        lines.push(Line::from(vec![
            Span::styled(
                "  Tokens: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(
                    "{} in / {} out",
                    format_tokens(task.input_tokens),
                    format_tokens(task.output_tokens),
                ),
                Style::default().fg(theme.text_primary),
            ),
        ]));
    }

    // Timing
    if let Some(ref started) = task.started_at {
        lines.push(Line::from(vec![
            Span::styled(
                "  Started: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(started.clone(), Style::default().fg(theme.text_primary)),
        ]));
    }
    if let Some(ref completed) = task.completed_at {
        lines.push(Line::from(vec![
            Span::styled(
                "  Completed: ",
                Style::default()
                    .fg(theme.text_secondary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(completed.clone(), Style::default().fg(theme.text_primary)),
        ]));
    }

    // Prompt section
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Prompt",
        Style::default()
            .fg(theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "  ─".to_string() + &"─".repeat(inner.width.saturating_sub(4) as usize),
        Style::default().fg(theme.text_secondary),
    )));

    if task.description.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no description)",
            Style::default().fg(theme.text_secondary),
        )));
    } else {
        for line in task.description.lines() {
            lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(theme.text_primary),
            )));
        }
    }

    // Subtask section
    let subtasks = app
        .store
        .list_subtasks_for_task(&task.id)
        .unwrap_or_default();
    if !subtasks.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("  Subtasks ({})", subtasks.len()),
            Style::default()
                .fg(theme.accent_secondary)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "  ─".to_string() + &"─".repeat(inner.width.saturating_sub(4) as usize),
            Style::default().fg(theme.text_secondary),
        )));
        for (i, st) in subtasks.iter().enumerate() {
            lines.push(Line::from(Span::styled(
                format!("  {}. {}", i + 1, st.title),
                Style::default().fg(theme.text_primary),
            )));
        }
    }

    let paragraph = Paragraph::new(lines)
        .scroll((app.task_details_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, inner);
}

pub(super) fn draw_help_overlay(frame: &mut Frame, app: &App) {
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

pub(super) fn draw_configure_wizard(frame: &mut Frame, app: &App) {
    let area = frame.area();

    // Load current status
    let status = crate::configure::load_config_status();

    // Build content lines
    let mut lines: Vec<Line<'_>> = Vec::new();

    lines.push(Line::from(Span::styled(
        " Configure Claude Code Permissions",
        Style::default()
            .fg(app.theme.text_accent)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    match &status {
        Ok(status) => {
            let total_missing: usize = status.diffs.iter().map(|d| d.missing.len()).sum();

            if total_missing == 0 {
                lines.push(Line::from(Span::styled(
                    " ✓ All permissions match recommendations!",
                    Style::default().fg(app.theme.toast_success),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!(" {total_missing} missing permission(s) detected"),
                    Style::default()
                        .fg(app.theme.accent_secondary)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));

                // Show recommended (from config.toml)
                let rec = &status.recommended;
                for (label, recommended, color) in [
                    ("allow", &rec.allow, app.theme.toast_success),
                    ("deny", &rec.deny, app.theme.status_error),
                    ("ask", &rec.ask, app.theme.accent_secondary),
                ] {
                    lines.push(Line::from(Span::styled(
                        format!(" {label}:"),
                        Style::default()
                            .fg(app.theme.text_primary)
                            .add_modifier(Modifier::BOLD),
                    )));
                    for perm in recommended {
                        lines.push(Line::from(Span::styled(
                            format!("   {perm}"),
                            Style::default().fg(color),
                        )));
                    }
                }

                lines.push(Line::from(""));

                // Show what's missing per category
                lines.push(Line::from(Span::styled(
                    " Missing:",
                    Style::default()
                        .fg(app.theme.text_primary)
                        .add_modifier(Modifier::BOLD),
                )));

                for diff in &status.diffs {
                    for m in &diff.missing {
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("   + {m}"),
                                Style::default().fg(app.theme.toast_success),
                            ),
                            Span::styled(
                                format!("  ({})", diff.category),
                                Style::default().fg(app.theme.text_secondary),
                            ),
                        ]));
                    }
                }
            }
        }
        Err(e) => {
            lines.push(Line::from(Span::styled(
                format!(" Error loading settings: {e}"),
                Style::default().fg(app.theme.status_error),
            )));
        }
    }

    // Hints at bottom
    lines.push(Line::from(""));
    let has_missing = status
        .as_ref()
        .is_ok_and(|s| s.diffs.iter().any(|d| !d.missing.is_empty()));
    if has_missing {
        lines.push(Line::from(vec![
            Span::styled(
                " a",
                Style::default()
                    .fg(app.theme.text_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                ":apply all  ",
                Style::default().fg(app.theme.text_secondary),
            ),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(app.theme.text_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":close", Style::default().fg(app.theme.text_secondary)),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled(
                " Esc",
                Style::default()
                    .fg(app.theme.text_accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(":close", Style::default().fg(app.theme.text_secondary)),
        ]));
    }

    // Size and position the overlay
    let content_height = u16::try_from(lines.len()).unwrap_or(20) + 2; // +2 for borders
    let width = 60u16.min(area.width.saturating_sub(4));
    let height = content_height.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(width)) / 2;
    let y = (area.height.saturating_sub(height)) / 2;
    let overlay_area = Rect::new(x, y, width, height);

    frame.render_widget(Clear, overlay_area);

    let block = Block::default()
        .title(" Configure ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.accent_primary));

    let inner = block.inner(overlay_area);
    frame.render_widget(block, overlay_area);
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}
