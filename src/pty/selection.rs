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

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(start: (u16, u16), end: (u16, u16)) -> Selection {
        Selection {
            pane: 0,
            start,
            end,
        }
    }

    // ── normalized ──

    #[test]
    fn normalized_already_ordered() {
        let s = sel((1, 5), (3, 10));
        assert_eq!(s.normalized(), ((1, 5), (3, 10)));
    }

    #[test]
    fn normalized_reversed_rows() {
        let s = sel((5, 2), (1, 8));
        assert_eq!(s.normalized(), ((1, 8), (5, 2)));
    }

    #[test]
    fn normalized_same_row_reversed_cols() {
        let s = sel((3, 10), (3, 2));
        assert_eq!(s.normalized(), ((3, 2), (3, 10)));
    }

    #[test]
    fn normalized_same_point() {
        let s = sel((4, 7), (4, 7));
        assert_eq!(s.normalized(), ((4, 7), (4, 7)));
    }

    // ── contains ──

    #[test]
    fn contains_single_row_selection() {
        let s = sel((2, 3), (2, 8));
        assert!(s.contains(2, 3));
        assert!(s.contains(2, 5));
        assert!(s.contains(2, 8));
        assert!(!s.contains(2, 2));
        assert!(!s.contains(2, 9));
        assert!(!s.contains(1, 5));
        assert!(!s.contains(3, 5));
    }

    #[test]
    fn contains_multi_row_start_row() {
        // Selection from (1, 5) to (3, 10)
        let s = sel((1, 5), (3, 10));
        // Start row: col >= 5
        assert!(s.contains(1, 5));
        assert!(s.contains(1, 79));
        assert!(!s.contains(1, 4));
    }

    #[test]
    fn contains_multi_row_end_row() {
        let s = sel((1, 5), (3, 10));
        // End row: col <= 10
        assert!(s.contains(3, 0));
        assert!(s.contains(3, 10));
        assert!(!s.contains(3, 11));
    }

    #[test]
    fn contains_multi_row_middle_row() {
        let s = sel((1, 5), (3, 10));
        // Middle row: all columns
        assert!(s.contains(2, 0));
        assert!(s.contains(2, 79));
    }

    #[test]
    fn contains_reversed_selection() {
        // End before start — normalized handles this
        let s = sel((3, 10), (1, 5));
        assert!(s.contains(2, 0));
        assert!(s.contains(1, 5));
        assert!(s.contains(3, 10));
        assert!(!s.contains(0, 0));
        assert!(!s.contains(4, 0));
    }

    // ── extract_text ──

    #[test]
    fn extract_text_single_line() {
        let parser = vt100::Parser::new(24, 80, 0);
        // Write "Hello World" to the screen
        let mut p = parser;
        p.process(b"Hello World");
        let screen = p.screen();

        let s = sel((0, 0), (0, 10));
        let text = s.extract_text(screen);
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn extract_text_partial_line() {
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(b"ABCDEFGHIJ");
        let screen = p.screen();

        let s = sel((0, 2), (0, 5));
        let text = s.extract_text(screen);
        assert_eq!(text, "CDEF");
    }

    #[test]
    fn extract_text_multi_line() {
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(b"Line one\r\nLine two\r\nLine three");
        let screen = p.screen();

        let s = sel((0, 5), (2, 3));
        let text = s.extract_text(screen);
        assert_eq!(text, "one\nLine two\nLine");
    }

    #[test]
    fn extract_text_trims_trailing_spaces() {
        let mut p = vt100::Parser::new(24, 80, 0);
        p.process(b"Hi");
        let screen = p.screen();

        // Select past the end of "Hi" — empty cells become spaces, but trailing should be trimmed
        let s = sel((0, 0), (0, 10));
        let text = s.extract_text(screen);
        assert_eq!(text, "Hi");
    }
}
