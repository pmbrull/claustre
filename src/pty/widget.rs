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
}

impl<'a> TerminalWidget<'a> {
    pub fn new(screen: &'a vt100::Screen, focused: bool) -> Self {
        Self {
            screen,
            focused,
            selection: None,
        }
    }

    pub fn with_selection(mut self, selection: Option<&'a Selection>) -> Self {
        self.selection = selection;
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
            let cx = area.x + cursor.1;
            let cy = area.y + cursor.0;
            if cx < area.x + area.width && cy < area.y + area.height {
                let cursor_selected = self
                    .selection
                    .is_some_and(|sel| sel.contains(cursor.0, cursor.1));
                if !cursor_selected {
                    if let Some(cell) = buf.cell_mut((cx, cy)) {
                        cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
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
