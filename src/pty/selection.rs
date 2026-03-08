//! Text selection within a terminal pane (vt100 screen coordinates).

use super::PaneId;

/// A text selection within a terminal pane (vt100 screen coordinates).
#[derive(Clone, Copy)]
pub struct Selection {
    pub pane: PaneId,
    /// Start position (row, col) where mouse was pressed.
    pub start: (u16, u16),
    /// Current end position (row, col) where mouse was dragged/released.
    pub end: (u16, u16),
}

impl Selection {
    /// Return the selection bounds normalized so that `from` is before `to`.
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        let (sr, sc) = self.start;
        let (er, ec) = self.end;
        if sr < er || (sr == er && sc <= ec) {
            ((sr, sc), (er, ec))
        } else {
            ((er, ec), (sr, sc))
        }
    }

    /// Check if a cell at (row, col) is within this selection.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        let ((sr, sc), (er, ec)) = self.normalized();
        if row < sr || row > er {
            return false;
        }
        if sr == er {
            return col >= sc && col <= ec;
        }
        if row == sr {
            return col >= sc;
        }
        if row == er {
            return col <= ec;
        }
        true // middle row
    }

    /// Extract the selected text from a vt100 screen.
    pub fn extract_text(&self, screen: &vt100::Screen) -> String {
        let ((sr, sc), (er, ec)) = self.normalized();
        let mut text = String::new();
        let max_cols = screen.size().1;

        for row in sr..=er {
            let col_start = if row == sr { sc } else { 0 };
            let col_end = if row == er {
                ec
            } else {
                max_cols.saturating_sub(1)
            };

            for col in col_start..=col_end {
                if let Some(cell) = screen.cell(row, col) {
                    let contents = cell.contents();
                    if contents.is_empty() {
                        text.push(' ');
                    } else {
                        text.push_str(&contents);
                    }
                }
            }
            // Trim trailing spaces on each line and add newline between rows
            if row < er {
                let trimmed = text.trim_end_matches(' ');
                text.truncate(trimmed.len());
                text.push('\n');
            }
        }
        // Trim trailing spaces on the last line
        let trimmed = text.trim_end_matches(' ');
        text.truncate(trimmed.len());
        text
    }
}
