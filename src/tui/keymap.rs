use crossterm::event::{KeyCode, KeyModifiers};

// ── Actions ──────────────────────────────────────────────────────────

/// Every discrete action the TUI can perform in response to a key press.
///
/// Actions are context-free identifiers; the *execution* code in `App`
/// decides what actually happens based on the current focus / state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // Global
    Quit,
    OpenCommandPalette,
    NextTab,
    PrevTab,
    ShowHelp,

    // Navigation
    FocusProjects,
    FocusTasks,
    MoveUp,
    MoveDown,

    // Task actions
    Select,
    LaunchTask,
    KillSession,
    MarkDone,
    NewTask,
    EditTask,
    DeleteItem,
    OpenPR,
    OpenSubtasks,
    OpenSkills,
    AddProject,
    FilterTasks,
    ReorderTaskDown,
    ReorderTaskUp,
    // Session-only
    ReturnToDashboard,
    FocusPrevPane,
    FocusNextPane,
    ScrollToBottom,
    SplitRight,
    SplitDown,
    ClosePane,
}

// ── Help categories ──────────────────────────────────────────────────

/// Logical groupings shown in the help overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HelpCategory {
    Navigation,
    Projects,
    Tasks,
    SkillsPanel,
    SessionTab,
}

impl HelpCategory {
    fn label(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Projects => "Projects",
            Self::Tasks => "Tasks",
            Self::SkillsPanel => "Skills Panel (i)",
            Self::SessionTab => "Session Tab",
        }
    }

    /// Fixed display order for the help overlay.
    const ORDERED: &[Self] = &[
        Self::Navigation,
        Self::Projects,
        Self::Tasks,
        Self::SkillsPanel,
        Self::SessionTab,
    ];
}

// ── Keybinding ───────────────────────────────────────────────────────

/// A single key → action mapping with metadata for the help overlay.
#[derive(Debug, Clone)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub action: Action,
    /// Human-readable key label shown in help (e.g. `"Ctrl+P"`).
    pub label: &'static str,
    /// Short description shown next to the label in the help overlay.
    pub description: &'static str,
    pub category: HelpCategory,
}

// ── Entry for help display ───────────────────────────────────────────

/// A single row in the help overlay.
#[derive(Debug, Clone)]
pub struct HelpEntry {
    pub label: &'static str,
    pub description: &'static str,
}

// ── KeyMap ────────────────────────────────────────────────────────────

/// Declarative registry of every key binding in the TUI.
///
/// Two separate tables: `normal` (dashboard normal-mode keys) and
/// `session` (keys intercepted before forwarding to the PTY).
pub struct KeyMap {
    pub normal: Vec<KeyBinding>,
    pub session: Vec<KeyBinding>,
}

impl KeyMap {
    /// Build the default key map encoding all current bindings.
    pub fn default_keymap() -> Self {
        Self {
            normal: default_normal_bindings(),
            session: default_session_bindings(),
        }
    }

    /// Look up a normal-mode action for the given key event.
    pub fn lookup_normal(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        lookup(&self.normal, code, modifiers)
    }

    /// Look up a session-mode action for the given key event.
    pub fn lookup_session(&self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        lookup(&self.session, code, modifiers)
    }

    /// Generate grouped help entries in display order.
    ///
    /// The skills-panel entries are hardcoded separately (they live inside
    /// their own modal and aren't part of the normal/session tables).
    pub fn help_entries(&self) -> Vec<(&'static str, Vec<HelpEntry>)> {
        let mut out = Vec::new();

        for &cat in HelpCategory::ORDERED {
            let mut entries: Vec<HelpEntry> = Vec::new();

            // Collect from normal bindings.
            for kb in &self.normal {
                if kb.category == cat
                    && !kb.description.is_empty()
                    && !entries.iter().any(|e| e.label == kb.label)
                {
                    entries.push(HelpEntry {
                        label: kb.label,
                        description: kb.description,
                    });
                }
            }

            // Collect from session bindings.
            for kb in &self.session {
                if kb.category == cat
                    && !kb.description.is_empty()
                    && !entries.iter().any(|e| e.label == kb.label)
                {
                    entries.push(HelpEntry {
                        label: kb.label,
                        description: kb.description,
                    });
                }
            }

            // Skills-panel entries are hardcoded (they're modal-internal).
            if cat == HelpCategory::SkillsPanel {
                entries.extend([
                    HelpEntry {
                        label: "  j/k",
                        description: "Navigate skills",
                    },
                    HelpEntry {
                        label: "  f",
                        description: "Find skills (remote search)",
                    },
                    HelpEntry {
                        label: "  a",
                        description: "Add skill by package name",
                    },
                    HelpEntry {
                        label: "  x",
                        description: "Remove selected skill",
                    },
                    HelpEntry {
                        label: "  u",
                        description: "Update all skills",
                    },
                    HelpEntry {
                        label: "  g",
                        description: "Toggle global / project scope",
                    },
                ]);
            }

            if !entries.is_empty() {
                out.push((cat.label(), entries));
            }
        }

        out
    }
}

// ── Lookup helper ────────────────────────────────────────────────────

fn lookup(bindings: &[KeyBinding], code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    bindings
        .iter()
        .find(|kb| kb.code == code && kb.modifiers == modifiers)
        .map(|kb| kb.action)
}

// ── Default normal-mode bindings ─────────────────────────────────────

#[allow(clippy::enum_glob_use)]
fn default_normal_bindings() -> Vec<KeyBinding> {
    use Action::*;
    use HelpCategory::*;

    vec![
        // ── Navigation ───────────────────────────────────────────
        KeyBinding {
            code: KeyCode::Char('p'),
            modifiers: KeyModifiers::CONTROL,
            action: OpenCommandPalette,
            label: "  Ctrl+P",
            description: "Command palette",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('k'),
            modifiers: KeyModifiers::CONTROL,
            action: PrevTab,
            label: "  Ctrl+J/K",
            description: "Switch tab",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::CONTROL,
            action: NextTab,
            label: "",
            description: "",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('h'),
            modifiers: KeyModifiers::NONE,
            action: FocusProjects,
            label: "  h/l",
            description: "Focus projects / tasks",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('1'),
            modifiers: KeyModifiers::NONE,
            action: FocusProjects,
            label: "",
            description: "",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Left,
            modifiers: KeyModifiers::NONE,
            action: FocusProjects,
            label: "",
            description: "",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('2'),
            modifiers: KeyModifiers::NONE,
            action: FocusTasks,
            label: "",
            description: "",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Right,
            modifiers: KeyModifiers::NONE,
            action: FocusTasks,
            label: "",
            description: "",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            action: MoveDown,
            label: "  j/k",
            description: "Navigate up/down",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            action: MoveDown,
            label: "  arrows",
            description: "Navigate (all directions)",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            action: MoveUp,
            label: "",
            description: "",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('?'),
            modifiers: KeyModifiers::NONE,
            action: ShowHelp,
            label: "  ?",
            description: "This help screen",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            action: Quit,
            label: "  q",
            description: "Quit",
            category: Navigation,
        },
        KeyBinding {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CONTROL,
            action: Quit,
            label: "",
            description: "",
            category: Navigation,
        },
        // ── Projects ─────────────────────────────────────────────
        KeyBinding {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            action: Select,
            label: "  Enter",
            description: "Select project",
            category: Projects,
        },
        KeyBinding {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            action: AddProject,
            label: "  a",
            description: "Add project",
            category: Projects,
        },
        KeyBinding {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers::NONE,
            action: DeleteItem,
            label: "  d",
            description: "Delete project",
            category: Projects,
        },
        // ── Tasks ────────────────────────────────────────────────
        // Note: Enter, d are context-dependent (Projects vs Tasks),
        // but help shows them under both categories.
        KeyBinding {
            code: KeyCode::Char('n'),
            modifiers: KeyModifiers::NONE,
            action: NewTask,
            label: "  n",
            description: "New task",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('e'),
            modifiers: KeyModifiers::NONE,
            action: EditTask,
            label: "  e",
            description: "Edit task (pending only)",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('s'),
            modifiers: KeyModifiers::NONE,
            action: OpenSubtasks,
            label: "  s",
            description: "Subtasks panel",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('l'),
            modifiers: KeyModifiers::NONE,
            action: LaunchTask,
            label: "  l",
            description: "Launch task",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('k'),
            modifiers: KeyModifiers::NONE,
            action: KillSession,
            label: "  k",
            description: "Kill session (stuck tasks)",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('r'),
            modifiers: KeyModifiers::NONE,
            action: MarkDone,
            label: "  r",
            description: "Mark done",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('o'),
            modifiers: KeyModifiers::NONE,
            action: OpenPR,
            label: "  o",
            description: "Open PR in browser",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('/'),
            modifiers: KeyModifiers::NONE,
            action: FilterTasks,
            label: "  /",
            description: "Filter tasks",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('J'),
            modifiers: KeyModifiers::NONE,
            action: ReorderTaskDown,
            label: "  J/K",
            description: "Reorder tasks",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('K'),
            modifiers: KeyModifiers::NONE,
            action: ReorderTaskUp,
            label: "",
            description: "",
            category: Tasks,
        },
        KeyBinding {
            code: KeyCode::Char('i'),
            modifiers: KeyModifiers::NONE,
            action: OpenSkills,
            label: "",
            description: "",
            category: Navigation,
        },
    ]
}

// ── Default session-mode bindings ────────────────────────────────────

#[allow(clippy::enum_glob_use)]
fn default_session_bindings() -> Vec<KeyBinding> {
    use Action::*;
    use HelpCategory::*;

    vec![
        KeyBinding {
            code: KeyCode::Char('d'),
            modifiers: KeyModifiers::CONTROL,
            action: ReturnToDashboard,
            label: "  Ctrl+D",
            description: "Return to dashboard",
            category: SessionTab,
        },
        KeyBinding {
            code: KeyCode::Char('h'),
            modifiers: KeyModifiers::CONTROL,
            action: FocusPrevPane,
            label: "  Ctrl+H/L",
            description: "Switch pane focus",
            category: SessionTab,
        },
        KeyBinding {
            code: KeyCode::Char('l'),
            modifiers: KeyModifiers::CONTROL,
            action: FocusNextPane,
            label: "",
            description: "",
            category: SessionTab,
        },
        KeyBinding {
            code: KeyCode::Char('k'),
            modifiers: KeyModifiers::CONTROL,
            action: PrevTab,
            label: "  Ctrl+J/K",
            description: "Switch tab",
            category: SessionTab,
        },
        KeyBinding {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::CONTROL,
            action: NextTab,
            label: "",
            description: "",
            category: SessionTab,
        },
        KeyBinding {
            code: KeyCode::Char('g'),
            modifiers: KeyModifiers::CONTROL,
            action: ScrollToBottom,
            label: "  Ctrl+G",
            description: "Scroll to bottom (live screen)",
            category: SessionTab,
        },
        KeyBinding {
            code: KeyCode::Char('b'),
            modifiers: KeyModifiers::CONTROL,
            action: SplitRight,
            label: "  Ctrl+B",
            description: "Split right",
            category: SessionTab,
        },
        KeyBinding {
            code: KeyCode::Char('n'),
            modifiers: KeyModifiers::CONTROL,
            action: SplitDown,
            label: "  Ctrl+N",
            description: "Split down",
            category: SessionTab,
        },
        KeyBinding {
            code: KeyCode::Char('w'),
            modifiers: KeyModifiers::CONTROL,
            action: ClosePane,
            label: "  Ctrl+W",
            description: "Close pane",
            category: SessionTab,
        },
    ]
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_normal_quit() {
        let km = KeyMap::default_keymap();
        assert_eq!(
            km.lookup_normal(KeyCode::Char('q'), KeyModifiers::NONE),
            Some(Action::Quit)
        );
    }

    #[test]
    fn lookup_normal_ctrl_c_quit() {
        let km = KeyMap::default_keymap();
        assert_eq!(
            km.lookup_normal(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Some(Action::Quit)
        );
    }

    #[test]
    fn lookup_session_return_to_dashboard() {
        let km = KeyMap::default_keymap();
        assert_eq!(
            km.lookup_session(KeyCode::Char('d'), KeyModifiers::CONTROL),
            Some(Action::ReturnToDashboard)
        );
    }

    #[test]
    fn lookup_session_unknown_key() {
        let km = KeyMap::default_keymap();
        assert_eq!(
            km.lookup_session(KeyCode::Char('x'), KeyModifiers::NONE),
            None
        );
    }

    #[test]
    fn help_entries_cover_all_categories() {
        let km = KeyMap::default_keymap();
        let entries = km.help_entries();
        let labels: Vec<&str> = entries.iter().map(|(l, _)| *l).collect();
        assert!(labels.contains(&"Navigation"));
        assert!(labels.contains(&"Projects"));
        assert!(labels.contains(&"Tasks"));
        assert!(labels.contains(&"Skills Panel (i)"));
        assert!(labels.contains(&"Session Tab"));
    }

    #[test]
    fn help_entries_no_duplicates() {
        let km = KeyMap::default_keymap();
        for (_, entries) in km.help_entries() {
            let mut seen = std::collections::HashSet::new();
            for e in &entries {
                assert!(seen.insert(e.label), "duplicate help label: {:?}", e.label);
            }
        }
    }

    #[test]
    fn normal_bindings_has_expected_count() {
        let km = KeyMap::default_keymap();
        // We have a substantial number of normal bindings
        assert!(km.normal.len() >= 20);
    }

    #[test]
    fn session_bindings_has_expected_count() {
        let km = KeyMap::default_keymap();
        assert_eq!(km.session.len(), 9);
    }
}
