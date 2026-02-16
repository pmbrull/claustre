mod app;
mod event;
mod ui;

use std::io::stdout;

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;

use crate::store::Store;

pub fn run(store: Store) -> Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableMouseCapture)?;
    let mut app = app::App::new(store)?;
    let result = app.run(&mut terminal);
    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
