//! Native PTY embedding via `portable-pty` and `vt100`.
//!
//! Provides `EmbeddedTerminal` (local PTY backend),
//! `SessionTerminals` (tree-based pane layout), and the rendering widget.

pub mod protocol;
mod widget;
pub use widget::TerminalWidget;

mod embedded;
mod layout;
mod selection;
pub(crate) mod session_terminals;

#[cfg(test)]
pub(crate) use embedded::Backend;
pub use embedded::EmbeddedTerminal;
pub use layout::{LayoutNode, SplitDirection};
pub use selection::Selection;
pub use session_terminals::SessionTerminals;

/// Unique identifier for a pane within a session.
pub type PaneId = u16;

/// Maximum bytes to feed into the vt100 parser per `process_output()` call.
/// Limits how long the UI thread is blocked parsing terminal output on each
/// tick, keeping the interface responsive even when a session produces a
/// massive burst of output (e.g. a large diff).  Data beyond this budget
/// stays in the channel and is drained on subsequent ticks.
///
/// 256 KB at 60 fps ≈ 15 MB/s sustained throughput — well above normal
/// interactive output while preventing multi-second freezes on bulk data.
const PROCESS_BYTE_BUDGET: usize = 256 * 1024;

/// Lines of scrollback history kept by the vt100 parser.
const SCROLLBACK_LINES: usize = 5_000;

/// Divisor for proportional scroll-down speed.
/// Each `scroll_down` moves at least `lines` rows but also at least
/// `scroll_offset / SCROLL_DOWN_ACCEL_DIVISOR` rows, giving geometric
/// convergence toward the live screen.  With divisor 4, each event
/// covers 25% of the remaining distance — roughly 17 events to traverse
/// the full 5 000-line scrollback buffer and reach the snap zone.
const SCROLL_DOWN_ACCEL_DIVISOR: usize = 4;

// ── Scrollback regression tests ──
//
// These guard the invariants that have repeatedly broken in production:
// - scroll_offset stays 0 when the user hasn't scrolled
// - scroll_offset is preserved during active output (no auto-decay)
// - budget-limited vs unbounded processing behaves correctly
// - resize and alternate-screen transitions reset the offset

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Create a test terminal with a **controlled** output channel.
    ///
    /// Uses `Backend::Mock` — no real PTY file descriptors are allocated,
    /// so tests can run fully in parallel without exhausting the OS PTY limit.
    /// The returned `Sender` is the only way to inject data.
    fn test_terminal(rows: u16, cols: u16) -> (EmbeddedTerminal, mpsc::Sender<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();

        let term = EmbeddedTerminal {
            backend: Backend::Mock,
            output_rx: rx,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK_LINES),
            exited: false,
            scroll_offset: 0,
            available_scrollback: 0,
        };
        (term, tx)
    }

    // ── Budget enforcement ──

    #[test]
    fn process_output_respects_byte_budget() {
        let (mut term, tx) = test_terminal(24, 80);
        // 400 × 1 KB = 400 KB — exceeds the 256 KB budget.
        for _ in 0..400 {
            tx.send(vec![b'x'; 1024]).unwrap();
        }
        drop(tx); // close sender so Disconnected fires when channel is empty

        term.process_output(); // budget-limited
        assert!(
            !term.exited,
            "budget-limited call must stop before draining entire channel"
        );
    }

    #[test]
    fn process_output_full_drains_everything() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..400 {
            tx.send(vec![b'x'; 1024]).unwrap();
        }
        drop(tx);

        term.process_output_full(); // unbounded
        assert!(
            term.exited,
            "unbounded call must drain all output and see Disconnected"
        );
    }

    // ── Scrollback basics ──

    #[test]
    fn initial_scroll_offset_is_zero() {
        let (term, _tx) = test_terminal(24, 80);
        assert_eq!(term.scroll_offset, 0);
        assert_eq!(term.available_scrollback, 0);
    }

    #[test]
    fn scroll_offset_unchanged_without_user_action() {
        let (mut term, tx) = test_terminal(24, 80);
        for i in 0..200 {
            tx.send(format!("line {i}\r\n").into_bytes()).unwrap();
        }
        term.process_output();

        assert_eq!(
            term.scroll_offset, 0,
            "scroll_offset must stay 0 when user hasn't scrolled"
        );
        assert!(
            term.available_scrollback > 0,
            "should have scrollback after many lines"
        );
    }

    #[test]
    fn scroll_up_increases_offset() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_up(10);
        assert_eq!(term.scroll_offset, 10);
    }

    #[test]
    fn scroll_up_clamped_to_available() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..50 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_up(usize::MAX);
        assert_eq!(term.scroll_offset, term.available_scrollback);
    }

    // ── Parser invariant: always at scrollback 0 outside render ──

    #[test]
    fn parser_at_zero_after_process_output() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        assert_eq!(term.parser.screen().scrollback(), 0);
    }

    #[test]
    fn parser_at_zero_after_scroll_up() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);
        // scroll_up is pure arithmetic — parser unchanged.
        assert_eq!(term.parser.screen().scrollback(), 0);
    }

    #[test]
    fn parser_at_zero_after_scroll_down() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);
        term.scroll_down(10);
        assert_eq!(term.parser.screen().scrollback(), 0);
    }

    #[test]
    fn render_phase_sets_and_restores_scrollback() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);

        // Render phase.
        term.prepare_for_render();
        assert_eq!(term.parser.screen().scrollback(), 50);
        term.restore_after_render();

        // Verify state is clean.
        assert_eq!(term.parser.screen().scrollback(), 0);
        assert_eq!(term.scroll_offset, 50);
    }

    // ── Regression: scroll-stuck bug ──

    #[test]
    fn regression_ctrl_g_reaches_live_screen() {
        let (mut term, tx) = test_terminal(24, 80);
        for i in 0..500 {
            tx.send(format!("line {i}\r\n").into_bytes()).unwrap();
        }
        term.process_output();

        term.scroll_up(300);
        assert!(term.scroll_offset > 0);

        // Simulate Ctrl+G (reset_scrollback).
        term.reset_scrollback();
        assert_eq!(term.scroll_offset, 0, "reset_scrollback must reach 0");

        // Simulate render phase — should show live screen.
        term.prepare_for_render();
        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "after reset + prepare, parser must show live screen"
        );
        term.restore_after_render();
        assert_eq!(term.parser.screen().scrollback(), 0);
    }

    #[test]
    fn regression_typing_reaches_live_screen() {
        let (mut term, tx) = test_terminal(24, 80);
        for i in 0..500 {
            tx.send(format!("line {i}\r\n").into_bytes()).unwrap();
        }
        term.process_output();

        term.scroll_up(300);
        assert!(term.scroll_offset > 0);

        // Simulate typing: reset_scrollback + send_bytes + process_output.
        term.reset_scrollback();
        tx.send(b"user typed something\r\n".to_vec()).unwrap();
        term.process_output();

        assert_eq!(term.scroll_offset, 0);
        assert_eq!(term.parser.screen().scrollback(), 0);

        term.prepare_for_render();
        assert_eq!(term.parser.screen().scrollback(), 0);
        term.restore_after_render();
    }

    #[test]
    fn regression_tab_switch_shows_fresh_content() {
        let (mut term, tx) = test_terminal(24, 80);
        for i in 0..200 {
            tx.send(format!("background line {i}\r\n").into_bytes())
                .unwrap();
        }
        // Simulate flush_all_pty_output (tab switch).
        term.process_output_full();

        assert_eq!(term.scroll_offset, 0);
        assert_eq!(term.parser.screen().scrollback(), 0);

        term.prepare_for_render();
        assert_eq!(term.parser.screen().scrollback(), 0);
        term.restore_after_render();
    }

    #[test]
    fn regression_scroll_down_from_deep_scrollback_reaches_zero() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..1000 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Scroll to max scrollback.
        term.scroll_up(usize::MAX);
        let deep = term.scroll_offset;
        assert!(deep > 0);

        // Repeatedly scroll down — should converge to 0.
        for _ in 0..100 {
            term.scroll_down(5);
        }

        assert_eq!(
            term.scroll_offset, 0,
            "scroll_down must converge to 0 from any depth"
        );
    }

    // ── Stress test ──

    #[test]
    fn stress_parser_always_at_zero_after_any_operation() {
        let (mut term, tx) = test_terminal(24, 80);

        // Seed with scrollback content.
        for _ in 0..500 {
            tx.send(b"initial content\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Run 1000 deterministic operations.
        for i in 0..1000_u32 {
            match i % 7 {
                0 => {
                    tx.send(format!("tick {i}\r\n").into_bytes()).unwrap();
                    term.process_output();
                }
                1 => {
                    term.scroll_up((i as usize % 50) + 1);
                }
                2 => {
                    term.scroll_down((i as usize % 50) + 1);
                }
                3 => {
                    term.reset_scrollback();
                }
                4 => {
                    let rows = 20 + (i % 20) as u16;
                    let cols = 60 + (i % 40) as u16;
                    let _ = term.resize(rows, cols);
                }
                5 => {
                    term.prepare_for_render();
                    let _ = term.parser.screen().scrollback();
                    term.restore_after_render();
                }
                6 => {
                    tx.send(format!("full {i}\r\n").into_bytes()).unwrap();
                    term.process_output_full();
                }
                _ => unreachable!(),
            }

            assert_eq!(
                term.parser.screen().scrollback(),
                0,
                "parser must be at scrollback 0 after operation {i} (op type {})",
                i % 7,
            );

            assert!(
                term.scroll_offset <= term.available_scrollback,
                "scroll_offset ({}) exceeds available_scrollback ({}) at operation {i}",
                term.scroll_offset,
                term.available_scrollback,
            );
        }
    }

    // ── Proportional scroll-down acceleration ──

    #[test]
    fn scroll_down_uses_proportional_acceleration() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..2000 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Place at a deep offset where proportional step > base step.
        // At offset 1000, effective = max(5, 1000/4) = 250.
        term.scroll_offset = 1000;
        term.scroll_down(5);

        // With proportional acceleration: 1000 - max(5, 250) = 750.
        // Without it: 1000 - 5 = 995.
        assert!(
            term.scroll_offset <= 750,
            "scroll_down should use proportional acceleration at deep offset, \
             got {} (expected <= 750)",
            term.scroll_offset,
        );
    }

    #[test]
    fn scroll_down_minimum_is_lines_when_close_to_live() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // At offset 100, effective = max(5, 100/4) = max(5, 25) = 25.
        // new_offset = 100 - 25 = 75, which is > snap_zone (24), so stays 75.
        term.scroll_offset = 100;
        term.scroll_down(5);

        assert_eq!(
            term.scroll_offset, 75,
            "near live screen: effective = max(5, 100/4) = 25, so 100-25 = 75"
        );
    }

    // ── Edge case: scroll_down when already at live screen ──

    #[test]
    fn scroll_down_from_zero_stays_at_zero() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_offset = 0;
        term.scroll_down(5);

        assert_eq!(term.scroll_offset, 0, "scroll_down from 0 must stay at 0");
        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser invariant must hold after scroll_down from 0"
        );
    }

    // ── Alternate screen exit recovery ──

    #[test]
    fn alternate_screen_exit_restores_scrollback_capacity() {
        let (mut term, tx) = test_terminal(24, 80);
        // Build scrollback on the normal screen.
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        let normal_scrollback = term.available_scrollback;
        assert!(
            normal_scrollback > 0,
            "should have scrollback on normal screen"
        );

        // Enter alternate screen — zeroes everything.
        tx.send(b"\x1b[?1049h".to_vec()).unwrap();
        term.process_output();
        assert_eq!(term.available_scrollback, 0);
        assert_eq!(term.scroll_offset, 0);

        // Exit alternate screen — normal buffer should return.
        tx.send(b"\x1b[?1049l".to_vec()).unwrap();
        term.process_output();

        assert!(
            term.available_scrollback > 0,
            "exiting alternate screen must restore normal scrollback capacity"
        );
        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser invariant must hold after alternate screen exit"
        );
    }

    // ── Multiple budget-limited calls drain eventually ──

    #[test]
    fn multiple_budgeted_calls_drain_all_output() {
        let (mut term, tx) = test_terminal(24, 80);
        // Send 400 KB — exceeds 256 KB budget.
        for _ in 0..400 {
            tx.send(vec![b'x'; 1024]).unwrap();
        }
        drop(tx);

        // First call leaves remainder.
        term.process_output();
        assert!(!term.exited, "first call should not drain all");

        // Subsequent calls should eventually drain everything.
        for _ in 0..10 {
            if term.exited {
                break;
            }
            term.process_output();
        }

        assert!(
            term.exited,
            "multiple budget-limited calls must eventually drain all output"
        );
    }

    // ── Rapid scroll interleaving ──

    #[test]
    fn rapid_scroll_up_down_maintains_invariant() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..500 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Rapidly alternate scroll up and down.
        for _ in 0..100 {
            term.scroll_up(10);
            assert_eq!(term.parser.screen().scrollback(), 0);
            assert!(term.scroll_offset <= term.available_scrollback);

            term.scroll_down(3);
            assert_eq!(term.parser.screen().scrollback(), 0);
        }

        // Interleave with output processing.
        for i in 0..50 {
            term.scroll_up(5);
            tx.send(format!("interleaved {i}\r\n").into_bytes())
                .unwrap();
            term.process_output();
            assert_eq!(term.parser.screen().scrollback(), 0);
            assert!(term.scroll_offset <= term.available_scrollback);

            term.scroll_down(8);
            assert_eq!(term.parser.screen().scrollback(), 0);
        }
    }

    // ── Available scrollback grows with content ──

    #[test]
    fn available_scrollback_grows_with_content() {
        let (mut term, tx) = test_terminal(24, 80);
        assert_eq!(term.available_scrollback, 0);

        let mut prev = 0;
        for batch in 0..5 {
            for _ in 0..50 {
                tx.send(b"line\r\n".to_vec()).unwrap();
            }
            term.process_output();

            assert!(
                term.available_scrollback >= prev,
                "available_scrollback must not decrease (batch {batch}): was {prev}, now {}",
                term.available_scrollback,
            );
            prev = term.available_scrollback;
        }

        assert!(
            prev > 0,
            "available_scrollback must be > 0 after producing content"
        );
    }

    #[test]
    fn available_scrollback_caps_at_scrollback_lines() {
        let (mut term, tx) = test_terminal(24, 80);
        // Produce more lines than SCROLLBACK_LINES.
        for _ in 0..SCROLLBACK_LINES + 1000 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output_full();

        assert!(
            term.available_scrollback <= SCROLLBACK_LINES,
            "available_scrollback ({}) must not exceed SCROLLBACK_LINES ({})",
            term.available_scrollback,
            SCROLLBACK_LINES,
        );
    }

    // ── Public API: scrollback() method ──

    #[test]
    fn scrollback_method_returns_scroll_offset() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        assert_eq!(term.scrollback(), 0, "initial scrollback should be 0");

        term.scroll_up(42);
        assert_eq!(
            term.scrollback(),
            42,
            "scrollback() must match scroll_offset after scroll_up"
        );

        term.reset_scrollback();
        assert_eq!(
            term.scrollback(),
            0,
            "scrollback() must be 0 after reset_scrollback"
        );
    }

    // ── Resize clears available_scrollback knowledge ──

    #[test]
    fn resize_while_scrolled_returns_to_live() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_up(100);
        assert_eq!(term.scroll_offset, 100);

        term.resize(30, 100).unwrap();

        assert_eq!(term.scroll_offset, 0, "resize must snap to live screen");
        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser invariant must hold after resize"
        );

        // Render should show live screen.
        term.prepare_for_render();
        assert_eq!(term.parser.screen().scrollback(), 0);
        term.restore_after_render();
    }

    // ── Multiple render cycles ──

    #[test]
    fn multiple_render_cycles_are_idempotent() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);

        // Simulate multiple render frames.
        for _ in 0..10 {
            term.prepare_for_render();
            assert_eq!(term.parser.screen().scrollback(), 50);
            term.restore_after_render();
            assert_eq!(term.parser.screen().scrollback(), 0);
            assert_eq!(term.scroll_offset, 50);
        }
    }

    // ── Render cycle with output between frames ──

    #[test]
    fn render_cycle_with_interleaved_output() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);

        for i in 0..5 {
            // Process output (simulates tick).
            tx.send(format!("frame {i}\r\n").into_bytes()).unwrap();
            term.process_output();
            assert_eq!(
                term.parser.screen().scrollback(),
                0,
                "parser at 0 after process_output (frame {i})"
            );

            // Render.
            term.prepare_for_render();
            let scrollback_during_render = term.parser.screen().scrollback();
            assert_eq!(scrollback_during_render, term.scroll_offset);
            term.restore_after_render();
            assert_eq!(term.parser.screen().scrollback(), 0);
        }
    }

    // ── Scroll up clamped when scrollback shrinks (impossible in practice
    //    since vt100 never shrinks its buffer, but guards against future changes) ──

    #[test]
    fn scroll_offset_clamped_when_available_scrollback_is_stale() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Simulate stale state: offset exceeds what's actually available.
        term.scroll_offset = term.available_scrollback + 500;

        // process_output re-queries and clamps.
        term.process_output();
        assert!(
            term.scroll_offset <= term.available_scrollback,
            "scroll_offset must be clamped after process_output"
        );
    }

    // ── Alternate screen with stale scroll state ──

    #[test]
    fn alternate_screen_with_deep_scroll_offset() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Scroll deep into history.
        term.scroll_up(term.available_scrollback);
        let deep_offset = term.scroll_offset;
        assert!(deep_offset > 0);

        // Enter alternate screen (e.g. vim opens).
        tx.send(b"\x1b[?1049h".to_vec()).unwrap();
        term.process_output();

        assert_eq!(term.scroll_offset, 0, "alternate screen must zero offset");
        assert_eq!(term.available_scrollback, 0);
        assert_eq!(term.parser.screen().scrollback(), 0);

        // Trying to scroll up on alternate screen should be a no-op.
        term.scroll_up(100);
        assert_eq!(
            term.scroll_offset, 0,
            "scroll_up on alternate screen (no scrollback) must stay 0"
        );
    }

    // ── Scroll position preserved when no output arrives ──

    #[test]
    fn scroll_offset_preserved_without_output() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_offset = 100;

        // No output — scroll position must be preserved.
        term.process_output();

        assert_eq!(
            term.scroll_offset, 100,
            "scroll position must be preserved when there is no output"
        );
    }

    // ── Scroll position preserved when output arrives ──

    #[test]
    fn scroll_offset_preserved_during_output() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_offset = 100;

        // New output arrives — scroll position must still be preserved.
        tx.send(b"new output\r\n".to_vec()).unwrap();
        term.process_output();

        assert_eq!(
            term.scroll_offset, 100,
            "scroll position must be preserved during active output"
        );
    }

    // ── Scroll convergence: scroll_down always reaches 0 ──

    #[test]
    fn scroll_down_converges_from_any_offset() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..SCROLLBACK_LINES + 100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output_full();

        // Test convergence from several starting points.
        for &start in &[1, 10, 100, 1000, 4999] {
            term.scroll_offset = start.min(term.available_scrollback);
            let mut events = 0;
            while term.scroll_offset > 0 {
                term.scroll_down(5);
                events += 1;
                assert!(
                    events < 200,
                    "scroll_down must converge to 0 from offset {start} within 200 events"
                );
            }
        }
    }

    // ── Stress: render + output + scroll interleaved ──

    #[test]
    fn stress_full_lifecycle_simulation() {
        let (mut term, tx) = test_terminal(24, 80);

        // Simulate a realistic session lifecycle:
        // 1. Initial output builds scrollback
        for _ in 0..100 {
            tx.send(b"initial output\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // 2. User scrolls up to read history
        term.scroll_up(50);
        assert_eq!(term.scroll_offset, 50);

        // 3. Render a few frames while scrolled
        for _ in 0..3 {
            term.prepare_for_render();
            assert_eq!(term.parser.screen().scrollback(), 50);
            term.restore_after_render();
        }

        // 4. New output arrives — scroll position preserved
        for tick in 0..40_u8 {
            tx.send(format!("new line {tick}\r\n").into_bytes())
                .unwrap();
            term.process_output();
            assert_eq!(term.parser.screen().scrollback(), 0);

            term.prepare_for_render();
            term.restore_after_render();
        }

        // 5. Scroll position preserved (no auto-decay)
        assert_eq!(
            term.scroll_offset, 50,
            "scroll_offset must be preserved during output (no auto-decay)"
        );

        // 6. User scrolls up further
        term.scroll_up(30);
        assert_eq!(term.scroll_offset, 80);

        // 7. Simulate resize
        term.resize(30, 100).unwrap();
        assert_eq!(term.scroll_offset, 0, "resize snaps to live");

        // 8. User scrolls up, then types (reset + send)
        term.scroll_up(25);
        term.reset_scrollback();
        tx.send(b"user typed\r\n".to_vec()).unwrap();
        term.process_output();
        assert_eq!(term.scroll_offset, 0);

        // 9. Enter alternate screen
        tx.send(b"\x1b[?1049h".to_vec()).unwrap();
        term.process_output();
        assert_eq!(term.scroll_offset, 0);
        assert_eq!(term.available_scrollback, 0);

        // 10. Exit alternate screen
        tx.send(b"\x1b[?1049l".to_vec()).unwrap();
        term.process_output();
        assert!(term.available_scrollback > 0);

        // Final invariant check.
        assert_eq!(term.parser.screen().scrollback(), 0);
        assert!(term.scroll_offset <= term.available_scrollback);
    }

    // ── Mouse protocol mode detection ──

    #[test]
    fn mouse_protocol_mode_default_is_none() {
        let (term, _tx) = test_terminal(24, 80);
        assert_eq!(term.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
    }

    #[test]
    fn mouse_protocol_mode_detects_sgr_1006() {
        let (mut term, tx) = test_terminal(24, 80);
        // Enable SGR mouse mode (1000=press/release, 1006=SGR encoding)
        tx.send(b"\x1b[?1000h\x1b[?1006h".to_vec()).unwrap();
        term.process_output();
        assert_ne!(term.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
        assert_eq!(
            term.mouse_protocol_encoding(),
            vt100::MouseProtocolEncoding::Sgr
        );
    }

    #[test]
    fn mouse_protocol_mode_resets_on_disable() {
        let (mut term, tx) = test_terminal(24, 80);
        // Enable then disable
        tx.send(b"\x1b[?1000h\x1b[?1006h".to_vec()).unwrap();
        term.process_output();
        assert_ne!(term.mouse_protocol_mode(), vt100::MouseProtocolMode::None);

        tx.send(b"\x1b[?1000l".to_vec()).unwrap();
        term.process_output();
        assert_eq!(term.mouse_protocol_mode(), vt100::MouseProtocolMode::None);
    }

    // ── should_forward_mouse decision logic ──
    //
    // This is the decision that historically caused scroll-to-bottom bugs:
    // if mouse events are forwarded when they shouldn't be, the user's
    // scroll wheel is silently consumed and Claustre's own scrollback is
    // unreachable.

    #[test]
    fn forward_mouse_false_by_default() {
        let (term, _tx) = test_terminal(24, 80);
        assert!(
            !term.should_forward_mouse(),
            "no mouse tracking enabled → should NOT forward"
        );
    }

    #[test]
    fn forward_mouse_true_when_tracking_enabled() {
        let (mut term, tx) = test_terminal(24, 80);
        tx.send(b"\x1b[?1000h\x1b[?1006h".to_vec()).unwrap();
        term.process_output();
        assert!(
            term.should_forward_mouse(),
            "mouse tracking enabled + process alive → should forward"
        );
    }

    #[test]
    fn forward_mouse_false_after_tracking_disabled() {
        let (mut term, tx) = test_terminal(24, 80);
        tx.send(b"\x1b[?1000h\x1b[?1006h".to_vec()).unwrap();
        term.process_output();
        assert!(term.should_forward_mouse());

        tx.send(b"\x1b[?1000l".to_vec()).unwrap();
        term.process_output();
        assert!(
            !term.should_forward_mouse(),
            "mouse tracking disabled → should NOT forward"
        );
    }

    #[test]
    fn forward_mouse_false_after_process_exit() {
        let (mut term, tx) = test_terminal(24, 80);
        tx.send(b"\x1b[?1000h\x1b[?1006h".to_vec()).unwrap();
        term.process_output();
        assert!(term.should_forward_mouse());

        drop(tx);
        term.process_output();
        assert!(
            !term.should_forward_mouse(),
            "process exited → must NOT forward, even if parser retains mode"
        );
    }

    #[test]
    fn forward_mouse_false_after_crash_from_alternate_screen() {
        let (mut term, tx) = test_terminal(24, 80);
        // Claude Code: alternate screen + mouse tracking
        tx.send(b"\x1b[?1049h\x1b[?1000h\x1b[?1003h\x1b[?1006h".to_vec())
            .unwrap();
        term.process_output();
        assert!(term.should_forward_mouse());

        // Simulate crash (no cleanup sequences sent)
        drop(tx);
        term.process_output();
        assert!(
            !term.should_forward_mouse(),
            "crash from alternate screen → must NOT forward"
        );
    }

    // ── Process exit cleanup ──

    #[test]
    fn process_exit_resets_mouse_tracking() {
        let (mut term, tx) = test_terminal(24, 80);
        tx.send(b"\x1b[?1000h\x1b[?1006h".to_vec()).unwrap();
        term.process_output();
        assert_ne!(term.mouse_protocol_mode(), vt100::MouseProtocolMode::None);

        drop(tx);
        term.process_output();

        assert!(term.exited);
        assert_eq!(
            term.mouse_protocol_mode(),
            vt100::MouseProtocolMode::None,
            "mouse mode must be reset after process exit"
        );
    }

    #[test]
    fn process_exit_resets_all_mouse_modes() {
        let (mut term, tx) = test_terminal(24, 80);
        // Enable all three mouse tracking modes (1000, 1002, 1003) + SGR encoding
        tx.send(b"\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h".to_vec())
            .unwrap();
        term.process_output();
        assert_ne!(term.mouse_protocol_mode(), vt100::MouseProtocolMode::None);

        drop(tx);
        term.process_output();

        assert_eq!(
            term.mouse_protocol_mode(),
            vt100::MouseProtocolMode::None,
            "all mouse tracking modes must be cleared on exit"
        );
    }

    #[test]
    fn process_exit_exits_alternate_screen() {
        let (mut term, tx) = test_terminal(24, 80);
        tx.send(b"\x1b[?1049h".to_vec()).unwrap();
        term.process_output();
        assert!(term.screen().alternate_screen());

        drop(tx);
        term.process_output();

        assert!(term.exited);
        assert!(
            !term.screen().alternate_screen(),
            "alternate screen must be exited after process exit"
        );
    }

    #[test]
    fn process_exit_restores_scrollback_from_alternate() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        let normal_scrollback = term.available_scrollback;
        assert!(normal_scrollback > 0);

        tx.send(b"\x1b[?1049h".to_vec()).unwrap();
        term.process_output();
        assert_eq!(term.available_scrollback, 0);

        drop(tx);
        term.process_output();

        assert!(term.exited);
        assert!(
            term.available_scrollback > 0,
            "normal screen scrollback must be accessible after exit from alternate"
        );
    }

    // ── Live screen reachability invariant ──
    //
    // The contract that keeps breaking: "the user can ALWAYS get back to
    // scroll_offset == 0."  These tests verify every available path.

    #[test]
    fn reachability_reset_scrollback_from_any_depth() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..500 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Scroll to maximum scrollback
        term.scroll_up(usize::MAX);
        assert!(term.scroll_offset > 0);

        term.reset_scrollback();
        assert_eq!(
            term.scroll_offset, 0,
            "reset_scrollback must ALWAYS reach 0"
        );
    }

    #[test]
    fn reachability_scroll_down_from_max_scrollback() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..2000 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_up(usize::MAX);
        let deep = term.scroll_offset;
        assert!(deep > 0);

        // Scroll down repeatedly — must converge to 0.
        for _ in 0..100 {
            term.scroll_down(5);
        }
        assert_eq!(
            term.scroll_offset, 0,
            "scroll_down must converge to 0 from max scrollback ({deep} lines)"
        );
    }

    #[test]
    fn reachability_page_scroll_down_from_max_scrollback() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..2000 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_up(usize::MAX);
        assert!(term.scroll_offset > 0);

        // Simulate half-page scroll-down (what Shift+PageDown does).
        let half_page = usize::from(term.screen().size().0) / 2;
        for _ in 0..200 {
            term.scroll_down(half_page);
        }
        assert_eq!(
            term.scroll_offset, 0,
            "half-page scroll_down must converge to 0"
        );
    }

    #[test]
    fn reachability_reset_from_max_scrollback() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..500 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_up(usize::MAX);
        assert!(term.scroll_offset > 0);

        // reset_scrollback must always bring offset to 0.
        term.reset_scrollback();
        assert_eq!(
            term.scroll_offset, 0,
            "reset_scrollback must bring scroll_offset to 0"
        );
    }

    #[test]
    fn reachability_after_process_exit_with_all_modes() {
        let (mut term, tx) = test_terminal(24, 80);
        // Build scrollback, then enter Claude-Code-like state
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Enter alternate screen + enable all mouse modes
        tx.send(b"\x1b[?1049h\x1b[?1000h\x1b[?1003h\x1b[?1006h\x1b[?2004h".to_vec())
            .unwrap();
        term.process_output();

        // Crash: exit without cleanup
        drop(tx);
        term.process_output();

        // After exit: mouse forwarding must be off, scrollback accessible
        assert!(!term.should_forward_mouse());
        assert!(!term.screen().alternate_screen());
        assert!(term.available_scrollback > 0);

        // User can scroll through history and return to live screen
        term.scroll_up(50);
        assert_eq!(term.scroll_offset, 50);
        term.reset_scrollback();
        assert_eq!(term.scroll_offset, 0);
    }

    // ── Full Claude Code session lifecycle ──
    //
    // Simulates a complete Claude Code session: startup → alternate screen
    // + mouse tracking → output → user scrolls → process exits → user
    // scrolls through history → returns to live screen.

    #[test]
    fn lifecycle_claude_code_session() {
        let (mut term, tx) = test_terminal(24, 80);

        // 1. Shell prints lines before Claude starts (enough to fill scrollback)
        for i in 0..200 {
            tx.send(format!("shell line {i}\r\n").into_bytes()).unwrap();
        }
        term.process_output();
        assert!(term.available_scrollback > 0, "shell built scrollback");
        assert!(!term.should_forward_mouse(), "no mouse tracking yet");

        // 2. Claude Code starts: enters alternate screen + mouse tracking
        tx.send(b"\x1b[?1049h\x1b[?1000h\x1b[?1006h".to_vec())
            .unwrap();
        term.process_output();
        assert!(term.screen().alternate_screen());
        assert!(term.should_forward_mouse());
        assert_eq!(
            term.available_scrollback, 0,
            "alternate screen has no scrollback"
        );
        assert_eq!(term.scroll_offset, 0, "scroll reset on alternate screen");

        // 3. Claude produces output on alternate screen
        for i in 0..50 {
            tx.send(format!("\x1b[1;1Hworking... step {i}").into_bytes())
                .unwrap();
        }
        term.process_output();
        assert_eq!(term.scroll_offset, 0, "still at live screen");
        assert!(term.should_forward_mouse(), "still forwarding");

        // 4. scroll_up does nothing on alternate screen (no scrollback)
        term.scroll_up(100);
        assert_eq!(term.scroll_offset, 0, "can't scroll up on alternate screen");

        // 5. Claude Code exits cleanly: disables mouse, exits alternate screen
        tx.send(b"\x1b[?1006l\x1b[?1000l\x1b[?1049l".to_vec())
            .unwrap();
        term.process_output();
        assert!(!term.screen().alternate_screen());
        assert!(!term.should_forward_mouse());
        assert!(
            term.available_scrollback > 0,
            "normal screen scrollback restored"
        );

        // 6. User scrolls through shell history
        term.scroll_up(10);
        assert_eq!(term.scroll_offset, 10);

        // 7. User returns to live screen
        term.reset_scrollback();
        assert_eq!(term.scroll_offset, 0);
    }

    #[test]
    fn lifecycle_claude_code_crash() {
        let (mut term, tx) = test_terminal(24, 80);

        // 1. Shell output (enough to fill scrollback)
        for _ in 0..200 {
            tx.send(b"shell\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // 2. Claude starts: alternate screen + mouse tracking
        tx.send(b"\x1b[?1049h\x1b[?1000h\x1b[?1003h\x1b[?1006h".to_vec())
            .unwrap();
        term.process_output();
        assert!(term.should_forward_mouse());

        // 3. Claude crashes (no cleanup sequences)
        drop(tx);
        term.process_output();

        // 4. Post-crash state must be fully recoverable
        assert!(term.exited);
        assert!(
            !term.should_forward_mouse(),
            "mouse must not forward after crash"
        );
        assert!(
            !term.screen().alternate_screen(),
            "alternate screen must be exited"
        );
        assert!(
            term.available_scrollback > 0,
            "normal scrollback must be accessible"
        );

        // 5. User can scroll through history and return
        term.scroll_up(20);
        assert_eq!(term.scroll_offset, 20);
        term.scroll_down(5);
        assert!(term.scroll_offset < 20);
        term.reset_scrollback();
        assert_eq!(term.scroll_offset, 0);
    }

    #[test]
    fn lifecycle_multiple_alternate_screen_transitions() {
        let (mut term, tx) = test_terminal(24, 80);

        // Build initial scrollback
        for _ in 0..50 {
            tx.send(b"initial\r\n".to_vec()).unwrap();
        }
        term.process_output();
        let initial_scrollback = term.available_scrollback;

        // Transition in and out 5 times (vim → shell → vim → ...)
        for _ in 0..5 {
            tx.send(b"\x1b[?1049h\x1b[?1000h".to_vec()).unwrap();
            term.process_output();
            assert!(term.should_forward_mouse());
            assert_eq!(term.scroll_offset, 0);

            tx.send(b"\x1b[?1000l\x1b[?1049l".to_vec()).unwrap();
            term.process_output();
            assert!(!term.should_forward_mouse());
        }

        // Scrollback preserved across transitions
        assert!(
            term.available_scrollback >= initial_scrollback,
            "scrollback must survive alternate screen transitions"
        );

        // Can still scroll and return
        term.scroll_up(10);
        term.reset_scrollback();
        assert_eq!(term.scroll_offset, 0);
    }
}
