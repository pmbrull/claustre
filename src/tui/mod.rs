mod app;
mod event;
mod ui;

use anyhow::Result;

use crate::store::Store;

pub fn run(store: Store) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = app::App::new(store)?;
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}
