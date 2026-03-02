# Render-Phase-Only Scrollback Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Eliminate recurring scroll-stuck bugs by enforcing that the vt100 parser's scrollback is ALWAYS 0 except during a single, bracketed render phase.

**Architecture:** Move from 6 scattered `set_scrollback()` mutation points to exactly 1: a `prepare_for_render()` → draw → `restore_after_render()` cycle. Scroll state management becomes pure arithmetic. `available_scrollback` is tracked after each output processing pass for clamping.

**Tech Stack:** Rust, vt100 0.15, ratatui, portable-pty

---

### Task 1: Add `available_scrollback` field and update `test_terminal`

**Files:**
- Modify: `src/pty/mod.rs:92-107` (struct definition)
- Modify: `src/pty/mod.rs:160-170` (spawn constructor)
- Modify: `src/pty/mod.rs:210-217` (connect constructor)
- Modify: `src/pty/mod.rs:987-998` (test helper)

**Step 1: Add the field to `EmbeddedTerminal`**

In `src/pty/mod.rs`, add `available_scrollback: usize` after `decay_counter`:

```rust
pub struct EmbeddedTerminal {
    backend: Backend,
    output_rx: mpsc::Receiver<Vec<u8>>,
    parser: Parser,
    pub exited: bool,
    scroll_offset: usize,
    decay_counter: u8,
    /// Maximum scrollback lines currently available in the parser.
    /// Updated after each `process_output()` call. Used to clamp
    /// `scroll_offset` without calling `parser.set_scrollback()`.
    available_scrollback: usize,
}
```

**Step 2: Initialize to 0 in all constructors**

In `spawn()` (line ~160), `connect()` (line ~210), and `test_terminal()` (line ~987), add `available_scrollback: 0` to the struct literal.

**Step 3: Run tests to confirm compilation**

Run: `cargo test -p claustre --lib pty`
Expected: All existing tests pass (field is added but not used yet).

**Step 4: Commit**

```bash
git add src/pty/mod.rs
git commit -m "refactor: add available_scrollback field to EmbeddedTerminal"
```

---

### Task 2: Refactor `process_output_inner()` — remove step 5, add scrollback query

**Files:**
- Modify: `src/pty/mod.rs:264-317` (`process_output_inner`)

**Step 1: Write failing test — parser always at scrollback 0 after process_output**

Add to the test module in `src/pty/mod.rs`:

```rust
#[test]
fn parser_at_scrollback_zero_after_process_output() {
    let (mut term, tx) = test_terminal(24, 80);
    // Generate scrollback content.
    for _ in 0..200 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    // Scroll up so scroll_offset is non-zero.
    term.process_output();
    term.scroll_up(50);

    // Send more output and process it.
    tx.send(b"new output\r\n".to_vec()).unwrap();
    term.process_output();

    // INVARIANT: parser must be at scrollback 0 after processing.
    assert_eq!(
        term.parser.screen().scrollback(),
        0,
        "parser must be at scrollback 0 after process_output()"
    );
}

#[test]
fn parser_at_scrollback_zero_after_process_output_full() {
    let (mut term, tx) = test_terminal(24, 80);
    for _ in 0..200 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output_full();
    term.scroll_up(50);

    tx.send(b"new output\r\n".to_vec()).unwrap();
    term.process_output_full();

    assert_eq!(
        term.parser.screen().scrollback(),
        0,
        "parser must be at scrollback 0 after process_output_full()"
    );
}

#[test]
fn available_scrollback_tracks_buffer_capacity() {
    let (mut term, tx) = test_terminal(24, 80);
    assert_eq!(term.available_scrollback, 0, "no content = no scrollback");

    // Generate content that exceeds screen height.
    for _ in 0..100 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();

    assert!(
        term.available_scrollback > 0,
        "should have scrollback after output exceeds screen height"
    );
}

#[test]
fn scroll_offset_clamped_to_available_scrollback_after_output() {
    let (mut term, tx) = test_terminal(24, 80);
    // Generate some scrollback.
    for _ in 0..50 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();

    // Set scroll_offset absurdly high.
    term.scroll_offset = 999_999;
    term.process_output(); // no bytes, but should still clamp

    assert!(
        term.scroll_offset <= term.available_scrollback,
        "scroll_offset must be clamped to available_scrollback"
    );
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p claustre --lib pty -- parser_at_scrollback_zero`
Expected: FAIL — current code leaves parser at `scroll_offset` in step 5.

**Step 3: Refactor `process_output_inner()`**

Replace the current step 5 (lines 312-316) with:

```rust
        // 5. Query available scrollback for clamping, then ensure parser
        //    stays at live screen (scrollback 0).
        //
        //    INVARIANT: the parser's scrollback must be 0 after this method
        //    returns.  The only code that sets it to non-zero is the render
        //    phase (prepare_for_render / restore_after_render).
        self.parser.set_scrollback(usize::MAX);
        self.available_scrollback = self.parser.screen().scrollback();
        self.parser.set_scrollback(0);

        // Clamp scroll_offset to what the buffer actually holds.
        self.scroll_offset = self.scroll_offset.min(self.available_scrollback);
```

Also update the alternate screen early return (line ~296-300) to set `available_scrollback = 0`:

```rust
        if self.parser.screen().alternate_screen() {
            self.scroll_offset = 0;
            self.decay_counter = 0;
            self.available_scrollback = 0;
            self.parser.set_scrollback(0);
            return;
        }
```

**Step 4: Run all tests**

Run: `cargo test -p claustre --lib pty`
Expected: All tests pass including new invariant tests.

**Step 5: Commit**

```bash
git add src/pty/mod.rs
git commit -m "refactor: process_output leaves parser at scrollback 0, tracks available_scrollback"
```

---

### Task 3: Refactor scroll methods to pure arithmetic

**Files:**
- Modify: `src/pty/mod.rs:382-424` (scroll_up, scroll_down, reset_scrollback)

**Step 1: Write failing tests — parser at 0 after scroll operations**

Add to test module:

```rust
#[test]
fn parser_at_zero_after_scroll_up() {
    let (mut term, tx) = test_terminal(24, 80);
    for _ in 0..200 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();

    term.scroll_up(50);

    assert_eq!(
        term.parser.screen().scrollback(),
        0,
        "parser must stay at 0 after scroll_up"
    );
    assert_eq!(term.scroll_offset, 50);
}

#[test]
fn parser_at_zero_after_scroll_down() {
    let (mut term, tx) = test_terminal(24, 80);
    for _ in 0..200 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();
    term.scroll_offset = 200;

    term.scroll_down(5);

    assert_eq!(
        term.parser.screen().scrollback(),
        0,
        "parser must stay at 0 after scroll_down"
    );
}

#[test]
fn parser_at_zero_after_reset_scrollback() {
    let (mut term, tx) = test_terminal(24, 80);
    for _ in 0..200 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();
    term.scroll_offset = 100;

    term.reset_scrollback();

    assert_eq!(
        term.parser.screen().scrollback(),
        0,
        "parser must stay at 0 after reset_scrollback"
    );
    assert_eq!(term.scroll_offset, 0);
}

#[test]
fn scroll_up_clamps_to_available_scrollback() {
    let (mut term, tx) = test_terminal(24, 80);
    for _ in 0..50 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();

    let avail = term.available_scrollback;
    term.scroll_up(999_999);

    assert_eq!(
        term.scroll_offset, avail,
        "scroll_up must clamp to available_scrollback"
    );
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p claustre --lib pty -- parser_at_zero_after_scroll`
Expected: FAIL — current code calls `set_scrollback()` in scroll methods.

**Step 3: Refactor the scroll methods**

Replace `scroll_up` (line 382):

```rust
    /// Scroll up into history by `lines` rows.
    ///
    /// Pure arithmetic — does not touch the parser's scrollback state.
    /// The offset is clamped to `available_scrollback` so it never exceeds
    /// the buffer capacity.
    pub fn scroll_up(&mut self, lines: usize) {
        self.decay_counter = 0;
        self.scroll_offset = (self.scroll_offset + lines).min(self.available_scrollback);
    }
```

Replace `scroll_down` (line 403):

```rust
    /// Scroll down toward the live screen by `lines` rows.
    ///
    /// Uses **proportional acceleration**: the further back the viewport is,
    /// the larger each scroll step becomes.  This gives geometric convergence
    /// toward the live screen — ~17 wheel events from max scrollback — while
    /// preserving fine-grained control near the bottom.
    ///
    /// When the resulting offset falls within one screenful of the bottom,
    /// snaps directly to the live screen (offset 0).
    ///
    /// Does **not** reset `decay_counter` so the auto-decay mechanism
    /// actively assists the user rather than restarting its grace period on
    /// every scroll-down event.
    ///
    /// Pure arithmetic — does not touch the parser's scrollback state.
    pub fn scroll_down(&mut self, lines: usize) {
        let effective = lines.max(self.scroll_offset / SCROLL_DOWN_ACCEL_DIVISOR);
        let new_offset = self.scroll_offset.saturating_sub(effective);
        let snap_zone = usize::from(self.parser.screen().size().0);
        if new_offset <= snap_zone {
            self.scroll_offset = 0;
        } else {
            self.scroll_offset = new_offset;
        }
    }
```

Replace `reset_scrollback` (line 420):

```rust
    /// Reset scrollback to the live screen (offset = 0).
    ///
    /// Pure arithmetic — the parser is already at scrollback 0 per the
    /// render-phase-only invariant.
    pub fn reset_scrollback(&mut self) {
        self.scroll_offset = 0;
        self.decay_counter = 0;
    }
```

**Step 4: Update existing tests that assert parser state**

The existing test `scroll_up_increases_offset_and_resets_decay` (line 1148) asserts `term.scroll_offset == 10` — this still works since we clamp to `available_scrollback` which is populated by `process_output`. Same for `scroll_down_decreases_offset_without_resetting_decay` and `scroll_down_snaps_to_zero_within_snap_zone`.

The existing test `scroll_offset_clamped_to_available_scrollback` (line 1261) scrolls up with no content. Since `available_scrollback` is 0 after initialization and no output has been processed, `scroll_up(9999)` clamps to 0. This still passes.

The existing test `reset_scrollback_clears_offset_and_decay` (line 1207) still passes — it only checks `scroll_offset` and `decay_counter`.

**Step 5: Run all tests**

Run: `cargo test -p claustre --lib pty`
Expected: All tests pass.

**Step 6: Commit**

```bash
git add src/pty/mod.rs
git commit -m "refactor: scroll methods use pure arithmetic, no parser mutation"
```

---

### Task 4: Add render-phase methods to `EmbeddedTerminal` and `SessionTerminals`

**Files:**
- Modify: `src/pty/mod.rs:419-425` (after reset_scrollback, add new methods)
- Modify: `src/pty/mod.rs:770-800` (SessionTerminals, add new methods)

**Step 1: Write tests for the render phase**

Add to test module:

```rust
// ── Render phase ──

#[test]
fn prepare_for_render_sets_parser_to_scroll_offset() {
    let (mut term, tx) = test_terminal(24, 80);
    for _ in 0..200 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();
    term.scroll_up(50);

    // Parser should be at 0 before prepare.
    assert_eq!(term.parser.screen().scrollback(), 0);

    term.prepare_for_render();
    assert_eq!(
        term.parser.screen().scrollback(),
        50,
        "prepare_for_render must set parser to scroll_offset"
    );
}

#[test]
fn restore_after_render_returns_parser_to_zero() {
    let (mut term, tx) = test_terminal(24, 80);
    for _ in 0..200 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();
    term.scroll_up(50);

    term.prepare_for_render();
    assert_eq!(term.parser.screen().scrollback(), 50);

    term.restore_after_render();
    assert_eq!(
        term.parser.screen().scrollback(),
        0,
        "restore_after_render must return parser to scrollback 0"
    );
    // scroll_offset should be unchanged.
    assert_eq!(term.scroll_offset, 50);
}

#[test]
fn full_render_cycle_preserves_scroll_state() {
    let (mut term, tx) = test_terminal(24, 80);
    for _ in 0..200 {
        tx.send(b"line\r\n".to_vec()).unwrap();
    }
    term.process_output();
    term.scroll_up(50);

    // Simulate the render cycle.
    term.prepare_for_render();
    // (widget rendering would happen here, reading parser.screen())
    let _screen = term.parser.screen(); // read cells
    term.restore_after_render();

    // Verify state is clean.
    assert_eq!(term.parser.screen().scrollback(), 0);
    assert_eq!(term.scroll_offset, 50);
    assert_eq!(term.decay_counter, 0);
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test -p claustre --lib pty -- render`
Expected: FAIL — methods don't exist yet.

**Step 3: Add `prepare_for_render()` and `restore_after_render()` to `EmbeddedTerminal`**

Add after `reset_scrollback()` in `src/pty/mod.rs`:

```rust
    /// Set the parser's scrollback to the user's scroll position for rendering.
    ///
    /// This is the **only** code path that sets `scrollback > 0` on the parser.
    /// Must be paired with [`restore_after_render`] immediately after the draw
    /// call to restore the invariant (parser always at scrollback 0).
    pub fn prepare_for_render(&mut self) {
        self.parser.set_scrollback(self.scroll_offset);
    }

    /// Restore the parser to the live screen (scrollback 0) after rendering.
    ///
    /// Must be called after every [`prepare_for_render`] to maintain the
    /// invariant that the parser's scrollback is always 0 outside the render
    /// phase.
    pub fn restore_after_render(&mut self) {
        self.parser.set_scrollback(0);
    }
```

**Step 4: Add corresponding methods to `SessionTerminals`**

Add after `process_output_full()` (around line 786) in `SessionTerminals`:

```rust
    /// Prepare all panes for rendering by setting each parser to its
    /// user's scroll offset.  Must be paired with [`restore_after_render`].
    pub fn prepare_for_render(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.prepare_for_render();
        }
    }

    /// Restore all parsers to the live screen after rendering.
    pub fn restore_after_render(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.restore_after_render();
        }
    }
```

**Step 5: Run all tests**

Run: `cargo test -p claustre --lib pty`
Expected: All tests pass.

**Step 6: Commit**

```bash
git add src/pty/mod.rs
git commit -m "feat: add prepare_for_render/restore_after_render render-phase methods"
```

---

### Task 5: Wire render phase into the TUI main loop

**Files:**
- Modify: `src/tui/app.rs:1502-1520` (process_pty_output, flush_all_pty_output)
- Modify: `src/tui/app.rs:1572-1696` (main loop)

**Step 1: Add `prepare_render_scrollback()` and `restore_live_scrollback()` to `App`**

Add after `flush_all_pty_output()` (around line 1520):

```rust
    /// Set all session terminal parsers to their scroll offsets for rendering.
    /// Must be called immediately before `terminal.draw()` and paired with
    /// [`restore_live_scrollback`] immediately after.
    fn prepare_render_scrollback(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.prepare_for_render();
            }
        }
    }

    /// Restore all session terminal parsers to the live screen (scrollback 0).
    fn restore_live_scrollback(&mut self) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                terminals.restore_after_render();
            }
        }
    }
```

**Step 2: Update the main loop**

In `App::run()` (line ~1572), change the draw sequence from:

```rust
            self.process_pty_output();

            terminal.draw(|frame| {
                self.last_terminal_area = frame.area();
                ui::draw(frame, self);
            })?;
```

to:

```rust
            self.process_pty_output();

            self.prepare_render_scrollback();
            terminal.draw(|frame| {
                self.last_terminal_area = frame.area();
                ui::draw(frame, self);
            })?;
            self.restore_live_scrollback();
```

**Step 3: Run `cargo clippy` and `cargo build`**

Run: `cargo clippy`
Expected: No warnings.

**Step 4: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat: wire prepare/restore render phase into TUI main loop"
```

---

### Task 6: Simplify `with_claude_live_screen()` — no more save/restore

**Files:**
- Modify: `src/pty/mod.rs:676-683` (`with_claude_live_screen`)

**Step 1: Simplify the method**

The parser is now always at scrollback 0 (invariant). Replace:

```rust
    /// Temporarily snap the Claude pane to the live screen (scrollback 0),
    /// call `f` with the screen reference, then restore the previous offset.
    ///
    /// This lets detection logic (paused, waiting) inspect what Claude is
    /// *currently* showing regardless of where the user has scrolled.
    pub fn with_claude_live_screen<R>(&mut self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        let info = self.panes.get_mut(&self.claude_pane_id)?;
        let saved = info.terminal.scroll_offset;
        info.terminal.parser.set_scrollback(0);
        let result = f(info.terminal.parser.screen());
        info.terminal.parser.set_scrollback(saved);
        Some(result)
    }
```

with:

```rust
    /// Get the Claude pane's live screen for detection logic.
    ///
    /// Since the parser is always at scrollback 0 (render-phase-only
    /// invariant), this is a simple read — no save/restore needed.
    pub fn with_claude_live_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        let info = self.panes.get(&self.claude_pane_id)?;
        Some(f(info.terminal.screen()))
    }
```

Note: signature changes from `&mut self` to `&self`. Check if any callers need updating.

**Step 2: Check callers for `&mut self` requirement**

The caller is `detect_paused_sessions()` which iterates `&mut self.tabs`. Since `with_claude_live_screen` now takes `&self`, this should work fine (the iterator gives `&mut Tab`, but we only need `&SessionTerminals` now).

Actually, `detect_paused_sessions` borrows `self.tabs` mutably via `for tab in &mut self.tabs`. Since `with_claude_live_screen` now takes `&self`, we can change the iterator to `&self.tabs` to avoid the mutable borrow.

Check `detect_paused_sessions` in `src/tui/app.rs:1529`. If the only mutation in that loop is `with_claude_live_screen`, change `&mut self.tabs` to `&self.tabs`.

**Step 3: Run all tests and clippy**

Run: `cargo test && cargo clippy`
Expected: All pass, no warnings.

**Step 4: Commit**

```bash
git add src/pty/mod.rs src/tui/app.rs
git commit -m "refactor: simplify with_claude_live_screen — parser is always at live screen"
```

---

### Task 7: Update doc comments and struct documentation

**Files:**
- Modify: `src/pty/mod.rs` (struct doc, method docs, module doc)

**Step 1: Update the `EmbeddedTerminal` struct doc**

Replace the existing doc comment (lines 73-91) to document the new invariant:

```rust
/// An embedded terminal backed by a PTY + vt100 state machine.
///
/// ## Scrollback architecture (render-phase-only)
///
/// The vt100 parser's `scrollback_offset` is **always 0** outside of the
/// render phase.  This invariant eliminates the class of bugs where scattered
/// `set_scrollback()` calls leave the parser in an inconsistent state.
///
/// - **`scroll_offset`**: our own field tracking the user's position.
///   Modified by `scroll_up()`, `scroll_down()`, `reset_scrollback()`, and
///   decay logic in `process_output()`.  Pure arithmetic — never touches
///   the parser.
///
/// - **`available_scrollback`**: updated after each `process_output()` call
///   by querying the parser's maximum scrollback capacity.  Used to clamp
///   `scroll_offset`.
///
/// - **Render phase**: `prepare_for_render()` sets the parser to
///   `scroll_offset` for the widget to read.  `restore_after_render()`
///   returns it to 0.  These are the **only** code paths that set
///   `scrollback > 0` on the parser.
```

**Step 2: Run `cargo clippy`**

Run: `cargo clippy`
Expected: No warnings.

**Step 3: Commit**

```bash
git add src/pty/mod.rs
git commit -m "docs: update EmbeddedTerminal scrollback architecture docs"
```

---

### Task 8: Comprehensive regression and stress tests

**Files:**
- Modify: `src/pty/mod.rs` (test module)

**Step 1: Add regression tests for the original bug scenarios**

```rust
// ── Regression: scroll-stuck bug ──

#[test]
fn regression_ctrl_g_reaches_live_screen() {
    let (mut term, tx) = test_terminal(24, 80);
    // Generate deep scrollback.
    for i in 0..500 {
        tx.send(format!("line {i}\r\n").into_bytes()).unwrap();
    }
    term.process_output();

    // Scroll deep into history.
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

    // Scroll deep into history.
    term.scroll_up(300);
    assert!(term.scroll_offset > 0);

    // Simulate typing: reset_scrollback + send_bytes + process_output.
    term.reset_scrollback();
    // (send_bytes would go to PTY — we skip that in unit test)
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
    // Simulate output accumulated while on dashboard.
    for i in 0..200 {
        tx.send(format!("background line {i}\r\n").into_bytes()).unwrap();
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
```

**Step 2: Add stress test — random operation sequence**

```rust
#[test]
fn stress_parser_always_at_zero_after_any_operation() {
    let (mut term, tx) = test_terminal(24, 80);

    // Seed with scrollback content.
    for _ in 0..500 {
        tx.send(b"initial content\r\n".to_vec()).unwrap();
    }
    term.process_output();

    // Run 1000 random-ish operations.  Use a deterministic pattern
    // (not actual randomness) so the test is reproducible.
    for i in 0..1000_u32 {
        match i % 7 {
            0 => {
                // process_output with new data
                tx.send(format!("tick {i}\r\n").into_bytes()).unwrap();
                term.process_output();
            }
            1 => {
                // scroll up
                term.scroll_up((i as usize % 50) + 1);
            }
            2 => {
                // scroll down
                term.scroll_down((i as usize % 50) + 1);
            }
            3 => {
                // reset scrollback
                term.reset_scrollback();
            }
            4 => {
                // resize
                let rows = 20 + (i % 20) as u16;
                let cols = 60 + (i % 40) as u16;
                let _ = term.resize(rows, cols);
            }
            5 => {
                // prepare + restore cycle
                term.prepare_for_render();
                let _ = term.parser.screen().scrollback(); // simulate read
                term.restore_after_render();
            }
            6 => {
                // process_output_full
                tx.send(format!("full {i}\r\n").into_bytes()).unwrap();
                term.process_output_full();
            }
            _ => unreachable!(),
        }

        // INVARIANT: parser must be at scrollback 0 after every operation
        // (except during the render phase which we manually bracket above).
        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser must be at scrollback 0 after operation {i} (op type {})",
            i % 7,
        );

        // scroll_offset must never exceed available_scrollback.
        assert!(
            term.scroll_offset <= term.available_scrollback,
            "scroll_offset ({}) exceeds available_scrollback ({}) at operation {i}",
            term.scroll_offset,
            term.available_scrollback,
        );
    }
}
```

**Step 3: Run all tests**

Run: `cargo test -p claustre --lib pty`
Expected: All pass.

**Step 4: Commit**

```bash
git add src/pty/mod.rs
git commit -m "test: comprehensive regression and stress tests for render-phase scrollback"
```

---

### Task 9: Run full CI checks

**Step 1: Run the complete CI suite locally**

Run: `cargo fmt --check && cargo clippy && cargo test`
Expected: All pass with zero warnings.

**Step 2: Fix any issues found**

If clippy or tests flag issues, fix them.

**Step 3: Final commit (if needed)**

```bash
git add -A
git commit -m "fix: address CI feedback for render-phase scrollback"
```
