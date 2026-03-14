use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use super::super::app::{App, InputMode};
use super::super::form::{cursor_visual_line, format_with_cursor};

pub(super) fn draw_task_form_panel(frame: &mut Frame, app: &App, title: &str) {
    let area = frame.area();
    let width = 60u16.min(area.width.saturating_sub(4));
    let inner_width = width.saturating_sub(2);

    // Calculate prompt text and measure wrapped line count using ratatui's own
    // word-wrapping so the panel height always matches the rendered text.
    let prompt_text = if app.new_task_field == 0 {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else {
        app.new_task_description.clone()
    };

    // Use usize to avoid u16 overflow with very long prompts.
    let total_prompt_lines: usize = if inner_width > 0 {
        Paragraph::new(Line::from(vec![
            Span::raw("  Prompt: "),
            Span::raw(&prompt_text),
        ]))
        .wrap(Wrap { trim: false })
        .line_count(inner_width)
        .max(1)
    } else {
        1
    };

    // Measure base text wrapping
    let base_text = if app.new_task_field == 2 {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else if app.new_task_base.is_empty() {
        "(default)".to_string()
    } else {
        app.new_task_base.clone()
    };
    let total_base_lines: u16 = if inner_width > 0 {
        Paragraph::new(Line::from(vec![
            Span::raw("  Base:   "),
            Span::raw(&base_text),
        ]))
        .wrap(Wrap { trim: false })
        .line_count(inner_width)
        .max(1) as u16
    } else {
        1
    };

    // Measure branch text wrapping
    let branch_text = if app.new_task_field == 3 {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else if app.new_task_branch.is_empty() {
        "(auto)".to_string()
    } else {
        app.new_task_branch.clone()
    };
    let total_branch_lines: u16 = if inner_width > 0 {
        Paragraph::new(Line::from(vec![
            Span::raw("  Branch: "),
            Span::raw(&branch_text),
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
    let st_input_text = if app.new_task_field == 6 {
        format!(
            "  > {}",
            format_with_cursor(&app.input_buffer, app.input_cursor)
        )
    } else {
        "  > ".to_string()
    };
    let st_input_lines = if inner_width > 0 {
        (Paragraph::new(st_input_text.as_str())
            .wrap(Wrap { trim: false })
            .line_count(inner_width)
            .max(1) as u16)
            .min(5) // cap subtask input display
    } else {
        1
    };

    // 1 (header "Subtasks:") + list + 1 (separator) + input lines
    let subtask_rows = 1u16
        .saturating_add(list_rows)
        .saturating_add(1)
        .saturating_add(st_input_lines);

    // Extra lines from base/branch wrapping beyond the 1-line baseline already in the 16.
    let base_extra_lines = total_base_lines.saturating_sub(1);
    let branch_extra_lines = total_branch_lines.saturating_sub(1);

    // Rows needed for non-prompt content (mode, base, branch, push, loop, subtasks, hints, padding).
    let non_prompt_rows = 16u16
        .saturating_add(subtask_rows)
        .saturating_add(base_extra_lines)
        .saturating_add(branch_extra_lines);

    // Layout: pad + prompt + pad + mode + pad + base + pad + branch + pad + push_mode + pad + loop + pad + subtask section + hints + pad
    let prompt_lines_clamped = total_prompt_lines.min(u16::MAX as usize) as u16;
    let ideal_height = non_prompt_rows.saturating_add(prompt_lines_clamped);
    let height = ideal_height.min(area.height.saturating_sub(4));
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

    // Cap prompt display height to leave room for other fields (mode, branch, push,
    // subtask section, hints, and their padding rows).
    // `non_prompt_rows` includes 2 border rows (used for panel height), but `inner.height`
    // already excludes borders, so subtract 2 to avoid double-counting.
    let max_prompt_display = inner
        .height
        .saturating_sub(non_prompt_rows.saturating_sub(2))
        .max(1);
    let display_prompt_height = prompt_lines_clamped.min(max_prompt_display);

    // Compute scroll offset to keep cursor visible when prompt is long.
    let prompt_scroll: u16 =
        if app.new_task_field == 0 && prompt_lines_clamped > display_prompt_height {
            let cursor_line = cursor_visual_line(
                "  Prompt: ",
                &app.input_buffer,
                app.input_cursor,
                inner_width,
            );
            if cursor_line >= display_prompt_height {
                cursor_line.saturating_sub(display_prompt_height) + 1
            } else {
                0
            }
        } else {
            0
        };

    // Field 0: Prompt (wraps to multiple lines, scrolls when content exceeds display)
    let (label_s, val) = if app.new_task_field == 0 {
        (
            highlight,
            format_with_cursor(&app.input_buffer, app.input_cursor),
        )
    } else {
        (dim, app.new_task_description.clone())
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("  Prompt: ", label_s),
            Span::styled(val, val_style),
        ]))
        .wrap(Wrap { trim: false })
        .scroll((prompt_scroll, 0)),
        Rect::new(inner.x, inner.y + 1, inner.width, display_prompt_height),
    );

    // Shift remaining fields down by extra prompt lines (capped).
    let extra_prompt = display_prompt_height.saturating_sub(1);
    let bottom = inner.y.saturating_add(inner.height);

    // Field 1: Mode
    let mode_y = inner.y + 3 + extra_prompt;
    if mode_y < bottom {
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
            Rect::new(inner.x, mode_y, inner.width, 1),
        );
    }

    // Field 2: Base (PR target branch, wraps for long values)
    let base_y = inner.y + 5 + extra_prompt;
    let base_display_h = total_base_lines.min(bottom.saturating_sub(base_y).max(1));
    if base_y < bottom {
        let base_label_s = if app.new_task_field == 2 {
            highlight
        } else {
            dim
        };
        let base_val = if app.new_task_field == 2 {
            format_with_cursor(&app.input_buffer, app.input_cursor)
        } else if app.new_task_base.is_empty() {
            "(default)".to_string()
        } else {
            app.new_task_base.clone()
        };
        let base_val_style = if app.new_task_base.is_empty() && app.new_task_field != 2 {
            dim
        } else {
            val_style
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Base:   ", base_label_s),
                Span::styled(base_val, base_val_style),
            ]))
            .wrap(Wrap { trim: false }),
            Rect::new(inner.x, base_y, inner.width, base_display_h),
        );
    }
    let extra_base = base_display_h.saturating_sub(1);

    // Field 3: Branch (existing branch to reuse, wraps for long values)
    let branch_y = inner.y + 7 + extra_prompt + extra_base;
    let branch_display_h = total_branch_lines.min(bottom.saturating_sub(branch_y).max(1));
    if branch_y < bottom {
        let branch_label_s = if app.new_task_field == 3 {
            highlight
        } else {
            dim
        };
        let branch_val = if app.new_task_field == 3 {
            format_with_cursor(&app.input_buffer, app.input_cursor)
        } else if app.new_task_branch.is_empty() {
            "(auto)".to_string()
        } else {
            app.new_task_branch.clone()
        };
        let branch_val_style = if app.new_task_branch.is_empty() && app.new_task_field != 3 {
            dim
        } else {
            val_style
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Branch: ", branch_label_s),
                Span::styled(branch_val, branch_val_style),
            ]))
            .wrap(Wrap { trim: false }),
            Rect::new(inner.x, branch_y, inner.width, branch_display_h),
        );
    }
    let extra_branch = branch_display_h.saturating_sub(1);

    // Combined extra offset for fields below base/branch
    let extra = extra_prompt + extra_base + extra_branch;

    // Field 4: Push Mode
    let push_y = inner.y + 9 + extra;
    if push_y < bottom {
        let push_label_s = if app.new_task_field == 4 {
            highlight
        } else {
            dim
        };
        let push_arrow_hint = if app.new_task_field == 4 {
            "  (\u{2190}/\u{2192} toggle)"
        } else {
            ""
        };
        let mode_s = Style::default()
            .fg(app.theme.accent_primary)
            .add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Push:   ", push_label_s),
                Span::styled(app.new_task_push_mode.as_str(), mode_s),
                Span::styled(push_arrow_hint, dim),
            ])),
            Rect::new(inner.x, push_y, inner.width, 1),
        );
    }

    // Field 5: Review Loop
    let loop_y = inner.y + 11 + extra;
    if loop_y < bottom {
        let loop_label_s = if app.new_task_field == 5 {
            highlight
        } else {
            dim
        };
        let loop_arrow_hint = if app.new_task_field == 5 {
            "  (\u{2190}/\u{2192} toggle)"
        } else {
            ""
        };
        let loop_val = if app.new_task_review_loop {
            "on"
        } else {
            "off"
        };
        let loop_val_style = if app.new_task_review_loop {
            Style::default()
                .fg(app.theme.accent_primary)
                .add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Loop:   ", loop_label_s),
                Span::styled(loop_val, loop_val_style),
                Span::styled(loop_arrow_hint, dim),
            ])),
            Rect::new(inner.x, loop_y, inner.width, 1),
        );
    }

    // Subtask section (always visible if space permits)
    let mut cursor_y = inner.y + 13 + extra;

    // Subtask header
    if cursor_y < bottom {
        let st_label = if app.new_task_field == 6 {
            highlight
        } else {
            dim
        };
        frame.render_widget(
            Paragraph::new(Span::styled("  Subtasks:", st_label)),
            Rect::new(inner.x, cursor_y, inner.width, 1),
        );
        cursor_y += 1;
    }

    // Subtask list
    let is_editing = app.editing_subtask_index.is_some();
    if app.new_task_subtasks.is_empty() {
        if cursor_y < bottom.saturating_sub(2) {
            frame.render_widget(
                Paragraph::new(Span::styled("    (none yet)", dim)),
                Rect::new(inner.x, cursor_y, inner.width, 1),
            );
            cursor_y += 1;
        }
    } else {
        for (i, desc) in app.new_task_subtasks.iter().take(10).enumerate() {
            if cursor_y >= bottom.saturating_sub(2) {
                break;
            }
            let is_sel = i == app.new_task_subtask_index && app.new_task_field == 6;
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
    let st_input_val = if app.new_task_field == 6 {
        format_with_cursor(&app.input_buffer, app.input_cursor)
    } else {
        String::new()
    };

    let st_input_prefix = if is_editing { "  \u{270e} " } else { "  > " };

    let st_input_lines = if inner_width > 0 {
        (Paragraph::new(Line::from(vec![
            Span::raw(st_input_prefix),
            Span::raw(&st_input_val),
        ]))
        .wrap(Wrap { trim: false })
        .line_count(inner_width)
        .max(1) as u16)
            .min(5) // cap display height for subtask input
    } else {
        1
    };
    let available = bottom.saturating_sub(2);
    let st_input_h = st_input_lines.min(available.saturating_sub(cursor_y));

    if cursor_y < available {
        let input_label_style = if is_editing {
            Style::default().fg(app.theme.form_highlight)
        } else if app.new_task_field == 6 {
            highlight
        } else {
            dim
        };

        // Scroll subtask input to keep cursor visible
        let st_scroll: u16 =
            if app.new_task_field == 6 && st_input_lines > st_input_h && st_input_h > 0 {
                let cursor_line = cursor_visual_line(
                    st_input_prefix,
                    &app.input_buffer,
                    app.input_cursor,
                    inner_width,
                );
                if cursor_line >= st_input_h {
                    cursor_line.saturating_sub(st_input_h) + 1
                } else {
                    0
                }
            } else {
                0
            };

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(st_input_prefix, input_label_style),
                Span::styled(st_input_val, val_style),
            ]))
            .wrap(Wrap { trim: false })
            .scroll((st_scroll, 0)),
            Rect::new(inner.x, cursor_y, inner.width, st_input_h),
        );
        cursor_y += st_input_h;
    }

    // Hints (context-aware based on field and editing state)
    let hints_y = cursor_y + 1;
    if hints_y < inner.y + inner.height {
        let hint_spans = if app.new_task_field == 6 && is_editing {
            // Editing a subtask
            vec![
                Span::styled("  Enter", highlight),
                Span::styled(":save  ", dim),
                Span::styled("Esc", highlight),
                Span::styled(":cancel", dim),
            ]
        } else if app.new_task_field == 6 && !app.new_task_subtasks.is_empty() {
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

pub(super) fn draw_new_project_panel(frame: &mut Frame, app: &App) {
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
