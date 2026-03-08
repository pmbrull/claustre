use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    widgets::{Block, Borders},
};

use crate::pty::{LayoutNode, PaneId, SplitDirection, TerminalWidget};

use super::super::app::{App, Tab};
use super::super::form::render_hints;
use super::tab_bar::draw_tab_bar;

/// Draw the session terminal view with a dynamic pane layout tree.
pub(super) fn draw_session_tab(frame: &mut Frame, app: &App) {
    let size = frame.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(0),    // terminal area
            Constraint::Length(1), // hint bar
        ])
        .split(size);

    draw_tab_bar(frame, app, outer[0]);

    if let Some(Tab::Session {
        terminals, label, ..
    }) = app.tabs.get(app.active_tab)
    {
        render_layout_node(
            &terminals.layout,
            terminals,
            label,
            &app.theme,
            frame,
            outer[1],
        );
    }

    // Hint bar
    render_hints(
        frame,
        outer[2],
        &[
            ("  Ctrl+D", ": dashboard  "),
            ("Ctrl+H/L", ": switch pane  "),
            ("Ctrl+J/K", ": switch tab  "),
            ("Ctrl+G", ": scroll bottom  "),
            ("Ctrl+R/B", ": split  "),
            ("Ctrl+W", ": close  "),
        ],
        Style::default().fg(app.theme.accent_secondary),
        Style::default(),
    );
}

/// Recursively render a layout node tree into the given area.
fn render_layout_node(
    node: &LayoutNode,
    terminals: &crate::pty::SessionTerminals,
    session_label: &str,
    theme: &super::super::theme::Theme,
    frame: &mut Frame,
    area: Rect,
) {
    match node {
        LayoutNode::Pane(id) => {
            render_single_pane(*id, terminals, session_label, theme, frame, area);
        }
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let dir = match direction {
                SplitDirection::Horizontal => Direction::Horizontal,
                SplitDirection::Vertical => Direction::Vertical,
            };
            let chunks = Layout::default()
                .direction(dir)
                .constraints([
                    Constraint::Percentage(*ratio),
                    Constraint::Percentage(100 - *ratio),
                ])
                .split(area);

            render_layout_node(first, terminals, session_label, theme, frame, chunks[0]);
            render_layout_node(second, terminals, session_label, theme, frame, chunks[1]);
        }
    }
}

/// Render a single terminal pane with border and title.
fn render_single_pane(
    id: PaneId,
    terminals: &crate::pty::SessionTerminals,
    session_label: &str,
    theme: &super::super::theme::Theme,
    frame: &mut Frame,
    area: Rect,
) {
    let Some(term) = terminals.terminal(id) else {
        return;
    };

    let is_focused = terminals.focused == id;
    let is_claude = id == terminals.claude_pane_id;

    let base_label = if is_claude {
        session_label.to_string()
    } else {
        terminals.label(id).to_string()
    };

    let scrollback = term.scrollback();
    let title = if scrollback > 0 {
        format!(" {base_label} [+{scrollback} lines] ")
    } else {
        format!(" {base_label} ")
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(if is_focused {
            theme.focused_border()
        } else {
            theme.unfocused_border()
        });

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sel = terminals.selection.as_ref().filter(|s| s.pane == id);

    frame.render_widget(
        TerminalWidget::new(term.screen(), is_focused)
            .with_selection(sel)
            .with_scrollback_offset(term.scrollback()),
        inner,
    );
}
