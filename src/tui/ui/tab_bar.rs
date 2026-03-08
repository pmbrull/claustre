use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::super::app::{App, Tab};

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
pub(super) fn draw_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
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
