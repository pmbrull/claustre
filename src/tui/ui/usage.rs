use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::super::app::App;

pub(super) fn draw_usage_bars(frame: &mut Frame, app: &App, area: Rect) {
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
                .and_then(|i| reset_at.get(i + 1..))
                .unwrap_or(reset_at.as_str());
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
    theme: &super::super::theme::Theme,
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

pub(crate) fn format_tokens(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        format!("{tokens}")
    }
}
