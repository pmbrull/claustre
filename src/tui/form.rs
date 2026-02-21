use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

// ── Text editing helpers ──────────────────────────────────────────────

/// Find the byte offset of the previous word boundary (for word-left navigation).
pub fn word_boundary_left(s: &str, pos: usize) -> usize {
    let before = &s[..pos];
    // Skip trailing whitespace
    let trimmed = before.trim_end();
    if trimmed.is_empty() {
        return 0;
    }
    // Find last whitespace char before the word
    match trimmed.rfind(|c: char| c.is_whitespace()) {
        Some(idx) => {
            let ch = trimmed[idx..].chars().next().expect("non-empty slice");
            idx + ch.len_utf8()
        }
        None => 0,
    }
}

/// Find the byte offset of the next word boundary (for word-right navigation).
pub fn word_boundary_right(s: &str, pos: usize) -> usize {
    let after = &s[pos..];
    // Skip leading non-whitespace (rest of current word)
    let ws_start = after.find(|c: char| c.is_whitespace());
    match ws_start {
        None => s.len(),
        Some(offset) => {
            let from_ws = &after[offset..];
            // Skip whitespace to find start of next word
            match from_ws.find(|c: char| !c.is_whitespace()) {
                None => s.len(),
                Some(word_start) => pos + offset + word_start,
            }
        }
    }
}

/// Apply standard text-editing shortcuts to a string buffer with cursor tracking.
/// Handles character insertion, deletion, cursor movement (arrow keys, word/line jumps),
/// and line deletion (Super+Backspace / Ctrl+U).
/// Returns `true` if the key event was consumed.
pub fn apply_text_edit(
    buf: &mut String,
    cursor: &mut usize,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> bool {
    // Safety: clamp cursor to buffer length
    *cursor = (*cursor).min(buf.len());

    match code {
        // --- Cursor movement ---
        KeyCode::Left if modifiers.contains(KeyModifiers::SUPER) => {
            *cursor = 0;
            true
        }
        KeyCode::Left if modifiers.contains(KeyModifiers::ALT) => {
            *cursor = word_boundary_left(buf, *cursor);
            true
        }
        KeyCode::Left => {
            if *cursor > 0 {
                let before = &buf[..*cursor];
                if let Some(ch) = before.chars().next_back() {
                    *cursor -= ch.len_utf8();
                }
            }
            true
        }
        KeyCode::Right if modifiers.contains(KeyModifiers::SUPER) => {
            *cursor = buf.len();
            true
        }
        KeyCode::Right if modifiers.contains(KeyModifiers::ALT) => {
            *cursor = word_boundary_right(buf, *cursor);
            true
        }
        KeyCode::Right => {
            if *cursor < buf.len() {
                let ch = buf[*cursor..].chars().next().expect("cursor within bounds");
                *cursor += ch.len_utf8();
            }
            true
        }
        KeyCode::Home => {
            *cursor = 0;
            true
        }
        KeyCode::End => {
            *cursor = buf.len();
            true
        }

        // --- Deletion ---
        KeyCode::Backspace if modifiers.contains(KeyModifiers::ALT) => {
            let new_pos = word_boundary_left(buf, *cursor);
            buf.drain(new_pos..*cursor);
            *cursor = new_pos;
            true
        }
        KeyCode::Backspace if modifiers.contains(KeyModifiers::SUPER) => {
            buf.drain(..*cursor);
            *cursor = 0;
            true
        }
        KeyCode::Char('w') if modifiers.contains(KeyModifiers::CONTROL) => {
            let new_pos = word_boundary_left(buf, *cursor);
            buf.drain(new_pos..*cursor);
            *cursor = new_pos;
            true
        }
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
            buf.drain(..*cursor);
            *cursor = 0;
            true
        }
        KeyCode::Backspace => {
            if *cursor > 0 {
                let before = &buf[..*cursor];
                if let Some(ch) = before.chars().next_back() {
                    let new_pos = *cursor - ch.len_utf8();
                    buf.drain(new_pos..*cursor);
                    *cursor = new_pos;
                }
            }
            true
        }
        KeyCode::Delete => {
            if *cursor < buf.len() {
                let ch = buf[*cursor..].chars().next().expect("cursor within bounds");
                buf.drain(*cursor..(*cursor + ch.len_utf8()));
            }
            true
        }

        // --- Character insertion ---
        KeyCode::Char(c) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
            buf.insert(*cursor, c);
            *cursor += c.len_utf8();
            true
        }
        _ => false,
    }
}

/// Format a text buffer with a visible block cursor at the given position.
pub fn format_with_cursor(buf: &str, cursor: usize) -> String {
    let pos = cursor.min(buf.len());
    let (before, after) = buf.split_at(pos);
    format!("{before}\u{2588}{after}")
}

// ── Rendering helpers ─────────────────────────────────────────────────

/// Render a centered modal overlay: `Clear` background, bordered block, returns inner `Rect`.
///
/// Centres a panel of the given `width`×`height` on screen, clamping to available space.
/// The caller gets back the usable inner area (inside borders).
pub fn render_modal(
    frame: &mut Frame,
    title: &str,
    border_color: Style,
    width: u16,
    height: u16,
) -> Rect {
    let area = frame.area();
    let w = width.min(area.width.saturating_sub(4));
    let h = height.min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(w)) / 2;
    let y = (area.height.saturating_sub(h)) / 2;
    let panel = Rect::new(x, y, w, h);

    frame.render_widget(Clear, panel);

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_color);
    let inner = block.inner(panel);
    frame.render_widget(block, panel);

    inner
}

/// Measure how many visual lines a paragraph of `text` would occupy at `width`,
/// using ratatui's own word-wrapping (consistent with `Wrap { trim: false }`).
/// Returns at least 1.
pub fn measure_wrapped_height(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(width)
        .max(1) as u16
}

/// Render a horizontal hint bar: alternating key/description spans.
///
/// Each `(key_label, description)` pair is rendered as:
///   `Span::styled(key_label, key_style)` + `Span::styled(description, desc_style)`
pub fn render_hints(
    frame: &mut Frame,
    area: Rect,
    hints: &[(&str, &str)],
    key_style: Style,
    desc_style: Style,
) {
    let spans: Vec<Span<'_>> = hints
        .iter()
        .flat_map(|(key, desc)| {
            [
                Span::styled(*key, key_style),
                Span::styled(*desc, desc_style),
            ]
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // --- word_boundary_left ---

    #[test]
    fn word_boundary_left_from_end() {
        assert_eq!(word_boundary_left("hello world", 11), 6);
    }

    #[test]
    fn word_boundary_left_with_trailing_spaces() {
        assert_eq!(word_boundary_left("hello world  ", 13), 6);
    }

    #[test]
    fn word_boundary_left_single_word() {
        assert_eq!(word_boundary_left("hello", 5), 0);
    }

    #[test]
    fn word_boundary_left_empty() {
        assert_eq!(word_boundary_left("", 0), 0);
    }

    #[test]
    fn word_boundary_left_whitespace_only() {
        assert_eq!(word_boundary_left("   ", 3), 0);
    }

    // --- word_boundary_right ---

    #[test]
    fn word_boundary_right_from_start() {
        assert_eq!(word_boundary_right("hello world", 0), 6);
    }

    #[test]
    fn word_boundary_right_from_mid_word() {
        assert_eq!(word_boundary_right("hello world", 2), 6);
    }

    #[test]
    fn word_boundary_right_at_end() {
        assert_eq!(word_boundary_right("hello world", 11), 11);
    }

    // --- apply_text_edit ---

    #[test]
    fn apply_text_edit_alt_backspace_deletes_word() {
        let mut buf = String::from("hello world");
        let mut cursor = buf.len();
        let consumed =
            apply_text_edit(&mut buf, &mut cursor, KeyCode::Backspace, KeyModifiers::ALT);
        assert!(consumed);
        assert_eq!(buf, "hello ");
        assert_eq!(cursor, 6);
    }

    #[test]
    fn apply_text_edit_super_backspace_clears_before_cursor() {
        let mut buf = String::from("hello world");
        let mut cursor = buf.len();
        let consumed = apply_text_edit(
            &mut buf,
            &mut cursor,
            KeyCode::Backspace,
            KeyModifiers::SUPER,
        );
        assert!(consumed);
        assert_eq!(buf, "");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn apply_text_edit_ctrl_w_deletes_word() {
        let mut buf = String::from("hello world");
        let mut cursor = buf.len();
        let consumed = apply_text_edit(
            &mut buf,
            &mut cursor,
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        );
        assert!(consumed);
        assert_eq!(buf, "hello ");
        assert_eq!(cursor, 6);
    }

    #[test]
    fn apply_text_edit_ctrl_u_clears_before_cursor() {
        let mut buf = String::from("hello world");
        let mut cursor = buf.len();
        let consumed = apply_text_edit(
            &mut buf,
            &mut cursor,
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
        );
        assert!(consumed);
        assert_eq!(buf, "");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn apply_text_edit_regular_char() {
        let mut buf = String::from("hell");
        let mut cursor = buf.len();
        let consumed = apply_text_edit(
            &mut buf,
            &mut cursor,
            KeyCode::Char('o'),
            KeyModifiers::NONE,
        );
        assert!(consumed);
        assert_eq!(buf, "hello");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn apply_text_edit_ctrl_char_not_consumed() {
        let mut buf = String::from("hello");
        let mut cursor = buf.len();
        let consumed = apply_text_edit(
            &mut buf,
            &mut cursor,
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        );
        assert!(!consumed);
        assert_eq!(buf, "hello");
    }

    #[test]
    fn apply_text_edit_left_arrow_moves_cursor() {
        let mut buf = String::from("hello");
        let mut cursor = 5;
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(cursor, 4);
        assert_eq!(buf, "hello");
    }

    #[test]
    fn apply_text_edit_right_arrow_moves_cursor() {
        let mut buf = String::from("hello");
        let mut cursor = 0;
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(cursor, 1);
    }

    #[test]
    fn apply_text_edit_insert_at_cursor() {
        let mut buf = String::from("hllo");
        let mut cursor = 1;
        apply_text_edit(
            &mut buf,
            &mut cursor,
            KeyCode::Char('e'),
            KeyModifiers::NONE,
        );
        assert_eq!(buf, "hello");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn apply_text_edit_backspace_at_cursor() {
        let mut buf = String::from("heello");
        let mut cursor = 3;
        apply_text_edit(
            &mut buf,
            &mut cursor,
            KeyCode::Backspace,
            KeyModifiers::NONE,
        );
        assert_eq!(buf, "hello");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn apply_text_edit_delete_at_cursor() {
        let mut buf = String::from("heello");
        let mut cursor = 2;
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(buf, "hello");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn apply_text_edit_home_end() {
        let mut buf = String::from("hello");
        let mut cursor = 3;
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(cursor, 0);
        apply_text_edit(&mut buf, &mut cursor, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(cursor, 5);
    }

    #[test]
    fn apply_text_edit_alt_left_right_word_jump() {
        let mut buf = String::from("hello world test");
        let mut cursor = buf.len();
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(cursor, 12); // before "test"
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(cursor, 6); // before "world"
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(cursor, 12); // after "world " (at start of "test")
    }

    #[test]
    fn apply_text_edit_super_left_right_line_jump() {
        let mut buf = String::from("hello world");
        let mut cursor = 5;
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Left, KeyModifiers::SUPER);
        assert_eq!(cursor, 0);
        apply_text_edit(&mut buf, &mut cursor, KeyCode::Right, KeyModifiers::SUPER);
        assert_eq!(cursor, 11);
    }

    // --- format_with_cursor ---

    #[test]
    fn format_with_cursor_at_positions() {
        assert_eq!(format_with_cursor("hello", 0), "\u{2588}hello");
        assert_eq!(format_with_cursor("hello", 2), "he\u{2588}llo");
        assert_eq!(format_with_cursor("hello", 5), "hello\u{2588}");
    }
}
