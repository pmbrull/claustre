use std::time::Duration;

use anyhow::Result;
use ratatui::DefaultTerminal;

use super::super::event::{self, AppEvent};
use super::super::ui;
use super::{App, DASHBOARD_TICK, SESSION_TICK, SLOW_TICK};

impl App {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            // Adaptive tick rate: fast when viewing PTY, slow on dashboard.
            let tick_rate = if self.active_tab > 0 {
                SESSION_TICK
            } else {
                DASHBOARD_TICK
            };

            // Process PTY output before every draw so frames always show the
            // freshest available data.  This catches output that arrived since
            // the last processing pass — critical when switching from the
            // dashboard (1 s ticks) to a session tab (16 ms ticks).
            self.process_pty_output();

            self.prepare_render_scrollback();
            terminal.draw(|frame| {
                self.last_terminal_area = frame.area();
                ui::draw(frame, self);
            })?;
            self.restore_live_scrollback();

            let prev_tab = self.active_tab;

            match event::poll(tick_rate)? {
                AppEvent::Key(key) => {
                    // When on a session tab, route most keys to the PTY
                    if self.active_tab > 0 {
                        self.handle_session_tab_key(key.code, key.modifiers)?;
                        // Drain any additional queued input events before redrawing.
                        // Mouse and resize events are handled inline so they
                        // aren't silently discarded.
                        while let Ok(extra) = event::poll(Duration::from_millis(0)) {
                            match extra {
                                AppEvent::Key(k) => {
                                    self.handle_session_tab_key(k.code, k.modifiers)?;
                                }
                                AppEvent::Paste(text) => {
                                    self.handle_session_tab_paste(&text)?;
                                }
                                AppEvent::Mouse(mouse) => {
                                    self.handle_mouse(mouse)?;
                                }
                                AppEvent::Resize(cols, rows) => {
                                    self.handle_resize(cols, rows);
                                }
                                AppEvent::Tick => break,
                            }
                        }
                        // Process PTY output immediately so the next frame reflects the keystroke
                        self.process_pty_output();
                    } else {
                        self.handle_dashboard_key(key.code, key.modifiers)?;
                    }
                }
                AppEvent::Paste(text) => {
                    if self.active_tab > 0 {
                        self.handle_session_tab_paste(&text)?;
                        self.process_pty_output();
                    } else {
                        self.handle_dashboard_paste(&text)?;
                    }
                }
                AppEvent::Mouse(mouse) => {
                    self.handle_mouse(mouse)?;
                    // On session tabs, drain queued events and process PTY
                    // output so the next frame reflects all pending scroll
                    // and input changes without intermediate redraws.
                    if self.active_tab > 0 {
                        while let Ok(extra) = event::poll(Duration::from_millis(0)) {
                            match extra {
                                AppEvent::Key(k) => {
                                    self.handle_session_tab_key(k.code, k.modifiers)?;
                                }
                                AppEvent::Paste(text) => {
                                    self.handle_session_tab_paste(&text)?;
                                }
                                AppEvent::Mouse(m) => {
                                    self.handle_mouse(m)?;
                                }
                                AppEvent::Resize(cols, rows) => {
                                    self.handle_resize(cols, rows);
                                }
                                AppEvent::Tick => break,
                            }
                        }
                        self.process_pty_output();
                    }
                }
                AppEvent::Tick => {
                    self.process_pty_output();
                    self.detect_paused_sessions();

                    // Fast-path tick work (always runs)
                    self.tick_toast();
                    self.poll_title_results()?;
                    self.poll_session_ops();
                    self.auto_launch_pending_tasks();
                    self.poll_pr_merge_results()?;
                    self.poll_git_stats_results();
                    self.poll_scanner_results();
                    self.poll_update_results();

                    // Slow-path tick work (DB refresh, background polls)
                    // Throttled on all tabs: dashboard ticks are now 200 ms,
                    // so we gate the heavy work behind the same elapsed check.
                    let run_slow = self.last_slow_tick.elapsed() >= SLOW_TICK;
                    if run_slow {
                        self.last_slow_tick = std::time::Instant::now();
                        self.maybe_poll_pr_merges();
                        self.maybe_poll_git_stats();
                        self.maybe_scan_external_sessions();
                        self.maybe_poll_update_check();
                        self.refresh_data()?;
                    }
                }
                AppEvent::Resize(cols, rows) => {
                    self.handle_resize(cols, rows);
                }
            }

            // When switching to a session tab, flush all pending PTY output
            // without a byte budget so the first frame shows fully current
            // content.  This eliminates the visible catch-up lag caused by
            // output accumulating during the slower dashboard tick interval.
            if self.active_tab != prev_tab && self.active_tab > 0 {
                self.flush_all_pty_output();
            }

            if self.should_quit {
                return Ok(());
            }
        }
    }
}
