use ratatui::style::{Color, Modifier, Style};
use serde::Deserialize;

use crate::store::{ClaudeStatus, TaskStatus};

/// Semantic colour theme for the entire TUI.
///
/// Every colour used by the renderer is stored here so the user can
/// override any of them via `[theme]` in `config.toml`.
#[derive(Debug, Clone)]
pub struct Theme {
    // ── Borders ───────────────────────────────────────────────
    pub border_focused: Color,
    pub border_unfocused: Color,

    // ── Text ──────────────────────────────────────────────────
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_accent: Color,

    // ── Task status ───────────────────────────────────────────
    pub status_draft: Color,
    pub status_pending: Color,
    pub status_working: Color,
    pub status_interrupted: Color,
    pub status_in_review: Color,
    pub status_conflict: Color,
    pub status_ci_failed: Color,
    pub status_done: Color,
    pub status_error: Color,
    pub status_paused: Color,
    /// Style for the waiting override (Claude asked a question via `AskUserQuestion`).
    pub status_waiting: Color,

    // ── Accents ───────────────────────────────────────────────
    pub accent_primary: Color,
    pub accent_secondary: Color,
    pub accent_tertiary: Color,

    // ── Toast ─────────────────────────────────────────────────
    pub toast_info: Color,
    pub toast_success: Color,
    pub toast_error: Color,

    // ── Usage bars ────────────────────────────────────────────
    pub usage_low: Color,
    pub usage_medium: Color,
    pub usage_high: Color,

    // ── Forms ─────────────────────────────────────────────────
    pub form_border_task: Color,
    pub form_border_project: Color,
    pub form_highlight: Color,
    pub form_dim: Color,

    // ── Tabs ──────────────────────────────────────────────────
    pub tab_active: Color,
    pub tab_inactive: Color,

    // ── Misc ──────────────────────────────────────────────────
    pub selection_indicator: Color,
    pub pr_link: Color,
    pub spinner: Color,
    pub rate_limit_warning: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            border_focused: Color::Cyan,
            border_unfocused: Color::DarkGray,

            text_primary: Color::White,
            text_secondary: Color::DarkGray,
            text_accent: Color::Cyan,

            status_draft: Color::Cyan,
            status_pending: Color::DarkGray,
            status_working: Color::Green,
            status_interrupted: Color::Magenta,
            status_in_review: Color::Yellow,
            status_conflict: Color::Rgb(255, 165, 0),
            status_ci_failed: Color::LightRed,
            status_done: Color::Blue,
            status_error: Color::Red,
            status_paused: Color::Yellow,
            status_waiting: Color::Cyan,

            accent_primary: Color::Cyan,
            accent_secondary: Color::Yellow,
            accent_tertiary: Color::Magenta,

            toast_info: Color::Cyan,
            toast_success: Color::Green,
            toast_error: Color::Red,

            usage_low: Color::Green,
            usage_medium: Color::Yellow,
            usage_high: Color::Red,

            form_border_task: Color::Yellow,
            form_border_project: Color::Magenta,
            form_highlight: Color::Yellow,
            form_dim: Color::DarkGray,

            tab_active: Color::Cyan,
            tab_inactive: Color::DarkGray,

            selection_indicator: Color::Cyan,
            pr_link: Color::Magenta,
            spinner: Color::Yellow,
            rate_limit_warning: Color::Red,
        }
    }
}

impl Theme {
    /// Style for a focused panel border.
    pub fn focused_border(&self) -> Style {
        Style::default().fg(self.border_focused)
    }

    /// Style for an unfocused panel border.
    pub fn unfocused_border(&self) -> Style {
        Style::default().fg(self.border_unfocused)
    }

    /// Map a `TaskStatus` to its display style (foreground colour).
    pub fn task_status_style(&self, status: TaskStatus) -> Style {
        let color = match status {
            TaskStatus::Draft => self.status_draft,
            TaskStatus::Pending => self.status_pending,
            TaskStatus::Working => self.status_working,
            TaskStatus::Interrupted => self.status_interrupted,
            TaskStatus::InReview => self.status_in_review,
            TaskStatus::Conflict => self.status_conflict,
            TaskStatus::CiFailed => self.status_ci_failed,
            TaskStatus::Done => self.status_done,
            TaskStatus::Error => self.status_error,
        };
        Style::default().fg(color)
    }

    /// Map a `ClaudeStatus` to its display style (foreground colour).
    pub fn claude_status_style(&self, status: ClaudeStatus) -> Style {
        let color = match status {
            ClaudeStatus::Working => self.status_working,
            ClaudeStatus::Interrupted => self.status_interrupted,
            ClaudeStatus::Error => self.status_error,
            ClaudeStatus::Done => self.status_done,
            ClaudeStatus::Idle => self.status_pending,
        };
        Style::default().fg(color)
    }

    /// Style for the paused override (detected from PTY screen).
    pub fn paused_style(&self) -> Style {
        Style::default().fg(self.status_paused)
    }

    /// Style for the waiting override (Claude asked a question, detected from PTY screen).
    pub fn waiting_style(&self) -> Style {
        Style::default().fg(self.status_waiting)
    }

    /// Style for the active tab label.
    pub fn tab_active_style(&self) -> Style {
        Style::default()
            .fg(self.tab_active)
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    }

    /// Style for an inactive tab label.
    pub fn tab_inactive_style(&self) -> Style {
        Style::default().fg(self.tab_inactive)
    }

    /// Style for a toast notification.
    pub fn toast_style(&self, style: super::app::ToastStyle) -> Style {
        let color = match style {
            super::app::ToastStyle::Info => self.toast_info,
            super::app::ToastStyle::Success => self.toast_success,
            super::app::ToastStyle::Error => self.toast_error,
        };
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    }

    /// Colour for a usage bar at the given percentage.
    pub fn usage_bar_color(&self, pct: f64) -> Color {
        if pct > 90.0 {
            self.usage_high
        } else if pct >= 70.0 {
            self.usage_medium
        } else {
            self.usage_low
        }
    }
}

// ── Config deserialization ────────────────────────────────────────────

/// All-optional mirror of [`Theme`] for `config.toml` `[theme]` section.
///
/// Only `Some` fields override the default; everything else keeps its default.
#[derive(Debug, Default, Deserialize, Clone)]
pub struct ThemeConfig {
    pub border_focused: Option<String>,
    pub border_unfocused: Option<String>,

    pub text_primary: Option<String>,
    pub text_secondary: Option<String>,
    pub text_accent: Option<String>,

    pub status_draft: Option<String>,
    pub status_pending: Option<String>,
    pub status_working: Option<String>,
    pub status_interrupted: Option<String>,
    pub status_in_review: Option<String>,
    pub status_conflict: Option<String>,
    pub status_ci_failed: Option<String>,
    pub status_done: Option<String>,
    pub status_error: Option<String>,
    pub status_paused: Option<String>,
    pub status_waiting: Option<String>,

    pub accent_primary: Option<String>,
    pub accent_secondary: Option<String>,
    pub accent_tertiary: Option<String>,

    pub toast_info: Option<String>,
    pub toast_success: Option<String>,
    pub toast_error: Option<String>,

    pub usage_low: Option<String>,
    pub usage_medium: Option<String>,
    pub usage_high: Option<String>,

    pub form_border_task: Option<String>,
    pub form_border_project: Option<String>,
    pub form_highlight: Option<String>,
    pub form_dim: Option<String>,

    pub tab_active: Option<String>,
    pub tab_inactive: Option<String>,

    pub selection_indicator: Option<String>,
    pub pr_link: Option<String>,
    pub spinner: Option<String>,
    pub rate_limit_warning: Option<String>,
}

/// Parse a colour string into a ratatui `Color`.
///
/// Supports named colours (`"cyan"`, `"red"`, `"dark_gray"`, etc.) and
/// `"rgb(R,G,B)"` syntax.
fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    // Try rgb(R,G,B)
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            let r = parts[0].trim().parse::<u8>().ok()?;
            let g = parts[1].trim().parse::<u8>().ok()?;
            let b = parts[2].trim().parse::<u8>().ok()?;
            return Some(Color::Rgb(r, g, b));
        }
        return None;
    }

    // Named colours (case-insensitive, with underscore tolerance)
    let lower = s.to_lowercase().replace('-', "_");
    match lower.as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "gray" | "grey" => Some(Color::Gray),
        "dark_gray" | "dark_grey" | "darkgray" | "darkgrey" => Some(Color::DarkGray),
        "light_red" | "lightred" => Some(Color::LightRed),
        "light_green" | "lightgreen" => Some(Color::LightGreen),
        "light_yellow" | "lightyellow" => Some(Color::LightYellow),
        "light_blue" | "lightblue" => Some(Color::LightBlue),
        "light_magenta" | "lightmagenta" => Some(Color::LightMagenta),
        "light_cyan" | "lightcyan" => Some(Color::LightCyan),
        "white" => Some(Color::White),
        _ => None,
    }
}

/// Apply an optional config field: if the string parses to a valid colour,
/// overwrite `target`.
fn apply(target: &mut Color, source: Option<&String>) {
    if let Some(s) = source
        && let Some(color) = parse_color(s)
    {
        *target = color;
    }
}

impl ThemeConfig {
    /// Build a `Theme` starting from defaults, overriding any fields that were
    /// set in the config file.
    pub fn build(&self) -> Theme {
        let mut t = Theme::default();

        apply(&mut t.border_focused, self.border_focused.as_ref());
        apply(&mut t.border_unfocused, self.border_unfocused.as_ref());
        apply(&mut t.text_primary, self.text_primary.as_ref());
        apply(&mut t.text_secondary, self.text_secondary.as_ref());
        apply(&mut t.text_accent, self.text_accent.as_ref());
        apply(&mut t.status_draft, self.status_draft.as_ref());
        apply(&mut t.status_pending, self.status_pending.as_ref());
        apply(&mut t.status_working, self.status_working.as_ref());
        apply(&mut t.status_interrupted, self.status_interrupted.as_ref());
        apply(&mut t.status_in_review, self.status_in_review.as_ref());
        apply(&mut t.status_conflict, self.status_conflict.as_ref());
        apply(&mut t.status_ci_failed, self.status_ci_failed.as_ref());
        apply(&mut t.status_done, self.status_done.as_ref());
        apply(&mut t.status_error, self.status_error.as_ref());
        apply(&mut t.status_paused, self.status_paused.as_ref());
        apply(&mut t.status_waiting, self.status_waiting.as_ref());
        apply(&mut t.accent_primary, self.accent_primary.as_ref());
        apply(&mut t.accent_secondary, self.accent_secondary.as_ref());
        apply(&mut t.accent_tertiary, self.accent_tertiary.as_ref());
        apply(&mut t.toast_info, self.toast_info.as_ref());
        apply(&mut t.toast_success, self.toast_success.as_ref());
        apply(&mut t.toast_error, self.toast_error.as_ref());
        apply(&mut t.usage_low, self.usage_low.as_ref());
        apply(&mut t.usage_medium, self.usage_medium.as_ref());
        apply(&mut t.usage_high, self.usage_high.as_ref());
        apply(&mut t.form_border_task, self.form_border_task.as_ref());
        apply(
            &mut t.form_border_project,
            self.form_border_project.as_ref(),
        );
        apply(&mut t.form_highlight, self.form_highlight.as_ref());
        apply(&mut t.form_dim, self.form_dim.as_ref());
        apply(&mut t.tab_active, self.tab_active.as_ref());
        apply(&mut t.tab_inactive, self.tab_inactive.as_ref());
        apply(
            &mut t.selection_indicator,
            self.selection_indicator.as_ref(),
        );
        apply(&mut t.pr_link, self.pr_link.as_ref());
        apply(&mut t.spinner, self.spinner.as_ref());
        apply(&mut t.rate_limit_warning, self.rate_limit_warning.as_ref());

        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_has_expected_colors() {
        let t = Theme::default();
        assert_eq!(t.border_focused, Color::Cyan);
        assert_eq!(t.status_conflict, Color::Rgb(255, 165, 0));
        assert_eq!(t.text_primary, Color::White);
    }

    #[test]
    fn parse_named_colors() {
        assert_eq!(parse_color("cyan"), Some(Color::Cyan));
        assert_eq!(parse_color("dark_gray"), Some(Color::DarkGray));
        assert_eq!(parse_color("DarkGray"), Some(Color::DarkGray));
        assert_eq!(parse_color("light_red"), Some(Color::LightRed));
        assert_eq!(parse_color("white"), Some(Color::White));
        assert_eq!(parse_color("nope"), None);
    }

    #[test]
    fn parse_rgb_color() {
        assert_eq!(
            parse_color("rgb(255, 165, 0)"),
            Some(Color::Rgb(255, 165, 0))
        );
        assert_eq!(parse_color("rgb(0,0,0)"), Some(Color::Rgb(0, 0, 0)));
        assert_eq!(parse_color("rgb(256,0,0)"), None); // overflow
        assert_eq!(parse_color("rgb(1,2)"), None); // too few
    }

    #[test]
    fn theme_config_overrides() {
        let cfg = ThemeConfig {
            border_focused: Some("red".into()),
            status_conflict: Some("rgb(100,200,50)".into()),
            ..Default::default()
        };
        let t = cfg.build();
        assert_eq!(t.border_focused, Color::Red);
        assert_eq!(t.status_conflict, Color::Rgb(100, 200, 50));
        // Non-overridden field keeps default
        assert_eq!(t.text_primary, Color::White);
    }

    #[test]
    fn task_status_style_maps_correctly() {
        let t = Theme::default();
        assert_eq!(
            t.task_status_style(TaskStatus::Working),
            Style::default().fg(Color::Green)
        );
        assert_eq!(
            t.task_status_style(TaskStatus::Error),
            Style::default().fg(Color::Red)
        );
    }

    #[test]
    fn claude_status_style_maps_correctly() {
        let t = Theme::default();
        assert_eq!(
            t.claude_status_style(ClaudeStatus::Working),
            Style::default().fg(Color::Green)
        );
        assert_eq!(
            t.claude_status_style(ClaudeStatus::Done),
            Style::default().fg(Color::Blue)
        );
    }

    #[test]
    fn usage_bar_color_thresholds() {
        let t = Theme::default();
        assert_eq!(t.usage_bar_color(50.0), Color::Green);
        assert_eq!(t.usage_bar_color(75.0), Color::Yellow);
        assert_eq!(t.usage_bar_color(95.0), Color::Red);
    }

    #[test]
    fn focused_and_unfocused_border_styles() {
        let t = Theme::default();
        assert_eq!(t.focused_border(), Style::default().fg(Color::Cyan));
        assert_eq!(t.unfocused_border(), Style::default().fg(Color::DarkGray));
    }

    #[test]
    fn tab_styles() {
        let t = Theme::default();
        let active = t.tab_active_style();
        assert_eq!(active.fg, Some(t.tab_active));
        assert!(active.add_modifier.contains(Modifier::BOLD));
        assert!(active.add_modifier.contains(Modifier::REVERSED));

        let inactive = t.tab_inactive_style();
        assert_eq!(inactive.fg, Some(t.tab_inactive));
    }

    #[test]
    fn paused_style_uses_paused_color() {
        let t = Theme::default();
        assert_eq!(t.paused_style(), Style::default().fg(t.status_paused));
    }

    #[test]
    fn waiting_style_uses_waiting_color() {
        let t = Theme::default();
        assert_eq!(t.waiting_style(), Style::default().fg(t.status_waiting));
    }

    #[test]
    fn toast_styles() {
        use crate::tui::app::ToastStyle;
        let t = Theme::default();

        let info = t.toast_style(ToastStyle::Info);
        assert_eq!(info.fg, Some(t.toast_info));
        assert!(info.add_modifier.contains(Modifier::BOLD));

        let success = t.toast_style(ToastStyle::Success);
        assert_eq!(success.fg, Some(t.toast_success));

        let error = t.toast_style(ToastStyle::Error);
        assert_eq!(error.fg, Some(t.toast_error));
    }

    #[test]
    fn all_task_statuses_have_styles() {
        let t = Theme::default();
        let statuses = [
            TaskStatus::Draft,
            TaskStatus::Pending,
            TaskStatus::Working,
            TaskStatus::Interrupted,
            TaskStatus::InReview,
            TaskStatus::Conflict,
            TaskStatus::CiFailed,
            TaskStatus::Done,
            TaskStatus::Error,
        ];
        for status in statuses {
            let style = t.task_status_style(status);
            assert!(style.fg.is_some(), "missing style for {status:?}");
        }
    }

    #[test]
    fn all_claude_statuses_have_styles() {
        let t = Theme::default();
        let statuses = [
            ClaudeStatus::Working,
            ClaudeStatus::Interrupted,
            ClaudeStatus::Error,
            ClaudeStatus::Done,
            ClaudeStatus::Idle,
        ];
        for status in statuses {
            let style = t.claude_status_style(status);
            assert!(style.fg.is_some(), "missing style for {status:?}");
        }
    }

    #[test]
    fn usage_bar_color_boundary_values() {
        let t = Theme::default();
        // Exactly at thresholds
        assert_eq!(t.usage_bar_color(70.0), Color::Yellow);
        assert_eq!(t.usage_bar_color(90.0), Color::Yellow);
        assert_eq!(t.usage_bar_color(90.1), Color::Red);
        assert_eq!(t.usage_bar_color(0.0), Color::Green);
        assert_eq!(t.usage_bar_color(69.9), Color::Green);
    }
}
