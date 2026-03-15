//! Claustre shared library.
//!
//! Re-exports all modules so they can be used by both the CLI binary
//! and the Tauri desktop app.

pub mod config;
pub mod configure;
pub mod pty;
pub mod scanner;
pub mod session;
pub mod session_host;
pub mod session_update;
pub mod skills;
pub mod store;
pub mod sync;
pub mod tui;
pub mod update;
