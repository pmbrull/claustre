//! Multi-pane terminal management: `PaneInfo` and `SessionTerminals`.

use std::collections::HashMap;

use anyhow::{Context, Result};
use portable_pty::CommandBuilder;

use super::PaneId;
use super::embedded::EmbeddedTerminal;
use super::layout::{
    LayoutNode, SplitDirection, build_layout_from_config, collect_pane_ids, default_shell,
    remove_leaf, replace_leaf,
};
use super::selection::Selection;

/// Information about a single terminal pane.
pub(crate) struct PaneInfo {
    pub(crate) terminal: EmbeddedTerminal,
    pub(crate) label: String,
}

/// Terminal panes for a session, arranged in a configurable tree layout.
///
/// Replaces the former fixed shell+Claude pair with a dynamic tree of panes.
/// New shells can be added by splitting any pane (right or down).
pub struct SessionTerminals {
    panes: HashMap<PaneId, PaneInfo>,
    pub layout: LayoutNode,
    pub focused: PaneId,
    next_id: PaneId,
    /// Which pane holds the Claude terminal (for paused detection).
    pub claude_pane_id: PaneId,
    pub selection: Option<Selection>,
    /// Worktree path — needed to spawn new shell panes on split.
    pub worktree_path: String,
}

impl SessionTerminals {
    /// Create from a shell + Claude terminal pair with default side-by-side layout.
    pub fn from_parts(
        shell: EmbeddedTerminal,
        claude: EmbeddedTerminal,
        worktree_path: &str,
    ) -> Self {
        let mut panes = HashMap::new();
        panes.insert(
            0,
            PaneInfo {
                terminal: shell,
                label: "Shell".to_string(),
            },
        );
        panes.insert(
            1,
            PaneInfo {
                terminal: claude,
                label: "Claude".to_string(),
            },
        );
        Self {
            panes,
            layout: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 50,
                first: Box::new(LayoutNode::Pane(0)),
                second: Box::new(LayoutNode::Pane(1)),
            },
            focused: 1,
            next_id: 2,
            claude_pane_id: 1,
            selection: None,
            worktree_path: worktree_path.to_string(),
        }
    }

    /// Create from a layout config, spawning shell panes as needed.
    ///
    /// The `claude` terminal is placed at the "claude" leaf in the config tree.
    /// Every "shell" leaf gets a freshly-spawned shell in the worktree.
    pub fn from_layout(
        claude: EmbeddedTerminal,
        worktree_path: &str,
        layout_config: &crate::config::LayoutConfig,
        rows: u16,
        cols: u16,
    ) -> Result<Self> {
        let mut panes = HashMap::new();
        let mut next_id: PaneId = 0;
        let mut claude_opt = Some(claude);

        let layout = build_layout_from_config(
            layout_config,
            &mut panes,
            &mut next_id,
            &mut claude_opt,
            worktree_path,
            rows,
            cols,
        )?;

        let claude_pane_id = panes
            .iter()
            .find(|(_, info)| info.label == "Claude")
            .map(|(&id, _)| id)
            .context("layout config must contain exactly one 'claude' pane")?;

        Ok(Self {
            panes,
            layout,
            focused: claude_pane_id,
            next_id,
            claude_pane_id,
            selection: None,
            worktree_path: worktree_path.to_string(),
        })
    }

    /// Get a mutable reference to the focused terminal.
    ///
    /// Falls back to the first pane in layout order if the focused pane id is
    /// not in the map.  Returns `None` only if the pane map is empty (which
    /// should never happen — `close_focused` prevents closing the last pane).
    pub fn focused_terminal(&mut self) -> Option<&mut EmbeddedTerminal> {
        if !self.panes.contains_key(&self.focused)
            && let Some(&first_id) = self.pane_ids_in_order().first()
        {
            self.focused = first_id;
        }
        self.panes
            .get_mut(&self.focused)
            .map(|info| &mut info.terminal)
    }

    /// Get the terminal for a specific pane.
    pub fn terminal(&self, id: PaneId) -> Option<&EmbeddedTerminal> {
        self.panes.get(&id).map(|info| &info.terminal)
    }

    /// Get mutable terminal for a specific pane.
    pub fn terminal_mut(&mut self, id: PaneId) -> Option<&mut EmbeddedTerminal> {
        self.panes.get_mut(&id).map(|info| &mut info.terminal)
    }

    /// Get label for a pane.
    pub fn label(&self, id: PaneId) -> &str {
        self.panes.get(&id).map_or("", |info| &info.label)
    }

    /// Get the Claude pane's live screen for detection logic.
    ///
    /// Since the parser is always at scrollback 0 (render-phase-only
    /// invariant), this is a simple read — no save/restore needed.
    pub fn with_claude_live_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        let info = self.panes.get(&self.claude_pane_id)?;
        Some(f(info.terminal.screen()))
    }

    /// Cycle focus to the next pane (DFS order).
    pub fn focus_next(&mut self) {
        let ids = self.pane_ids_in_order();
        if !ids.is_empty()
            && let Some(pos) = ids.iter().position(|&id| id == self.focused)
        {
            self.focused = ids[(pos + 1) % ids.len()];
        }
    }

    /// Cycle focus to the previous pane (reverse DFS order).
    pub fn focus_prev(&mut self) {
        let ids = self.pane_ids_in_order();
        if !ids.is_empty()
            && let Some(pos) = ids.iter().position(|&id| id == self.focused)
        {
            self.focused = ids[(pos + ids.len() - 1) % ids.len()];
        }
    }

    /// Get pane IDs in layout order (DFS left-to-right, top-to-bottom).
    pub fn pane_ids_in_order(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        collect_pane_ids(&self.layout, &mut ids);
        ids
    }

    /// Split the focused pane, creating a new shell terminal beside/below it.
    pub fn split_focused(&mut self, direction: SplitDirection, rows: u16, cols: u16) -> Result<()> {
        let new_id = self.next_id;
        self.next_id += 1;

        let shell_path = std::env::var("SHELL").unwrap_or_else(|_| default_shell().into());
        let mut cmd = CommandBuilder::new(&shell_path);
        cmd.cwd(&self.worktree_path);

        // Approximate size for the new pane (corrected on next resize)
        let (new_rows, new_cols) = match direction {
            SplitDirection::Horizontal => (rows, cols / 2),
            SplitDirection::Vertical => (rows / 2, cols),
        };

        let terminal = EmbeddedTerminal::spawn(cmd, new_rows, new_cols)?;

        self.panes.insert(
            new_id,
            PaneInfo {
                terminal,
                label: "Shell".to_string(),
            },
        );

        // Replace the focused pane's leaf with a split containing both the
        // original pane and the new one.
        let replaced = replace_leaf(
            &mut self.layout,
            self.focused,
            LayoutNode::Split {
                direction,
                ratio: 50,
                first: Box::new(LayoutNode::Pane(self.focused)),
                second: Box::new(LayoutNode::Pane(new_id)),
            },
        );

        if !replaced {
            // Layout tree didn't contain the focused pane — roll back
            self.panes.remove(&new_id);
            anyhow::bail!("focused pane not found in layout tree during split");
        }

        self.focused = new_id;
        Ok(())
    }

    /// Split the focused pane, creating a new terminal running the given command.
    pub fn split_with_command(
        &mut self,
        direction: SplitDirection,
        rows: u16,
        cols: u16,
        cmd: CommandBuilder,
        label: &str,
    ) -> Result<()> {
        let new_id = self.next_id;
        self.next_id += 1;

        let (new_rows, new_cols) = match direction {
            SplitDirection::Horizontal => (rows, cols / 2),
            SplitDirection::Vertical => (rows / 2, cols),
        };

        let terminal = EmbeddedTerminal::spawn(cmd, new_rows, new_cols)?;

        self.panes.insert(
            new_id,
            PaneInfo {
                terminal,
                label: label.to_string(),
            },
        );

        let replaced = replace_leaf(
            &mut self.layout,
            self.focused,
            LayoutNode::Split {
                direction,
                ratio: 50,
                first: Box::new(LayoutNode::Pane(self.focused)),
                second: Box::new(LayoutNode::Pane(new_id)),
            },
        );

        if !replaced {
            self.panes.remove(&new_id);
            anyhow::bail!("focused pane not found in layout tree during split");
        }

        self.focused = new_id;
        Ok(())
    }

    /// Close the focused pane. Returns false if it's the last pane.
    pub fn close_focused(&mut self) -> bool {
        if self.panes.len() <= 1 {
            return false;
        }

        let closed_id = self.focused;
        self.panes.remove(&closed_id);
        remove_leaf(&mut self.layout, closed_id);

        // Focus the first remaining pane in layout order.
        // After removing one pane when len was > 1, at least one pane remains.
        let ids = self.pane_ids_in_order();
        if let Some(&first) = ids.first() {
            self.focused = first;
        }
        true
    }

    /// Drain output from all terminal panes (budget-limited per pane).
    pub fn process_output(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.process_output();
        }
    }

    /// Drain all pending output from every pane without a byte budget.
    /// Used when a session tab becomes active to flush any backlog
    /// accumulated during slower dashboard ticks.
    pub fn process_output_full(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.process_output_full();
        }
    }

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

    /// Resize individual panes to match their exact content areas.
    ///
    /// Each entry is `(pane_id, rows, cols)` representing the inner dimensions
    /// (inside borders) that the pane's PTY should match. Callers compute these
    /// using ratatui's `Layout` engine so sizes are identical to the rendered areas.
    pub fn resize_panes(&mut self, sizes: &[(PaneId, u16, u16)]) -> Result<()> {
        for &(id, rows, cols) in sizes {
            if let Some(info) = self.panes.get_mut(&id) {
                info.terminal.resize(rows, cols)?;
            }
        }
        Ok(())
    }

    /// Resize panes for a layout change (pane close), clearing the screen
    /// buffer of any pane whose width increased.
    ///
    /// Old text wrapped at the previous narrower width stays in the vt100
    /// screen buffer after `set_size` because the crate does not reflow.
    /// Clearing the buffer lets the child process — which already received
    /// `SIGWINCH` from `resize()` — redraw into a clean screen at the
    /// correct width.
    pub fn resize_panes_clearing_wider(&mut self, sizes: &[(PaneId, u16, u16)]) -> Result<()> {
        for &(id, rows, cols) in sizes {
            if let Some(info) = self.panes.get_mut(&id) {
                let old_cols = info.terminal.screen().size().1;
                info.terminal.resize(rows, cols)?;
                if cols > old_cols {
                    info.terminal.clear_screen();
                }
            }
        }
        Ok(())
    }
}
