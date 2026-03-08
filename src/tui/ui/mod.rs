mod dashboard;
mod forms;
mod overlays;
mod session;
mod tab_bar;
mod usage;

pub use tab_bar::compute_tab_layout;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    text::{Line, Span},
};

use super::app::{App, InputMode};

use dashboard::{draw_active, draw_active_in_area};
use forms::{draw_new_project_panel, draw_task_form_panel};
use overlays::{
    draw_command_palette, draw_help_overlay, draw_skill_add_overlay, draw_skill_panel,
    draw_skill_search_overlay, draw_subtask_panel, draw_task_details_panel,
};
use session::draw_session_tab;
use tab_bar::draw_tab_bar;

/// Returns an animated spinner character that cycles based on wall clock time.
pub(crate) fn spinner_char() -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const FRAME_MS: u128 = 250;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let idx = (ms / FRAME_MS) as usize % FRAMES.len();
    FRAMES[idx]
}

/// If a toast is active, return a styled `Line` for it; otherwise `None`.
pub(crate) fn toast_line(app: &App) -> Option<Line<'static>> {
    let msg = app.toast_message.as_ref()?;
    Some(Line::from(Span::styled(
        format!(" {msg} "),
        app.theme.toast_style(app.toast_style),
    )))
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    // If on a session tab, render the terminal view
    if app.active_tab > 0 {
        draw_session_tab(frame, app);
        return;
    }

    // Tab bar (only show if there are session tabs)
    if app.tabs.len() > 1 {
        let size = frame.area();
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(size);
        draw_tab_bar(frame, app, outer[0]);
        // Render the dashboard in the remaining area
        let sub_frame_area = outer[1];
        draw_active_in_area(frame, app, sub_frame_area);
    } else {
        draw_active(frame, app);
    }

    // Floating panel overlays
    match app.input_mode {
        InputMode::CommandPalette => draw_command_palette(frame, app),
        InputMode::NewTask => draw_task_form_panel(frame, app, " New Task "),
        InputMode::EditTask => draw_task_form_panel(frame, app, " Edit Task "),
        InputMode::NewProject => draw_new_project_panel(frame, app),
        InputMode::HelpOverlay => draw_help_overlay(frame, app),
        InputMode::TaskDetails => draw_task_details_panel(frame, app),
        InputMode::SubtaskPanel => draw_subtask_panel(frame, app),
        InputMode::SkillPanel => draw_skill_panel(frame, app),
        InputMode::SkillSearch | InputMode::SkillAdd => {
            draw_skill_panel(frame, app);
            if app.input_mode == InputMode::SkillSearch {
                draw_skill_search_overlay(frame, app);
            } else {
                draw_skill_add_overlay(frame, app);
            }
        }
        _ => {}
    }
}
