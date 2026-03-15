use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

use super::Selection;

/// A ratatui widget that renders a vt100 terminal screen.
pub struct TerminalWidget<'a> {
    screen: &'a vt100::Screen,
    focused: bool,
    selection: Option<&'a Selection>,
    /// Number of lines scrolled back from the live screen (0 = live).
    scrollback_offset: usize,
}

impl<'a> TerminalWidget<'a> {
    pub fn new(screen: &'a vt100::Screen, focused: bool) -> Self {
        Self {
            screen,
            focused,
            selection: None,
            scrollback_offset: 0,
        }
    }

    pub fn with_selection(mut self, selection: Option<&'a Selection>) -> Self {
        self.selection = selection;
        self
    }

    pub fn with_scrollback_offset(mut self, offset: usize) -> Self {
        self.scrollback_offset = offset;
        self
    }
}

impl Widget for TerminalWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows = area.height.min(self.screen.size().0);
        let cols = area.width.min(self.screen.size().1);

        for row in 0..rows {
            for col in 0..cols {
                let Some(vt_cell) = self.screen.cell(row, col) else {
                    continue;
                };
                let Some(buf_cell) = buf.cell_mut((area.x + col, area.y + row)) else {
                    continue;
                };

                // Write symbol directly to avoid the overhead of set_string()
                let contents = vt_cell.contents();
                if contents.is_empty() {
                    buf_cell.set_char(' ');
                } else {
                    buf_cell.set_symbol(&contents);
                }

                let is_selected = self.selection.is_some_and(|sel| sel.contains(row, col));

                if is_selected {
                    // Highlight selected cells: swap fg/bg for visibility
                    let fg = vt100_color_to_ratatui(vt_cell.bgcolor());
                    let bg = vt100_color_to_ratatui(vt_cell.fgcolor());
                    // Use sensible defaults when colors are Reset
                    buf_cell.set_fg(if fg == Color::Reset { Color::Black } else { fg });
                    buf_cell.set_bg(if bg == Color::Reset { Color::White } else { bg });
                } else {
                    buf_cell.set_fg(vt100_color_to_ratatui(vt_cell.fgcolor()));
                    buf_cell.set_bg(vt100_color_to_ratatui(vt_cell.bgcolor()));
                }

                // Build modifier flags in one shot
                let mut mods = Modifier::empty();
                if vt_cell.bold() {
                    mods |= Modifier::BOLD;
                }
                if vt_cell.italic() {
                    mods |= Modifier::ITALIC;
                }
                if vt_cell.underline() {
                    mods |= Modifier::UNDERLINED;
                }
                if vt_cell.inverse() {
                    mods |= Modifier::REVERSED;
                }
                // NOTE: vt100 0.15.x does not expose a dim() method on Cell.
                // Dim/faint attribute support requires an upstream vt100 update.
                buf_cell.set_style(Style::default().add_modifier(mods));
            }
        }

        // Draw cursor if focused, on the live screen (not scrolled back), and cursor is visible
        if self.focused && self.screen.scrollback() == 0 && !self.screen.hide_cursor() {
            let cursor = self.screen.cursor_position();
            let cx = area.x.saturating_add(cursor.1);
            let cy = area.y.saturating_add(cursor.0);
            if cx < area.x.saturating_add(area.width) && cy < area.y.saturating_add(area.height) {
                let cursor_selected = self
                    .selection
                    .is_some_and(|sel| sel.contains(cursor.0, cursor.1));
                if !cursor_selected && let Some(cell) = buf.cell_mut((cx, cy)) {
                    cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
        }

        // Draw scroll indicator when viewing history
        if self.scrollback_offset > 0 && area.width >= 10 {
            let label = format!(" [{} lines] ", self.scrollback_offset);
            let label_len = u16::try_from(label.len()).unwrap_or(u16::MAX);
            if label_len <= area.width {
                let x_start = area.x + area.width - label_len;
                let y = area.y;
                let style = Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD);
                for (i, ch) in label.chars().enumerate() {
                    let x = x_start + u16::try_from(i).unwrap_or(0);
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                        cell.set_style(style);
                    }
                }
            }
        }
    }
}

fn vt100_color_to_ratatui(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render a `TerminalWidget` into a `Buffer` and return it for assertions.
    fn render_widget(widget: TerminalWidget, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);
        buf
    }

    /// Extract the text content of a buffer row as a trimmed string.
    fn row_text(buf: &Buffer, row: u16) -> String {
        let area = buf.area;
        let mut s = String::new();
        for x in area.x..area.x + area.width {
            if let Some(cell) = buf.cell((x, row)) {
                s.push_str(cell.symbol());
            }
        }
        s.trim_end().to_string()
    }

    #[test]
    fn renders_plain_text() {
        let mut parser = vt100::Parser::new(5, 40, 0);
        parser.process(b"Hello, world!");
        let widget = TerminalWidget::new(parser.screen(), false);
        let buf = render_widget(widget, 40, 5);
        assert_eq!(row_text(&buf, 0), "Hello, world!");
    }

    #[test]
    fn renders_multiline_text() {
        let mut parser = vt100::Parser::new(5, 40, 0);
        parser.process(b"Line 1\r\nLine 2\r\nLine 3");
        let widget = TerminalWidget::new(parser.screen(), false);
        let buf = render_widget(widget, 40, 5);
        assert_eq!(row_text(&buf, 0), "Line 1");
        assert_eq!(row_text(&buf, 1), "Line 2");
        assert_eq!(row_text(&buf, 2), "Line 3");
    }

    #[test]
    fn cursor_shown_when_focused() {
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process(b"AB");
        // Cursor should be at (0, 2) after writing "AB"
        let widget = TerminalWidget::new(parser.screen(), true);
        let buf = render_widget(widget, 20, 5);
        let cell = buf.cell((2, 0)).unwrap();
        assert!(cell.modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn cursor_hidden_when_unfocused() {
        let mut parser = vt100::Parser::new(5, 20, 0);
        parser.process(b"AB");
        let widget = TerminalWidget::new(parser.screen(), false);
        let buf = render_widget(widget, 20, 5);
        let cell = buf.cell((2, 0)).unwrap();
        // Unfocused: cursor cell should NOT have REVERSED
        assert!(!cell.modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn bold_text_has_bold_modifier() {
        let mut parser = vt100::Parser::new(5, 40, 0);
        // ESC[1m = bold on, ESC[0m = reset
        parser.process(b"\x1b[1mBOLD\x1b[0m");
        let widget = TerminalWidget::new(parser.screen(), false);
        let buf = render_widget(widget, 40, 5);
        let cell = buf.cell((0, 0)).unwrap();
        assert!(cell.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn color_mapping_indexed() {
        let mut parser = vt100::Parser::new(5, 40, 0);
        // ESC[31m = red foreground (index 1)
        parser.process(b"\x1b[31mRed\x1b[0m");
        let widget = TerminalWidget::new(parser.screen(), false);
        let buf = render_widget(widget, 40, 5);
        let cell = buf.cell((0, 0)).unwrap();
        assert_eq!(cell.fg, Color::Indexed(1));
    }

    #[test]
    fn color_mapping_rgb() {
        let mut parser = vt100::Parser::new(5, 40, 0);
        // ESC[38;2;100;200;50m = RGB foreground
        parser.process(b"\x1b[38;2;100;200;50mRGB\x1b[0m");
        let widget = TerminalWidget::new(parser.screen(), false);
        let buf = render_widget(widget, 40, 5);
        let cell = buf.cell((0, 0)).unwrap();
        assert_eq!(cell.fg, Color::Rgb(100, 200, 50));
    }

    #[test]
    fn scroll_indicator_shown_when_scrolled_back() {
        let mut parser = vt100::Parser::new(5, 40, 0);
        parser.process(b"content");
        let widget = TerminalWidget::new(parser.screen(), false).with_scrollback_offset(42);
        let buf = render_widget(widget, 40, 5);
        // The scroll indicator should appear on the first row, right-aligned
        let line = row_text(&buf, 0);
        assert!(
            line.contains("[42 lines]"),
            "expected scroll indicator, got: {line}"
        );
    }

    #[test]
    fn no_scroll_indicator_at_live_screen() {
        let mut parser = vt100::Parser::new(5, 40, 0);
        parser.process(b"content");
        let widget = TerminalWidget::new(parser.screen(), false).with_scrollback_offset(0);
        let buf = render_widget(widget, 40, 5);
        let line = row_text(&buf, 0);
        assert!(
            !line.contains("lines"),
            "unexpected scroll indicator: {line}"
        );
    }

    #[test]
    fn selection_swaps_fg_bg() {
        let mut parser = vt100::Parser::new(5, 40, 0);
        parser.process(b"SELECT");
        // Select first 3 chars on row 0
        let sel = Selection {
            pane: 0,
            start: (0, 0),
            end: (0, 2),
        };
        let widget = TerminalWidget::new(parser.screen(), false).with_selection(Some(&sel));
        let buf = render_widget(widget, 40, 5);
        // Selected cell: bg should be non-Reset (swapped from fg)
        let cell = buf.cell((0, 0)).unwrap();
        // Default fg → bg becomes White (since default fg maps to Reset → White for selection)
        assert_eq!(cell.bg, Color::White);
        assert_eq!(cell.fg, Color::Black);
    }

    #[test]
    fn smaller_buffer_clips_gracefully() {
        let mut parser = vt100::Parser::new(10, 80, 0);
        parser.process(b"A long line that extends beyond the small buffer");
        // Render into a smaller area than the vt100 screen
        let widget = TerminalWidget::new(parser.screen(), false);
        let buf = render_widget(widget, 20, 3);
        let line = row_text(&buf, 0);
        assert_eq!(line.len(), 20); // clipped to buffer width
    }

    #[test]
    fn vt100_color_default_maps_to_reset() {
        assert_eq!(vt100_color_to_ratatui(vt100::Color::Default), Color::Reset);
    }

    #[test]
    fn vt100_color_idx_maps_to_indexed() {
        assert_eq!(
            vt100_color_to_ratatui(vt100::Color::Idx(42)),
            Color::Indexed(42)
        );
    }

    #[test]
    fn vt100_color_rgb_maps_to_rgb() {
        assert_eq!(
            vt100_color_to_ratatui(vt100::Color::Rgb(10, 20, 30)),
            Color::Rgb(10, 20, 30)
        );
    }
}
