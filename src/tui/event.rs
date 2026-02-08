use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEvent};

pub enum AppEvent {
    Key(KeyEvent),
    Tick,
}

pub fn poll(tick_rate: Duration) -> Result<AppEvent> {
    if event::poll(tick_rate)?
        && let Event::Key(key) = event::read()?
    {
        return Ok(AppEvent::Key(key));
    }
    Ok(AppEvent::Tick)
}
