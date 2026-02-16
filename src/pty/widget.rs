use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// A ratatui widget that renders a vt100 terminal screen.
pub struct TerminalWidget<'a> {
    screen: &'a vt100::Screen,
    focused: bool,
}

impl<'a> TerminalWidget<'a> {
    pub fn new(screen: &'a vt100::Screen, focused: bool) -> Self {
        Self { screen, focused }
    }
}

impl Widget for TerminalWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows = area.height.min(self.screen.size().0);
        let cols = area.width.min(self.screen.size().1);

        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = self.screen.cell(row, col) {
                    let style = vt100_to_ratatui_style(cell);
                    let contents = cell.contents();
                    let c = if contents.is_empty() { " " } else { &contents };
                    buf.set_string(area.x + col, area.y + row, c, style);
                }
            }
        }

        // Draw cursor if focused
        if self.focused {
            let cursor = self.screen.cursor_position();
            let cx = area.x + cursor.1;
            let cy = area.y + cursor.0;
            if cx < area.x + area.width && cy < area.y + area.height {
                if let Some(cell) = buf.cell_mut((cx, cy)) {
                    cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }
}

fn vt100_to_ratatui_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();

    style = style.fg(vt100_color_to_ratatui(cell.fgcolor()));
    style = style.bg(vt100_color_to_ratatui(cell.bgcolor()));

    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }

    style
}

fn vt100_color_to_ratatui(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
