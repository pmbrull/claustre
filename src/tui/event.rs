use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEvent};

pub enum AppEvent {
    Key(KeyEvent),
    Tick,
    Resize(u16, u16),
}

pub fn poll(tick_rate: Duration) -> Result<AppEvent> {
    if event::poll(tick_rate)? {
        match event::read()? {
            Event::Key(key) => return Ok(AppEvent::Key(key)),
            Event::Resize(cols, rows) => return Ok(AppEvent::Resize(cols, rows)),
            _ => {}
        }
    }
    Ok(AppEvent::Tick)
}
