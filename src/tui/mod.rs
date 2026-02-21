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
    execute!(stdout(), EnableMouseCapture, EnableBracketedPaste)?;
    let mut app = app::App::new(store)?;
    let result = app.run(&mut terminal);
    let _ = execute!(stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
    result
}
