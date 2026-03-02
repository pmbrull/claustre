# Render-Phase-Only Scrollback

**Date**: 2026-03-02
**Status**: Approved
**Problem**: Terminal session panels get stuck — user can only scroll up, never reach the live screen. Even Ctrl+G and typing fail to recover. 9 PRs have attempted to fix this; the bug keeps returning.

## Root Cause

The vt100 parser's `set_scrollback()` is called from 6 different code paths (process_output steps 1 & 5, scroll_up, scroll_down, reset_scrollback, with_claude_live_screen). Each call mutates global parser state that determines what `screen().cell()` returns. Any code path that leaves the parser in the wrong state causes the display to freeze at the wrong scroll position. The scattered mutation points create a fragile state machine that breaks under edge cases.

## Invariant

**The parser's `scrollback_offset` is ALWAYS 0, except during the render phase.**

This eliminates the entire class of bugs where the parser's scroll state gets desynchronized from the user's expected viewport.

## Architecture

### Before (6 mutation points)

```
process_output_inner: set_scrollback(0) ... set_scrollback(scroll_offset)
scroll_up:            set_scrollback(scroll_offset) → readback
scroll_down:          set_scrollback(scroll_offset) → readback
reset_scrollback:     set_scrollback(0)
resize:               set_scrollback(0)
with_claude_live_screen: set_scrollback(0) ... set_scrollback(saved)
```

### After (1 mutation point: render phase only)

```
process_output_inner:  set_scrollback(0) only (defense-in-depth)
scroll_up:             pure arithmetic
scroll_down:           pure arithmetic
reset_scrollback:      pure arithmetic
resize:                set_scrollback(0) only
with_claude_live_screen: no-op (parser already at 0)

render phase:          prepare → set_scrollback(scroll_offset) → draw → set_scrollback(0)
```

## Changes

### 1. `EmbeddedTerminal` — new field

```rust
available_scrollback: usize  // max scrollback lines for clamping
```

### 2. `process_output_inner()` — remove step 5, add scrollback query

Current step 5 sets parser to `scroll_offset` and reads back. Remove this entirely. Instead, after processing and decay:

```rust
// Query available scrollback for clamping.
self.parser.set_scrollback(usize::MAX);
self.available_scrollback = self.parser.screen().scrollback();
self.parser.set_scrollback(0);

// Clamp scroll_offset to available scrollback.
self.scroll_offset = self.scroll_offset.min(self.available_scrollback);
```

Parser ends at 0.

### 3. Scroll methods — remove all parser calls

**`scroll_up(lines)`**:
```rust
self.decay_counter = 0;
self.scroll_offset = (self.scroll_offset + lines).min(self.available_scrollback);
```

**`scroll_down(lines)`**:
```rust
let effective = lines.max(self.scroll_offset / SCROLL_DOWN_ACCEL_DIVISOR);
let new_offset = self.scroll_offset.saturating_sub(effective);
let snap_zone = usize::from(self.parser.screen().size().0);
self.scroll_offset = if new_offset <= snap_zone { 0 } else { new_offset };
```

**`reset_scrollback()`**:
```rust
self.scroll_offset = 0;
self.decay_counter = 0;
// NO parser.set_scrollback(0) — parser is already at 0
```

### 4. New render-phase methods

On `EmbeddedTerminal`:
```rust
pub fn prepare_for_render(&mut self) {
    self.parser.set_scrollback(self.scroll_offset);
}

pub fn restore_after_render(&mut self) {
    self.parser.set_scrollback(0);
}
```

On `SessionTerminals`:
```rust
pub fn prepare_for_render(&mut self) { ... }
pub fn restore_after_render(&mut self) { ... }
```

### 5. Main loop changes

```rust
loop {
    self.process_pty_output();            // parsers at 0

    self.prepare_render_scrollback();     // parsers at scroll_offset
    terminal.draw(|f| ui::draw(f, self))?;
    self.restore_live_scrollback();       // parsers at 0

    match event {
        Key => { handle_key(); process_pty_output(); }
        Tick => { process_pty_output(); detect_paused(); ... }
        ...
    }

    if tab_changed { flush_all_pty_output(); }
}
```

### 6. Simplified `with_claude_live_screen()`

Parser is always at 0, so this becomes a simple read:

```rust
pub fn with_claude_live_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
    let info = self.panes.get(&self.claude_pane_id)?;
    Some(f(info.terminal.parser.screen()))
}
```

Takes `&self` instead of `&mut self`. No save/restore needed.

## Testing Strategy

### A. Invariant tests

After every operation (process_output, scroll_up, scroll_down, reset_scrollback, resize), assert `parser.screen().scrollback() == 0`.

### B. Clamping tests

- `available_scrollback` correctly tracks buffer capacity as content grows
- `scroll_offset` never exceeds `available_scrollback`
- `scroll_up(usize::MAX)` clamps to `available_scrollback`

### C. Render cycle tests

- prepare → verify parser at scroll_offset → restore → verify parser at 0
- Output → scroll up → prepare → verify scrollback content correct → restore → verify live

### D. Regression tests (the original bugs)

- Output → deep scroll up → reset_scrollback → prepare → verify live screen
- Output → scroll up → sustained output decay → verify offset reaches 0
- Tab switch simulation → verify fresh content

### E. Property-based stress tests

Random sequences of (process_output, scroll_up, scroll_down, reset, resize) → assert parser at 0 after each. End with prepare → verify offset matches → restore → verify 0.

## Files Changed

- `src/pty/mod.rs` — Core refactor of `EmbeddedTerminal` + `SessionTerminals`
- `src/tui/app.rs` — Main loop render phase + `with_claude_live_screen` callers
- `src/tui/ui.rs` — No changes expected (rendering reads `screen()` as before)
