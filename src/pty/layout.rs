//! Split direction, layout tree, and layout helper functions.

use std::collections::HashMap;

use anyhow::{Context, Result};
use portable_pty::CommandBuilder;

use super::PaneId;
use super::embedded::EmbeddedTerminal;
use super::session_terminals::PaneInfo;

// ── Split direction & layout tree ──

/// Direction of a split between two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// Side by side: first | second
    Horizontal,
    /// Stacked: first on top, second on bottom
    Vertical,
}

/// A node in the pane layout tree.
#[derive(Debug, Clone)]
pub enum LayoutNode {
    /// A leaf node containing a single terminal pane.
    Pane(PaneId),
    /// A split node dividing space between two children.
    Split {
        direction: SplitDirection,
        /// Percentage of space allocated to the first child (1–99).
        ratio: u16,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

// ── Layout tree helpers ──

/// Return the first available shell, trying `/bin/zsh`, `/bin/bash`, then `/bin/sh`.
pub(crate) fn default_shell() -> &'static str {
    for shell in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if std::path::Path::new(shell).exists() {
            return shell;
        }
    }
    "/bin/sh"
}

/// Collect pane IDs in DFS left-to-right order.
pub(crate) fn collect_pane_ids(node: &LayoutNode, ids: &mut Vec<PaneId>) {
    match node {
        LayoutNode::Pane(id) => ids.push(*id),
        LayoutNode::Split { first, second, .. } => {
            collect_pane_ids(first, ids);
            collect_pane_ids(second, ids);
        }
    }
}

/// Replace the leaf node matching `target` with `replacement`.
pub(crate) fn replace_leaf(node: &mut LayoutNode, target: PaneId, replacement: LayoutNode) -> bool {
    match node {
        LayoutNode::Pane(id) if *id == target => {
            *node = replacement;
            true
        }
        LayoutNode::Split { first, second, .. } => {
            if replace_leaf(first, target, replacement.clone()) {
                true
            } else {
                replace_leaf(second, target, replacement)
            }
        }
        LayoutNode::Pane(_) => false,
    }
}

/// Remove a leaf from the tree by replacing its parent split with the sibling.
pub(crate) fn remove_leaf(node: &mut LayoutNode, target: PaneId) -> bool {
    match node {
        LayoutNode::Pane(_) => false,
        LayoutNode::Split { first, second, .. } => {
            // If a direct child is the target, replace this split with the sibling
            if matches!(first.as_ref(), LayoutNode::Pane(id) if *id == target) {
                *node = *second.clone();
                return true;
            }
            if matches!(second.as_ref(), LayoutNode::Pane(id) if *id == target) {
                *node = *first.clone();
                return true;
            }
            // Recurse into children
            remove_leaf(first, target) || remove_leaf(second, target)
        }
    }
}

/// Build a `LayoutNode` tree from a config, spawning shell terminals as needed.
pub(crate) fn build_layout_from_config(
    config: &crate::config::LayoutConfig,
    panes: &mut HashMap<PaneId, PaneInfo>,
    next_id: &mut PaneId,
    claude: &mut Option<EmbeddedTerminal>,
    worktree_path: &str,
    rows: u16,
    cols: u16,
) -> Result<LayoutNode> {
    match config {
        crate::config::LayoutConfig::Pane { pane } => {
            let id = *next_id;
            *next_id += 1;

            let (terminal, label) = if pane == "claude" {
                let t = claude
                    .take()
                    .context("layout config has multiple 'claude' panes")?;
                (t, "Claude".to_string())
            } else {
                let shell_path = std::env::var("SHELL").unwrap_or_else(|_| default_shell().into());
                let mut cmd = CommandBuilder::new(&shell_path);
                cmd.cwd(worktree_path);
                let t = EmbeddedTerminal::spawn(cmd, rows, cols)?;
                (t, "Shell".to_string())
            };

            panes.insert(id, PaneInfo { terminal, label });
            Ok(LayoutNode::Pane(id))
        }
        crate::config::LayoutConfig::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let dir = match direction.as_str() {
                "vertical" => SplitDirection::Vertical,
                _ => SplitDirection::Horizontal,
            };
            let r = ratio.unwrap_or(50).clamp(1, 99);

            let (first_rows, first_cols, second_rows, second_cols) = match dir {
                SplitDirection::Horizontal => {
                    let fc = (u32::from(cols) * u32::from(r) / 100) as u16;
                    (rows, fc, rows, cols.saturating_sub(fc))
                }
                SplitDirection::Vertical => {
                    let fr = (u32::from(rows) * u32::from(r) / 100) as u16;
                    (fr, cols, rows.saturating_sub(fr), cols)
                }
            };

            let first_node = build_layout_from_config(
                first,
                panes,
                next_id,
                claude,
                worktree_path,
                first_rows,
                first_cols,
            )?;
            let second_node = build_layout_from_config(
                second,
                panes,
                next_id,
                claude,
                worktree_path,
                second_rows,
                second_cols,
            )?;

            Ok(LayoutNode::Split {
                direction: dir,
                ratio: r,
                first: Box::new(first_node),
                second: Box::new(second_node),
            })
        }
    }
}
