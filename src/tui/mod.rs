//! Terminal UI built on ratatui.
//!
//! Manages the app state machine, crossterm event loop, and rendering
//! for the dashboard, session tabs, and overlay panels.

mod app;
mod event;
pub mod form;
pub mod keymap;
pub mod theme;
mod ui;

use std::io::stdout;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;

use crate::store::Store;

pub fn run(store: Store) -> Result<()> {
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableMouseCapture, EnableBracketedPaste);

    let result = app::App::new(store).and_then(|mut app| app.run(&mut terminal));

    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}
