use anyhow::Result;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::pty::SplitDirection;

use super::super::form::apply_text_edit;
use super::super::ui;
use super::{
    App, DeleteTarget, Focus, InputMode, PaletteAction, ProjectSummary, Tab, ToastStyle,
    compute_pane_sizes_for_resize, fallback_title,
};

impl App {
    /// Dispatch a key event to the correct dashboard handler based on `input_mode`.
    pub(super) fn handle_dashboard_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match self.input_mode {
            InputMode::Normal => self.handle_normal_key(code, modifiers)?,
            InputMode::NewTask => self.handle_input_key(code, modifiers)?,
            InputMode::EditTask => self.handle_edit_task_key(code, modifiers)?,
            InputMode::NewProject => self.handle_new_project_key(code, modifiers)?,
            InputMode::ConfirmDelete => self.handle_confirm_delete_key(code)?,
            InputMode::CommandPalette => self.handle_palette_key(code, modifiers)?,
            InputMode::SkillPanel => self.handle_skill_panel_key(code)?,
            InputMode::SkillSearch => self.handle_skill_search_key(code, modifiers)?,
            InputMode::SkillAdd => self.handle_skill_add_key(code, modifiers)?,
            InputMode::HelpOverlay => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('?' | 'q')) {
                    self.input_mode = InputMode::Normal;
                }
            }
            InputMode::TaskDetails => match code {
                KeyCode::Esc | KeyCode::Char('v' | 'q') => {
                    self.input_mode = InputMode::Normal;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.task_details_scroll = self.task_details_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.task_details_scroll = self.task_details_scroll.saturating_sub(1);
                }
                _ => {}
            },
            InputMode::TaskFilter => self.handle_task_filter_key(code, modifiers)?,
            InputMode::SubtaskPanel => self.handle_subtask_panel_key(code, modifiers)?,
        }
        Ok(())
    }

    /// Handle keys when a session tab is active.
    /// Intercept registered session keys; forward everything else to the PTY.
    pub(super) fn handle_session_tab_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        if let Some(action) = self.keymap.lookup_session(code, modifiers) {
            self.execute_session_action(action)?;
            return Ok(());
        }

        // Forward to focused PTY, clear selection, and snap back to live screen
        if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
            terminals.selection = None;
            if let Some(term) = terminals.focused_terminal() {
                term.reset_scrollback();
                let key_bytes = keycode_to_bytes(code, modifiers);
                if key_bytes.len > 0 {
                    let _ = term.send_bytes(key_bytes.as_bytes());
                }
            }
        }
        Ok(())
    }

    /// Execute a session-mode action (dashboard return, pane focus, splits, close).
    fn execute_session_action(&mut self, action: super::super::keymap::Action) -> Result<()> {
        use super::super::keymap::Action;
        match action {
            Action::ReturnToDashboard => {
                self.active_tab = 0;
            }
            Action::FocusPrevPane => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
                    terminals.focus_prev();
                }
            }
            Action::FocusNextPane => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
                    terminals.focus_next();
                }
            }
            Action::ScrollToBottom => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                    && let Some(term) = terminals.focused_terminal()
                {
                    term.reset_scrollback();
                }
            }
            Action::ScrollPageUp => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                    && let Some(term) = terminals.focused_terminal()
                {
                    let rows = usize::from(term.screen().size().0);
                    let half = rows / 2;
                    term.scroll_up(half);
                }
            }
            Action::ScrollPageDown => {
                if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                    && let Some(term) = terminals.focused_terminal()
                {
                    let rows = usize::from(term.screen().size().0);
                    let half = rows / 2;
                    term.scroll_down(half);
                }
            }
            Action::PrevTab => self.prev_tab(),
            Action::NextTab => self.next_tab(),
            Action::SplitRight => {
                let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                let rows = term_size.1.saturating_sub(2);
                let cols = term_size.0;
                let split_err = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let err = terminals
                        .split_focused(SplitDirection::Horizontal, rows, cols)
                        .err();
                    let sizes =
                        compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
                    let _ = terminals.resize_panes_with_clear(&sizes);
                    err
                } else {
                    None
                };
                if let Some(e) = split_err {
                    self.show_toast(format!("Split failed: {e}"), ToastStyle::Error);
                }
            }
            Action::SplitDown => {
                let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                let rows = term_size.1.saturating_sub(2);
                let cols = term_size.0;
                let split_err = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let err = terminals
                        .split_focused(SplitDirection::Vertical, rows, cols)
                        .err();
                    let sizes =
                        compute_pane_sizes_for_resize(&terminals.layout, term_size.0, term_size.1);
                    let _ = terminals.resize_panes_with_clear(&sizes);
                    err
                } else {
                    None
                };
                if let Some(e) = split_err {
                    self.show_toast(format!("Split failed: {e}"), ToastStyle::Error);
                }
            }
            Action::ClosePane => {
                let close_result = if let Some(Tab::Session { terminals, .. }) =
                    self.tabs.get_mut(self.active_tab)
                {
                    let closed = terminals.close_focused();
                    if closed {
                        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                        let sizes = compute_pane_sizes_for_resize(
                            &terminals.layout,
                            term_size.0,
                            term_size.1,
                        );
                        // Use clearing variant: panes that changed width after
                        // the closed pane's space was reclaimed need their
                        // screen buffer cleared so old text wrapped at the
                        // previous width doesn't persist.
                        let _ = terminals.resize_panes_with_clear(&sizes);
                    }
                    Some(closed)
                } else {
                    None
                };
                if close_result == Some(false) {
                    self.show_toast("Cannot close this pane", ToastStyle::Info);
                }
            }
            // Normal-mode-only actions are no-ops in session mode
            _ => {}
        }
        Ok(())
    }

    /// Forward pasted text to the focused PTY on a session tab.
    pub(super) fn handle_session_tab_paste(&mut self, text: &str) -> Result<()> {
        if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab) {
            terminals.selection = None;
            if let Some(term) = terminals.focused_terminal() {
                term.reset_scrollback();
                // Send as bracketed paste so the embedded shell/editor handles it correctly
                let bracketed = format!("\x1b[200~{text}\x1b[201~");
                let _ = term.send_bytes(bracketed.as_bytes());
            }
        }
        Ok(())
    }

    /// Handle pasted text on the dashboard by inserting at cursor in the active input buffer.
    pub(super) fn handle_dashboard_paste(&mut self, text: &str) -> Result<()> {
        match self.input_mode {
            InputMode::NewTask | InputMode::EditTask
                if self.new_task_field == 0 || self.new_task_field == 2 =>
            {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
            }
            InputMode::NewProject => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                if self.new_project_field == 1 {
                    self.update_path_suggestions();
                }
            }
            InputMode::CommandPalette => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                self.filter_palette();
                self.palette_index = 0;
            }
            InputMode::SkillSearch => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
                self.search_results.clear();
                self.skill_status_message.clear();
            }
            InputMode::SkillAdd | InputMode::SubtaskPanel => {
                self.input_buffer
                    .insert_str(self.input_cursor.min(self.input_buffer.len()), text);
                self.input_cursor = (self.input_cursor + text.len()).min(self.input_buffer.len());
            }
            InputMode::TaskFilter => {
                self.task_filter
                    .insert_str(self.task_filter_cursor.min(self.task_filter.len()), text);
                self.task_filter_cursor =
                    (self.task_filter_cursor + text.len()).min(self.task_filter.len());
                self.recompute_visible_tasks();
                self.task_index = 0;
            }
            // Normal, ConfirmDelete, SkillPanel, HelpOverlay: no text input
            _ => {}
        }
        Ok(())
    }

    /// Handle terminal resize events — resize all PTYs to match new dimensions.
    ///
    /// Uses ratatui's layout engine to compute exact inner areas for each pane,
    /// ensuring PTY sizes always match the rendered areas.  After resizing, a
    /// single `process_pty_output()` pass runs so the parser state (scroll
    /// offset, screen content) is synced before the next frame is drawn.
    pub(super) fn handle_resize(&mut self, cols: u16, rows: u16) {
        for tab in &mut self.tabs {
            if let Tab::Session { terminals, .. } = tab {
                let sizes = compute_pane_sizes_for_resize(&terminals.layout, cols, rows);
                let _ = terminals.resize_panes_with_clear(&sizes);
            }
        }
        // Process any pending PTY output immediately so the next draw uses
        // up-to-date parser state (scroll offset, screen content).
        self.process_pty_output();
    }

    /// Compute inner areas (content inside borders) for all panes in the current session tab.
    /// Returns a list of `(PaneId, inner_rect)` in absolute screen coordinates.
    fn session_pane_inner_areas(&self) -> Vec<(crate::pty::PaneId, Rect)> {
        use super::collect_pane_inner_areas;

        let size = self.last_terminal_area;
        let has_tab_bar = self.tabs.len() > 1;
        let tab_bar_height = u16::from(has_tab_bar);

        let term_area = Rect {
            x: 0,
            y: tab_bar_height,
            width: size.width,
            height: size.height.saturating_sub(tab_bar_height + 1),
        };

        if let Some(Tab::Session { terminals, .. }) = self.tabs.get(self.active_tab) {
            collect_pane_inner_areas(&terminals.layout, term_area)
        } else {
            vec![]
        }
    }

    /// Translate absolute screen coordinates to vt100 terminal coordinates for a pane.
    /// Returns `(PaneId, vt100_row, vt100_col)` or `None` if outside all panes.
    fn screen_to_terminal_coords(
        &self,
        screen_col: u16,
        screen_row: u16,
    ) -> Option<(crate::pty::PaneId, u16, u16)> {
        for (id, inner) in &self.session_pane_inner_areas() {
            if screen_col >= inner.x
                && screen_col < inner.x + inner.width
                && screen_row >= inner.y
                && screen_row < inner.y + inner.height
            {
                return Some((*id, screen_row - inner.y, screen_col - inner.x));
            }
        }
        None
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let col = mouse.column;
        let row = mouse.row;
        let size = self.last_terminal_area;

        if size.width == 0 || size.height == 0 {
            return Ok(());
        }

        // --- Session tab mouse handling ---
        //
        // When the focused PTY application has enabled mouse tracking (e.g.
        // Claude Code running in the alternate screen), mouse events are
        // encoded as escape sequences and forwarded to the PTY so the app
        // can handle its own scrolling and interactive elements.
        //
        // When mouse tracking is disabled (e.g. a plain shell), Claustre
        // handles events itself for scrollback and text selection.
        if self.active_tab > 0 {
            // Tab bar click: always handled by Claustre regardless of mouse mode
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let has_tab_bar = self.tabs.len() > 1;
                if has_tab_bar && row == 0 {
                    let layout = ui::compute_tab_layout(&self.tabs, self.active_tab, size.width);
                    for entry in &layout.entries {
                        if col >= entry.x_start && col < entry.x_start + entry.width {
                            self.active_tab = entry.tab_index;
                            return Ok(());
                        }
                    }
                    return Ok(());
                }
            }

            // Determine target pane and check mouse protocol.
            // `should_forward_mouse()` returns false when the process has
            // exited — preventing scroll events from being silently consumed
            // by a dead process while the parser retains stale mouse mode.
            let coords = self.screen_to_terminal_coords(col, row);
            let mouse_forwarded = if let Some((pane_id, vt_row, vt_col)) = coords
                && let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                && let Some(term) = terminals.terminal(pane_id)
                && term.should_forward_mouse()
            {
                let encoding = term.mouse_protocol_encoding();
                if let Some(bytes) = encode_mouse_event(&mouse.kind, vt_col, vt_row, encoding) {
                    // Focus the clicked pane on button press
                    if matches!(mouse.kind, MouseEventKind::Down(_)) {
                        terminals.focused = pane_id;
                    }
                    terminals.selection = None;
                    if let Some(term) = terminals.terminal_mut(pane_id) {
                        let _ = term.send_bytes(&bytes);
                    }
                }
                true
            } else {
                false
            };

            if mouse_forwarded {
                return Ok(());
            }

            // Mouse tracking disabled — use Claustre's own handling
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    // Click inside a terminal pane: start selection
                    if let Some((pane, vt_row, vt_col)) = coords {
                        if let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        {
                            terminals.focused = pane;
                            terminals.selection = Some(crate::pty::Selection {
                                pane,
                                start: (vt_row, vt_col),
                                end: (vt_row, vt_col),
                            });
                        }
                    } else {
                        // Click outside panes: clear selection
                        if let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        {
                            terminals.selection = None;
                        }
                    }
                    return Ok(());
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    // Compute pane areas before mutable borrow
                    let pane_areas = self.session_pane_inner_areas();
                    if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                        && let Some(ref mut sel) = terminals.selection
                        && let Some((_, inner)) = pane_areas.iter().find(|(id, _)| *id == sel.pane)
                    {
                        let vt_row = row
                            .saturating_sub(inner.y)
                            .min(inner.height.saturating_sub(1));
                        let vt_col = col
                            .saturating_sub(inner.x)
                            .min(inner.width.saturating_sub(1));
                        sel.end = (vt_row, vt_col);
                    }
                    return Ok(());
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    // Copy selected text to clipboard only if user actually
                    // dragged (start != end).  A plain click (down + up on the
                    // same cell) should behave like a normal terminal: reposition
                    // focus without copying anything.
                    if let Some(Tab::Session { terminals, .. }) = self.tabs.get_mut(self.active_tab)
                    {
                        // Copy selection (Selection is Copy) to release the
                        // immutable borrow on `terminals`, allowing mutable
                        // access to the terminal for scrollback adjustment.
                        let should_clear = if let Some(sel) = terminals.selection {
                            if sel.start == sel.end {
                                // Plain click — no drag occurred
                                true
                            } else if let Some(term) = terminals.terminal_mut(sel.pane) {
                                // Set the parser to the user's scroll offset so
                                // screen.cell() reads from the scrolled viewport
                                // (not the live screen).  Without this, copying
                                // while scrolled back extracts text from the
                                // bottom of the output instead of the visible
                                // region.
                                term.prepare_for_render();
                                let text = sel.extract_text(term.screen());
                                term.restore_after_render();
                                if !text.is_empty()
                                    && let Ok(mut clipboard) = arboard::Clipboard::new()
                                {
                                    let _ = clipboard.set_text(&text);
                                }
                                false
                            } else {
                                true
                            }
                        } else {
                            false
                        };
                        if should_clear {
                            terminals.selection = None;
                        }
                    }
                    return Ok(());
                }
                MouseEventKind::ScrollUp => {
                    if let Some((pane_id, _, _)) = coords
                        && let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        && let Some(term) = terminals.terminal_mut(pane_id)
                    {
                        term.scroll_up(5);
                    }
                    return Ok(());
                }
                MouseEventKind::ScrollDown => {
                    if let Some((pane_id, _, _)) = coords
                        && let Some(Tab::Session { terminals, .. }) =
                            self.tabs.get_mut(self.active_tab)
                        && let Some(term) = terminals.terminal_mut(pane_id)
                    {
                        term.scroll_down(5);
                    }
                    return Ok(());
                }
                _ => return Ok(()),
            }
        }

        // --- Dashboard events ---
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {}
            MouseEventKind::ScrollUp => {
                if self.input_mode == InputMode::Normal {
                    self.move_up();
                }
                return Ok(());
            }
            MouseEventKind::ScrollDown => {
                if self.input_mode == InputMode::Normal {
                    self.move_down();
                }
                return Ok(());
            }
            _ => return Ok(()),
        }

        let has_tab_bar = self.tabs.len() > 1;
        let tab_bar_height = u16::from(has_tab_bar);

        // --- Tab bar click (top row, only when visible) ---
        if has_tab_bar && row == 0 {
            // Use the same layout computation as draw_tab_bar
            let layout = ui::compute_tab_layout(&self.tabs, self.active_tab, size.width);
            for entry in &layout.entries {
                if col >= entry.x_start && col < entry.x_start + entry.width {
                    self.active_tab = entry.tab_index;
                    return Ok(());
                }
            }
            return Ok(());
        }

        // --- Dashboard: only handle clicks in Normal mode ---
        if self.input_mode != InputMode::Normal {
            return Ok(());
        }

        // Recompute dashboard layout to determine which panel was clicked.
        // Layout mirrors draw_active_impl(): title(1) + main(min) + bottom(2)
        let content_top = tab_bar_height + 1; // +1 for title bar
        let content_bottom = size.height.saturating_sub(2); // -2 for status+hints
        if row < content_top || row >= content_bottom {
            return Ok(());
        }

        let content_height = content_bottom - content_top;

        // Main area: left 30% | right 70%
        let left_width = size.width * 30 / 100;

        if col < left_width {
            // Click in the left column (Projects panel area)
            // Left column: top 60% = Projects, bottom 40% = Stats
            let projects_height = content_height * 60 / 100;
            if row < content_top + projects_height {
                self.focus = Focus::Projects;
                // Try to select the clicked project item.
                // Projects panel has a 1-row border, so inner starts at content_top + 1.
                let inner_top = content_top + 1;
                if row >= inner_top {
                    let clicked_row = (row - inner_top) as usize;
                    // Each project may take multiple lines (name + session statuses).
                    // Walk through projects to find which one this row falls in.
                    let empty_summary = ProjectSummary::default();
                    let mut current_row: usize = 0;
                    for (i, project) in self.projects.iter().enumerate() {
                        let summary = self
                            .project_summaries
                            .get(&project.id)
                            .unwrap_or(&empty_summary);
                        let item_height = 1 + summary.active_sessions.len();
                        if clicked_row >= current_row && clicked_row < current_row + item_height {
                            if i != self.project_index {
                                self.project_index = i;
                                let _ = self.refresh_data();
                                self.task_index = 0;
                            }
                            break;
                        }
                        current_row += item_height;
                    }
                }
            }
            // Stats panel click — no action needed
        } else {
            // Click in the right column
            // Right column: top 60% = Tasks, bottom 40% = Session Detail + Usage
            let tasks_height = content_height * 60 / 100;
            if row < content_top + tasks_height {
                self.focus = Focus::Tasks;
                // Try to select the clicked task item.
                // Tasks panel has a 1-row border, so inner starts at content_top + 1.
                let inner_top = content_top + 1;
                if row >= inner_top {
                    let clicked_row = (row - inner_top) as usize;
                    let visible_count = self.visible_tasks().len();
                    // Account for scroll offset — clicked_row is relative to the viewport
                    let absolute_index = clicked_row + self.task_list_state.offset();
                    if absolute_index < visible_count {
                        self.task_index = absolute_index;
                    }
                }
            }
            // Session Detail / Usage clicks — no action needed
        }

        Ok(())
    }

    pub(super) fn handle_normal_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        if let Some(action) = self.keymap.lookup_normal(code, modifiers) {
            self.execute_action(action)?;
        }
        Ok(())
    }

    /// Execute a normal-mode action. Context-dependent actions (e.g. `k` = kill
    /// or move-up, `l` = focus or launch) are resolved here based on current state.
    fn execute_action(&mut self, action: super::super::keymap::Action) -> Result<()> {
        use super::super::keymap::Action;
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::OpenCommandPalette => {
                self.input_mode = InputMode::CommandPalette;
                self.input_buffer.clear();
                self.palette_index = 0;
                self.filter_palette();
            }
            Action::PrevTab => self.prev_tab(),
            Action::NextTab => self.next_tab(),
            Action::FocusProjects => {
                self.focus = Focus::Projects;
            }
            Action::FocusTasks => self.focus = Focus::Tasks,
            Action::ShowHelp => {
                self.input_mode = InputMode::HelpOverlay;
            }
            Action::FilterTasks => {
                self.task_filter.clear();
                self.recompute_visible_tasks();
                self.input_mode = InputMode::TaskFilter;
                self.focus = Focus::Tasks;
            }
            Action::MoveDown => self.move_down(),
            Action::MoveUp => self.move_up(),
            Action::ReorderTaskDown => {
                if self.focus == Focus::Tasks {
                    let visible = self.visible_tasks();
                    if let (Some(current), Some(next)) = (
                        visible.get(self.task_index),
                        visible.get(self.task_index + 1),
                    ) {
                        let current_id = current.id.clone();
                        let next_id = next.id.clone();
                        if self.store.swap_task_order(&current_id, &next_id).is_ok() {
                            self.task_index += 1;
                            let _ = self.refresh_data();
                        }
                    }
                }
            }
            Action::ReorderTaskUp => {
                if self.focus == Focus::Tasks && self.task_index > 0 {
                    let visible = self.visible_tasks();
                    if let (Some(current), Some(prev)) = (
                        visible.get(self.task_index),
                        visible.get(self.task_index - 1),
                    ) {
                        let current_id = current.id.clone();
                        let prev_id = prev.id.clone();
                        if self.store.swap_task_order(&current_id, &prev_id).is_ok() {
                            self.task_index -= 1;
                            let _ = self.refresh_data();
                        }
                    }
                }
            }
            Action::Select => match self.focus {
                Focus::Projects => {
                    self.refresh_data()?;
                    self.task_index = 0;
                }
                Focus::Tasks => {
                    if let Some(task) = self.visible_tasks().get(self.task_index) {
                        if let Some(session_id) = &task.session_id {
                            let session = self.store.get_session(session_id)?;
                            if session.closed_at.is_none() {
                                if !self.goto_session_tab(&session.id) {
                                    self.restore_session_tab(&session)?;
                                }
                            } else {
                                self.show_toast("Session is closed", ToastStyle::Info);
                            }
                        } else if matches!(
                            task.status,
                            crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                        ) {
                            if self.session_op_in_progress {
                                self.show_toast(
                                    "Session operation in progress...",
                                    ToastStyle::Info,
                                );
                            } else if let Some(project_id) =
                                self.selected_project().map(|p| p.id.clone())
                            {
                                let task_id = task.id.clone();
                                self.launch_task(task_id, project_id)?;
                            }
                        }
                    }
                }
            },
            Action::ViewTaskDetails => {
                if self.focus == Focus::Tasks && !self.visible_tasks().is_empty() {
                    self.task_details_scroll = 0;
                    self.input_mode = InputMode::TaskDetails;
                }
            }
            Action::OpenSubtasks => {
                if self.focus == Focus::Tasks && !self.visible_tasks().is_empty() {
                    if let Some(task) = self.visible_tasks().get(self.task_index) {
                        self.subtasks = self
                            .store
                            .list_subtasks_for_task(&task.id)
                            .unwrap_or_default();
                    }
                    self.subtask_index = 0;
                    self.input_buffer.clear();
                    self.input_mode = InputMode::SubtaskPanel;
                }
            }
            Action::NewTask => {
                if self.selected_project().is_some() {
                    self.reset_task_form();
                    self.input_mode = InputMode::NewTask;
                }
            }
            Action::EditTask => {
                if self.focus == Focus::Tasks {
                    let task_data = self.visible_tasks().get(self.task_index).map(|t| {
                        (
                            t.id.clone(),
                            t.title.clone(),
                            t.description.clone(),
                            t.mode,
                            t.status,
                            t.base.clone(),
                            t.branch.clone(),
                            t.push_mode,
                            t.review_loop,
                        )
                    });
                    if let Some((
                        id,
                        _title,
                        desc,
                        mode,
                        status,
                        base,
                        branch,
                        push_mode,
                        review_loop,
                    )) = task_data
                        && matches!(
                            status,
                            crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                        )
                    {
                        self.editing_task_id = Some(id);
                        self.new_task_description.clone_from(&desc);
                        self.new_task_mode = mode;
                        self.new_task_base = base.unwrap_or_default();
                        self.new_task_branch = branch.unwrap_or_default();
                        self.new_task_push_mode = push_mode;
                        self.new_task_review_loop = review_loop;
                        self.new_task_field = 0;
                        self.input_buffer.clone_from(&desc);
                        self.input_cursor = self.input_buffer.len();
                        self.input_mode = InputMode::EditTask;
                    }
                }
            }
            Action::MarkDone => {
                if self.focus == Focus::Tasks
                    && let Some(task) = self.visible_tasks().get(self.task_index).copied()
                    && matches!(
                        task.status,
                        crate::store::TaskStatus::InReview
                            | crate::store::TaskStatus::Working
                            | crate::store::TaskStatus::Interrupted
                            | crate::store::TaskStatus::CiFailed
                    )
                {
                    self.store
                        .update_task_status(&task.id, crate::store::TaskStatus::Done)?;
                    if let Some(ref sid) = task.session_id {
                        self.spawn_teardown_session(sid.clone());
                    }
                    self.refresh_data()?;
                    self.show_toast("Task marked as done", ToastStyle::Success);
                }
            }
            // `k` = kill session when a running task is focused, otherwise vim-style move up
            Action::KillSession => {
                let mut killed = false;
                if self.focus == Focus::Tasks {
                    if self.session_op_in_progress {
                        self.show_toast("Session operation in progress...", ToastStyle::Info);
                        killed = true;
                    } else if let Some(task) = self.visible_tasks().get(self.task_index).copied()
                        && let Some(ref sid) = task.session_id
                        && matches!(
                            task.status,
                            crate::store::TaskStatus::Working
                                | crate::store::TaskStatus::InReview
                                | crate::store::TaskStatus::CiFailed
                                | crate::store::TaskStatus::Error
                        )
                    {
                        let sid = sid.clone();
                        self.store
                            .update_task_status(&task.id, crate::store::TaskStatus::Pending)?;
                        self.store.unassign_task_from_session(&task.id)?;
                        self.spawn_teardown_session(sid);
                        self.refresh_data()?;
                        self.show_toast("Session killed — press Enter to resume", ToastStyle::Info);
                        killed = true;
                    }
                }
                if !killed {
                    self.move_up();
                }
            }
            Action::OpenPR => {
                if self.focus == Focus::Tasks
                    && let Some(task) = self.visible_tasks().get(self.task_index).copied()
                    && let Some(ref url) = task.pr_url
                {
                    let opener = if cfg!(target_os = "macos") {
                        "open"
                    } else {
                        "xdg-open"
                    };
                    let _ = std::process::Command::new(opener).arg(url).spawn();
                    self.show_toast("Opening PR in browser", ToastStyle::Success);
                }
            }
            // `l` = focus tasks when on projects, launch task when on tasks.
            // If the task already has a session (stuck/working), tear it down first
            // and relaunch in a fresh session.
            Action::LaunchTask => {
                if self.focus == Focus::Projects {
                    self.focus = Focus::Tasks;
                } else if self.session_op_in_progress {
                    self.show_toast("Session operation in progress...", ToastStyle::Info);
                } else if let Some(task) = self.visible_tasks().get(self.task_index).copied()
                    && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                {
                    if matches!(
                        task.status,
                        crate::store::TaskStatus::Pending | crate::store::TaskStatus::Draft
                    ) {
                        // Normal launch for pending/draft tasks
                        self.launch_task(task.id.clone(), project_id)?;
                    } else if let Some(ref sid) = task.session_id
                        && matches!(
                            task.status,
                            crate::store::TaskStatus::Working
                                | crate::store::TaskStatus::InReview
                                | crate::store::TaskStatus::CiFailed
                                | crate::store::TaskStatus::Error
                        )
                    {
                        // Relaunch: tear down old session, then auto-launch fresh
                        let sid = sid.clone();
                        self.store
                            .update_task_status(&task.id, crate::store::TaskStatus::Pending)?;
                        self.store.unassign_task_from_session(&task.id)?;
                        self.pending_relaunch = Some((task.id.clone(), project_id));
                        self.spawn_teardown_session(sid);
                        self.refresh_data()?;
                        self.show_toast("Relaunching task in new session...", ToastStyle::Info);
                    }
                }
            }
            Action::DeleteItem => match self.focus {
                Focus::Projects => {
                    if let Some((name, id)) = self
                        .selected_project()
                        .map(|p| (p.name.clone(), p.id.clone()))
                    {
                        self.confirm_target = name;
                        self.confirm_entity_id = id;
                        self.confirm_delete_kind = DeleteTarget::Project;
                        self.input_mode = InputMode::ConfirmDelete;
                    }
                }
                Focus::Tasks => {
                    let task_data = self
                        .visible_tasks()
                        .get(self.task_index)
                        .map(|t| (t.id.clone(), t.title.clone()));
                    if let Some((id, title)) = task_data {
                        self.confirm_target = title;
                        self.confirm_entity_id = id;
                        self.confirm_delete_kind = DeleteTarget::Task;
                        self.input_mode = InputMode::ConfirmDelete;
                    }
                }
            },
            Action::OpenSkills => {
                self.refresh_skills();
                self.skill_index = 0;
                self.input_mode = InputMode::SkillPanel;
            }
            Action::AddProject => {
                self.input_mode = InputMode::NewProject;
                self.input_buffer.clear();
                self.new_project_name.clear();
                self.new_project_path = String::from(".");
                self.new_project_field = 0;
                self.clear_path_autocomplete();
            }
            // Session-only actions are no-ops in normal mode
            Action::ReturnToDashboard
            | Action::FocusPrevPane
            | Action::FocusNextPane
            | Action::ScrollToBottom
            | Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::SplitRight
            | Action::SplitDown
            | Action::ClosePane => {}
        }
        Ok(())
    }

    /// Handle keys shared between new-task and edit-task forms (tab, back-tab, mode toggle, typing).
    /// Returns `true` if the key was consumed.
    fn handle_task_form_shared_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let field_count: u8 = 7;
        match code {
            // On subtask field with subtasks: Tab cycles through them
            KeyCode::Tab if self.new_task_field == 6 && !self.new_task_subtasks.is_empty() => {
                // If editing, save the current edit first
                if let Some(idx) = self.editing_subtask_index {
                    let trimmed = self.input_buffer.trim().to_string();
                    if !trimmed.is_empty() {
                        self.new_task_subtasks[idx] = trimmed;
                    }
                    self.editing_subtask_index = None;
                    self.input_buffer.clear();
                }
                self.new_task_subtask_index =
                    (self.new_task_subtask_index + 1) % self.new_task_subtasks.len();
                true
            }
            KeyCode::Tab => {
                self.editing_subtask_index = None;
                self.save_current_task_field();
                self.new_task_field = (self.new_task_field + 1) % field_count;
                self.load_current_task_field();
                true
            }
            KeyCode::BackTab => {
                // Cancel any editing state when leaving field 4
                self.editing_subtask_index = None;
                self.save_current_task_field();
                self.new_task_field = if self.new_task_field == 0 {
                    field_count - 1
                } else {
                    self.new_task_field - 1
                };
                self.load_current_task_field();
                true
            }
            KeyCode::Left | KeyCode::Right if self.new_task_field == 1 && modifiers.is_empty() => {
                use crate::store::TaskMode::{Autonomous, Exploration, Supervised};
                // Right: Autonomous -> Supervised -> Exploration -> ...
                // Left:  Autonomous -> Exploration -> Supervised -> ...
                self.new_task_mode = match (code, self.new_task_mode) {
                    (KeyCode::Right, Autonomous) | (KeyCode::Left, Exploration) => Supervised,
                    (KeyCode::Right, Supervised) | (KeyCode::Left, Autonomous) => Exploration,
                    (KeyCode::Right, Exploration) | (KeyCode::Left, Supervised) => Autonomous,
                    _ => unreachable!(),
                };
                true
            }
            KeyCode::Left | KeyCode::Right if self.new_task_field == 4 && modifiers.is_empty() => {
                self.new_task_push_mode = match self.new_task_push_mode {
                    crate::store::PushMode::Pr => crate::store::PushMode::Push,
                    crate::store::PushMode::Push => crate::store::PushMode::Pr,
                };
                true
            }
            KeyCode::Left | KeyCode::Right if self.new_task_field == 5 && modifiers.is_empty() => {
                self.new_task_review_loop = !self.new_task_review_loop;
                true
            }
            // Subtask input field: typing, add, delete, navigate
            _ if self.new_task_field == 6 => self.handle_subtask_input_key(code, modifiers),
            // Base field: text input
            _ if self.new_task_field == 2 => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
            // Branch field: text input
            _ if self.new_task_field == 3 => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
            _ if self.new_task_field == 0 => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
            _ => false,
        }
    }

    /// Handle keys when the subtask input field (field 2) is focused in the task form.
    fn handle_subtask_input_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        match code {
            // Esc while editing a subtask: cancel edit
            KeyCode::Esc if self.editing_subtask_index.is_some() => {
                self.editing_subtask_index = None;
                self.input_buffer.clear();
                true
            }
            // Enter while editing: save edited subtask (trim, reject empty)
            KeyCode::Enter if self.editing_subtask_index.is_some() => {
                let trimmed = self.input_buffer.trim().to_string();
                if let Some(idx) = self.editing_subtask_index
                    && !trimmed.is_empty()
                {
                    self.new_task_subtasks[idx] = trimmed;
                }
                self.editing_subtask_index = None;
                self.input_buffer.clear();
                true
            }
            // Enter with text, not editing: add new subtask (trim, reject empty)
            KeyCode::Enter if !self.input_buffer.is_empty() => {
                let trimmed = self.input_buffer.trim().to_string();
                self.input_buffer.clear();
                if !trimmed.is_empty() {
                    self.new_task_subtasks.push(trimmed);
                }
                true
            }
            // Enter with empty input: start editing selected subtask
            KeyCode::Enter
                if self.input_buffer.is_empty()
                    && !self.new_task_subtasks.is_empty()
                    && self.editing_subtask_index.is_none() =>
            {
                let idx = self.new_task_subtask_index;
                self.editing_subtask_index = Some(idx);
                self.input_buffer.clone_from(&self.new_task_subtasks[idx]);
                self.input_cursor = self.input_buffer.len();
                true
            }
            // 'd' with empty input and not editing: delete selected subtask
            KeyCode::Char('d')
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                if !self.new_task_subtasks.is_empty() {
                    self.new_task_subtasks.remove(self.new_task_subtask_index);
                    if self.new_task_subtasks.is_empty() {
                        self.new_task_subtask_index = 0;
                    } else if self.new_task_subtask_index >= self.new_task_subtasks.len() {
                        self.new_task_subtask_index = self.new_task_subtasks.len() - 1;
                    }
                }
                true
            }
            // j/k navigation only when not editing
            KeyCode::Char('j') | KeyCode::Down
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                if !self.new_task_subtasks.is_empty() {
                    self.new_task_subtask_index = (self.new_task_subtask_index + 1)
                        .min(self.new_task_subtasks.len().saturating_sub(1));
                }
                true
            }
            KeyCode::Char('k') | KeyCode::Up
                if self.input_buffer.is_empty() && self.editing_subtask_index.is_none() =>
            {
                self.new_task_subtask_index = self.new_task_subtask_index.saturating_sub(1);
                true
            }
            _ => apply_text_edit(
                &mut self.input_buffer,
                &mut self.input_cursor,
                code,
                modifiers,
            ),
        }
    }

    pub(super) fn handle_input_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        if self.handle_task_form_shared_key(code, modifiers) {
            return Ok(());
        }
        match code {
            KeyCode::Enter => {
                self.save_current_task_field();
                let is_exploration = self.new_task_mode == crate::store::TaskMode::Exploration;
                if !self.new_task_description.is_empty() || is_exploration {
                    if let Some(project_id) = self.selected_project().map(|p| p.id.clone()) {
                        let fallback = if is_exploration && self.new_task_description.is_empty() {
                            "Exploration session".to_string()
                        } else {
                            fallback_title(&self.new_task_description)
                        };
                        let branch = if self.new_task_branch.is_empty() {
                            None
                        } else {
                            Some(self.new_task_branch.as_str())
                        };
                        let base = if self.new_task_base.is_empty() {
                            None
                        } else {
                            Some(self.new_task_base.as_str())
                        };
                        let task = self.store.create_task(
                            &project_id,
                            &fallback,
                            &self.new_task_description,
                            self.new_task_mode,
                            branch,
                            base,
                            self.new_task_push_mode,
                            self.new_task_review_loop,
                        )?;

                        // Create inline subtasks
                        for subtask_desc in &self.new_task_subtasks {
                            let st_title = fallback_title(subtask_desc);
                            self.store
                                .create_subtask(&task.id, &st_title, subtask_desc)?;
                        }

                        // Launch autonomous and exploration tasks immediately,
                        // or just generate the title for supervised tasks.
                        if matches!(
                            self.new_task_mode,
                            crate::store::TaskMode::Autonomous
                                | crate::store::TaskMode::Exploration
                        ) {
                            self.launch_task(task.id, project_id)?;
                        } else {
                            let desc = self.new_task_description.clone();
                            self.spawn_title_generation(task.id, desc);
                        }
                    }
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc => {
                self.save_current_task_field();
                let is_exploration = self.new_task_mode == crate::store::TaskMode::Exploration;
                if (!self.new_task_description.is_empty() || is_exploration)
                    && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                {
                    let fallback = if is_exploration && self.new_task_description.is_empty() {
                        "Exploration session".to_string()
                    } else {
                        fallback_title(&self.new_task_description)
                    };
                    let branch = if self.new_task_branch.is_empty() {
                        None
                    } else {
                        Some(self.new_task_branch.as_str())
                    };
                    let base = if self.new_task_base.is_empty() {
                        None
                    } else {
                        Some(self.new_task_base.as_str())
                    };
                    let task = self.store.create_task(
                        &project_id,
                        &fallback,
                        &self.new_task_description,
                        self.new_task_mode,
                        branch,
                        base,
                        self.new_task_push_mode,
                        self.new_task_review_loop,
                    )?;
                    self.store
                        .update_task_status(&task.id, crate::store::TaskStatus::Draft)?;

                    // Create inline subtasks
                    for subtask_desc in &self.new_task_subtasks {
                        let st_title = fallback_title(subtask_desc);
                        self.store
                            .create_subtask(&task.id, &st_title, subtask_desc)?;
                    }

                    self.show_toast("Task saved as draft", ToastStyle::Info);
                    self.refresh_data()?;
                }
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn save_current_task_field(&mut self) {
        match self.new_task_field {
            0 => self.new_task_description.clone_from(&self.input_buffer),
            2 => self.new_task_base.clone_from(&self.input_buffer),
            3 => self.new_task_branch.clone_from(&self.input_buffer),
            _ => {}
        }
    }

    fn load_current_task_field(&mut self) {
        match self.new_task_field {
            0 => {
                self.input_buffer.clone_from(&self.new_task_description);
                self.input_cursor = self.input_buffer.len();
            }
            2 => {
                self.input_buffer.clone_from(&self.new_task_base);
                self.input_cursor = self.input_buffer.len();
            }
            3 => {
                self.input_buffer.clone_from(&self.new_task_branch);
                self.input_cursor = self.input_buffer.len();
            }
            _ => {
                self.input_buffer.clear();
                self.input_cursor = 0;
            }
        }
    }

    pub(super) fn handle_new_project_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        // Path field (field 1) with autocomplete support
        if self.new_project_field == 1 {
            match code {
                KeyCode::Enter => {
                    self.save_current_project_field();
                    self.clear_path_autocomplete();
                    self.submit_new_project()?;
                }
                KeyCode::Tab if self.show_path_suggestions => {
                    self.accept_path_suggestion();
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.save_current_project_field();
                    self.clear_path_autocomplete();
                    self.new_project_field = 1 - self.new_project_field;
                    self.load_current_project_field();
                }
                KeyCode::Down if self.show_path_suggestions => {
                    if !self.path_suggestions.is_empty() {
                        self.path_suggestion_index = (self.path_suggestion_index + 1)
                            .min(self.path_suggestions.len().saturating_sub(1));
                    }
                }
                KeyCode::Up if self.show_path_suggestions => {
                    self.path_suggestion_index = self.path_suggestion_index.saturating_sub(1);
                }
                KeyCode::Esc if self.show_path_suggestions => {
                    self.clear_path_autocomplete();
                }
                KeyCode::Esc => {
                    self.input_buffer.clear();
                    self.new_project_name.clear();
                    self.new_project_path.clear();
                    self.new_project_field = 0;
                    self.clear_path_autocomplete();
                    self.input_mode = InputMode::Normal;
                }
                // Path field uses shared text editing with autocomplete refresh
                _ => {
                    let old_len = self.input_buffer.len();
                    let is_char = matches!(code, KeyCode::Char(_));
                    apply_text_edit(
                        &mut self.input_buffer,
                        &mut self.input_cursor,
                        code,
                        modifiers,
                    );
                    let new_len = self.input_buffer.len();

                    if new_len < old_len {
                        // Something was deleted — refresh autocomplete
                        self.refresh_path_autocomplete_after_delete();
                    } else if is_char && new_len > old_len {
                        // A character was inserted — check if it triggers autocomplete
                        let last_inserted =
                            self.input_buffer[..self.input_cursor].chars().next_back();
                        if last_inserted == Some('/')
                            || (last_inserted == Some('~') && self.input_buffer == "~")
                            || self.show_path_suggestions
                        {
                            self.update_path_suggestions();
                        }
                    }
                }
            }
        } else {
            // Name field (field 0) — use shared text editing
            match code {
                KeyCode::Enter => {
                    self.save_current_project_field();
                    self.submit_new_project()?;
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.save_current_project_field();
                    self.new_project_field = 1 - self.new_project_field;
                    self.load_current_project_field();
                }
                KeyCode::Esc => {
                    self.input_buffer.clear();
                    self.new_project_name.clear();
                    self.new_project_path.clear();
                    self.new_project_field = 0;
                    self.clear_path_autocomplete();
                    self.input_mode = InputMode::Normal;
                }
                _ => {
                    apply_text_edit(
                        &mut self.input_buffer,
                        &mut self.input_cursor,
                        code,
                        modifiers,
                    );
                }
            }
        }
        Ok(())
    }

    fn save_current_project_field(&mut self) {
        match self.new_project_field {
            0 => self.new_project_name.clone_from(&self.input_buffer),
            _ => self.new_project_path.clone_from(&self.input_buffer),
        }
    }

    fn submit_new_project(&mut self) -> Result<()> {
        if !self.new_project_name.is_empty() && !self.new_project_path.is_empty() {
            let name = self.new_project_name.clone();
            let path_to_resolve =
                Self::expand_tilde(&self.new_project_path).unwrap_or(self.new_project_path.clone());
            let Ok(abs_path) = std::fs::canonicalize(&path_to_resolve) else {
                self.show_toast(
                    format!("Invalid path: {path_to_resolve}"),
                    ToastStyle::Error,
                );
                return Ok(());
            };
            let Some(abs_str) = abs_path.to_str() else {
                self.show_toast("Path contains invalid UTF-8".to_string(), ToastStyle::Error);
                return Ok(());
            };
            let default_branch = crate::detect_default_branch(abs_str);
            self.store
                .create_project(&self.new_project_name, abs_str, &default_branch)?;
            self.new_project_name.clear();
            self.new_project_path.clear();
            self.new_project_field = 0;
            self.input_buffer.clear();
            self.clear_path_autocomplete();
            self.input_mode = InputMode::Normal;
            self.refresh_data()?;
            self.show_toast(format!("Project '{name}' created"), ToastStyle::Success);
        }
        Ok(())
    }

    fn load_current_project_field(&mut self) {
        match self.new_project_field {
            0 => self.input_buffer.clone_from(&self.new_project_name),
            _ => self.input_buffer.clone_from(&self.new_project_path),
        }
        self.input_cursor = self.input_buffer.len();
    }

    /// Expand `~` prefix to home directory in the given path string.
    fn expand_tilde(raw: &str) -> Option<String> {
        if let Some(rest) = raw.strip_prefix('~') {
            let home = dirs::home_dir()?;
            Some(home.to_string_lossy().to_string() + rest)
        } else {
            Some(raw.to_string())
        }
    }

    fn update_path_suggestions(&mut self) {
        let Some(expanded) = Self::expand_tilde(&self.input_buffer) else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        // Split into base directory and partial name
        let (base_dir, partial) = if expanded.ends_with('/') {
            (expanded.as_str(), "")
        } else if let Some(pos) = expanded.rfind('/') {
            (&expanded[..=pos], &expanded[pos + 1..])
        } else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        let partial_lower = partial.to_lowercase();

        let Ok(entries) = std::fs::read_dir(base_dir) else {
            self.show_path_suggestions = false;
            self.path_suggestions.clear();
            return;
        };

        let mut suggestions: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_type().is_ok_and(|ft| ft.is_dir())
                    && !e.file_name().to_string_lossy().starts_with('.')
            })
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| partial.is_empty() || name.to_lowercase().starts_with(&partial_lower))
            .collect();

        suggestions.sort_unstable();

        self.show_path_suggestions = !suggestions.is_empty();
        self.path_suggestions = suggestions;
        self.path_suggestion_index = 0;
    }

    fn accept_path_suggestion(&mut self) {
        let Some(suggestion) = self
            .path_suggestions
            .get(self.path_suggestion_index)
            .cloned()
        else {
            return;
        };

        let Some(expanded) = Self::expand_tilde(&self.input_buffer) else {
            return;
        };

        let base = if expanded.ends_with('/') {
            expanded
        } else if let Some(pos) = expanded.rfind('/') {
            expanded[..=pos].to_string()
        } else {
            return;
        };

        // Reconstruct with ~ if original started with ~
        let new_path = if self.input_buffer.starts_with('~') {
            if let Some(home) = dirs::home_dir() {
                let home_str = home.to_string_lossy().to_string();
                if let Some(rest) = base.strip_prefix(&home_str) {
                    format!("~{rest}{suggestion}/")
                } else {
                    format!("{base}{suggestion}/")
                }
            } else {
                format!("{base}{suggestion}/")
            }
        } else {
            format!("{base}{suggestion}/")
        };

        self.input_buffer = new_path;
        self.input_cursor = self.input_buffer.len();
        self.update_path_suggestions();
    }

    fn clear_path_autocomplete(&mut self) {
        self.path_suggestions.clear();
        self.path_suggestion_index = 0;
        self.show_path_suggestions = false;
    }

    /// Update or clear path autocomplete after a deletion in the path field.
    fn refresh_path_autocomplete_after_delete(&mut self) {
        if self.show_path_suggestions {
            if self.input_buffer.contains('/') || self.input_buffer == "~" {
                self.update_path_suggestions();
            } else {
                self.clear_path_autocomplete();
            }
        }
    }

    fn reset_task_form(&mut self) {
        self.input_buffer.clear();
        self.input_cursor = 0;
        self.new_task_description.clear();
        self.new_task_mode = crate::store::TaskMode::Autonomous;
        self.new_task_base.clear();
        self.new_task_branch.clear();
        self.new_task_push_mode = crate::store::PushMode::Pr;
        self.new_task_review_loop = false;
        self.new_task_field = 0;
        self.new_task_subtasks.clear();
        self.new_task_subtask_index = 0;
        self.editing_subtask_index = None;
    }

    pub(super) fn handle_confirm_delete_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Char('y') => {
                if !self.confirm_entity_id.is_empty() {
                    let name = self.confirm_target.clone();
                    match self.confirm_delete_kind {
                        DeleteTarget::Project => {
                            self.store.delete_project(&self.confirm_entity_id)?;
                            self.project_index = 0;
                            self.show_toast(
                                format!("Project '{name}' deleted"),
                                ToastStyle::Success,
                            );
                        }
                        DeleteTarget::Task => {
                            // Spawn teardown in background if task has a linked session
                            if let Ok(task) = self.store.get_task(&self.confirm_entity_id)
                                && let Some(ref sid) = task.session_id
                            {
                                self.spawn_teardown_session(sid.clone());
                            }
                            self.store.delete_task(&self.confirm_entity_id)?;
                            self.show_toast(format!("Task '{name}' deleted"), ToastStyle::Success);
                        }
                    }
                    self.confirm_entity_id.clear();
                    self.confirm_target.clear();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc | KeyCode::Char('n') => {
                self.confirm_entity_id.clear();
                self.confirm_target.clear();
                self.input_mode = InputMode::Normal;
            }
            _ => {}
        }
        Ok(())
    }

    fn move_down(&mut self) {
        match self.focus {
            Focus::Projects => {
                if !self.projects.is_empty() {
                    self.project_index =
                        (self.project_index + 1).min(self.projects.len().saturating_sub(1));
                    // Auto-load sessions/tasks for newly selected project
                    let _ = self.refresh_data();
                    self.task_index = 0;
                }
            }
            Focus::Tasks => {
                let visible_count = self.visible_tasks().len();
                if visible_count > 0 {
                    self.task_index = (self.task_index + 1).min(visible_count.saturating_sub(1));
                }
            }
        }
    }

    fn move_up(&mut self) {
        match self.focus {
            Focus::Projects => {
                self.project_index = self.project_index.saturating_sub(1);
                let _ = self.refresh_data();
                self.task_index = 0;
            }
            Focus::Tasks => {
                self.task_index = self.task_index.saturating_sub(1);
            }
        }
    }

    pub(super) fn handle_palette_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Enter => {
                if let Some(&idx) = self.palette_filtered.get(self.palette_index) {
                    let Some(item) = self.palette_items.get(idx) else {
                        return Ok(());
                    };
                    let action = item.action;
                    self.input_buffer.clear();
                    self.input_mode = InputMode::Normal;
                    self.execute_palette_action(action)?;
                }
            }
            KeyCode::Up | KeyCode::Char('k') if self.input_buffer.is_empty() => {
                self.palette_index = self.palette_index.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.input_buffer.is_empty() => {
                if self.palette_filtered.len() > 1 {
                    self.palette_index =
                        (self.palette_index + 1).min(self.palette_filtered.len().saturating_sub(1));
                }
            }
            _ => {
                if apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                ) {
                    self.filter_palette();
                    self.palette_index = self
                        .palette_index
                        .min(self.palette_filtered.len().saturating_sub(1));
                }
            }
        }
        Ok(())
    }

    pub(super) fn filter_palette(&mut self) {
        let query = self.input_buffer.to_lowercase();
        if query.is_empty() {
            self.palette_filtered = (0..self.palette_items.len()).collect();
        } else {
            self.palette_filtered = self
                .palette_items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.label.to_lowercase().contains(&query))
                .map(|(i, _)| i)
                .collect();
        }
    }

    fn execute_palette_action(&mut self, action: PaletteAction) -> Result<()> {
        match action {
            PaletteAction::NewTask => {
                if self.selected_project().is_some() {
                    self.reset_task_form();
                    self.input_mode = InputMode::NewTask;
                }
            }
            PaletteAction::AddProject => {
                self.input_mode = InputMode::NewProject;
                self.input_buffer.clear();
                self.new_project_name.clear();
                self.new_project_path = String::from(".");
                self.new_project_field = 0;
                self.clear_path_autocomplete();
            }
            PaletteAction::RemoveProject => {
                if let Some((name, id)) = self
                    .selected_project()
                    .map(|p| (p.name.clone(), p.id.clone()))
                {
                    self.confirm_target = name;
                    self.confirm_entity_id = id;
                    self.confirm_delete_kind = DeleteTarget::Project;
                    self.input_mode = InputMode::ConfirmDelete;
                }
            }
            PaletteAction::FocusProjects => self.focus = Focus::Projects,
            PaletteAction::FocusTasks => self.focus = Focus::Tasks,
            PaletteAction::FindSkills => {
                self.refresh_skills();
                self.input_mode = InputMode::SkillSearch;
                self.input_buffer.clear();
                self.search_results.clear();
            }
            PaletteAction::UpdateSkills => {
                self.skill_status_message = "Updating skills...".to_string();
                match crate::skills::update_skills() {
                    Ok(msg) => {
                        self.skill_status_message = msg;
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.skill_status_message = format!("Update failed: {e}");
                    }
                }
            }
            PaletteAction::Quit => self.should_quit = true,
        }
        Ok(())
    }

    pub fn refresh_skills(&mut self) {
        let mut all_skills = crate::skills::list_skills(true, None).unwrap_or_default();

        if let Some(project) = self.selected_project() {
            let project_skills =
                crate::skills::list_skills(false, Some(&project.repo_path)).unwrap_or_default();
            all_skills.extend(project_skills);
        }

        self.installed_skills = all_skills;

        if self.skill_index >= self.installed_skills.len() && !self.installed_skills.is_empty() {
            self.skill_index = self.installed_skills.len() - 1;
        }

        self.refresh_skill_detail();
    }

    fn refresh_skill_detail(&mut self) {
        if let Some(skill) = self.installed_skills.get(self.skill_index) {
            self.skill_detail_content = crate::skills::read_skill_md(&skill.path)
                .unwrap_or_else(|_| "Could not read SKILL.md".to_string());
        } else {
            self.skill_detail_content.clear();
        }
    }

    pub(super) fn handle_skill_panel_key(&mut self, code: KeyCode) -> Result<()> {
        match code {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.installed_skills.is_empty() {
                    self.skill_index =
                        (self.skill_index + 1).min(self.installed_skills.len().saturating_sub(1));
                    self.refresh_skill_detail();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.skill_index = self.skill_index.saturating_sub(1);
                self.refresh_skill_detail();
            }
            KeyCode::Char('f') => {
                self.input_mode = InputMode::SkillSearch;
                self.input_buffer.clear();
                self.search_results.clear();
                self.skill_index = 0;
            }
            KeyCode::Char('a') => {
                self.input_mode = InputMode::SkillAdd;
                self.input_buffer.clear();
            }
            KeyCode::Char('x') => {
                if let Some(skill) = self.installed_skills.get(self.skill_index) {
                    let name = skill.name.clone();
                    let global = skill.scope == crate::skills::SkillScope::Global;
                    let project_path =
                        if let crate::skills::SkillScope::Project(ref p) = skill.scope {
                            Some(p.clone())
                        } else {
                            None
                        };
                    match crate::skills::remove_skill(&name, global, project_path.as_deref()) {
                        Ok(_) => {
                            self.show_toast(format!("Removed {name}"), ToastStyle::Success);
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.show_toast(format!("Remove failed: {e}"), ToastStyle::Error);
                        }
                    }
                }
            }
            KeyCode::Char('u') => {
                self.show_toast("Updating skills...", ToastStyle::Info);
                match crate::skills::update_skills() {
                    Ok(msg) => {
                        self.show_toast(msg, ToastStyle::Success);
                        self.refresh_skills();
                    }
                    Err(e) => {
                        self.show_toast(format!("Update failed: {e}"), ToastStyle::Error);
                    }
                }
            }
            KeyCode::Char('g') => {
                self.skill_scope_global = !self.skill_scope_global;
                self.refresh_skills();
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_skill_search_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty() {
                    if self.search_results.is_empty() {
                        let query = self.input_buffer.clone();
                        self.skill_status_message = format!("Searching for '{query}'...");
                        match crate::skills::find_skills(&query) {
                            Ok(results) => {
                                self.skill_status_message =
                                    format!("Found {} results", results.len());
                                self.search_results = results;
                                self.skill_index = 0;
                            }
                            Err(e) => {
                                self.skill_status_message = format!("Search failed: {e}");
                            }
                        }
                    } else if let Some(result) = self.search_results.get(self.skill_index) {
                        let package = result.package.clone();
                        let global = self.skill_scope_global;
                        let project_path = if global {
                            None
                        } else {
                            self.selected_project().map(|p| p.repo_path.clone())
                        };

                        self.skill_status_message = format!("Installing {package}...");
                        match crate::skills::add_skill(&package, global, project_path.as_deref()) {
                            Ok(_) => {
                                self.skill_status_message = format!("Installed {package}");
                                self.input_mode = InputMode::SkillPanel;
                                self.input_buffer.clear();
                                self.search_results.clear();
                                self.refresh_skills();
                            }
                            Err(e) => {
                                self.skill_status_message = format!("Install failed: {e}");
                            }
                        }
                    }
                }
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.search_results.clear();
                self.input_mode = InputMode::SkillPanel;
                self.skill_status_message.clear();
            }
            KeyCode::Char('j') | KeyCode::Down if !self.search_results.is_empty() => {
                self.skill_index =
                    (self.skill_index + 1).min(self.search_results.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up if !self.search_results.is_empty() => {
                self.skill_index = self.skill_index.saturating_sub(1);
            }
            _ => {
                if apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                ) {
                    self.search_results.clear();
                    self.skill_index = 0;
                    self.skill_status_message.clear();
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_edit_task_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        if self.handle_task_form_shared_key(code, modifiers) {
            return Ok(());
        }
        match code {
            KeyCode::Enter => {
                self.save_current_task_field();
                let is_exploration = self.new_task_mode == crate::store::TaskMode::Exploration;
                if !self.new_task_description.is_empty() || is_exploration {
                    if let Some(ref task_id) = self.editing_task_id.clone() {
                        let fallback = if is_exploration && self.new_task_description.is_empty() {
                            "Exploration session".to_string()
                        } else {
                            fallback_title(&self.new_task_description)
                        };
                        let branch = if self.new_task_branch.is_empty() {
                            None
                        } else {
                            Some(self.new_task_branch.as_str())
                        };
                        let base = if self.new_task_base.is_empty() {
                            None
                        } else {
                            Some(self.new_task_base.as_str())
                        };
                        self.store.update_task(
                            task_id,
                            &fallback,
                            &self.new_task_description,
                            self.new_task_mode,
                            branch,
                            base,
                            self.new_task_push_mode,
                            self.new_task_review_loop,
                        )?;

                        // Promote draft -> pending on submit
                        if let Ok(task) = self.store.get_task(task_id)
                            && task.status == crate::store::TaskStatus::Draft
                        {
                            self.store
                                .update_task_status(task_id, crate::store::TaskStatus::Pending)?;
                        }

                        // Create inline subtasks added during edit
                        for subtask_desc in &self.new_task_subtasks {
                            let st_title = fallback_title(subtask_desc);
                            self.store
                                .create_subtask(task_id, &st_title, subtask_desc)?;
                        }

                        // Launch autonomous and exploration tasks immediately,
                        // or just generate the title for supervised tasks.
                        if matches!(
                            self.new_task_mode,
                            crate::store::TaskMode::Autonomous
                                | crate::store::TaskMode::Exploration
                        ) && let Some(project_id) = self.selected_project().map(|p| p.id.clone())
                        {
                            self.launch_task(task_id.clone(), project_id)?;
                        } else {
                            self.spawn_title_generation(
                                task_id.clone(),
                                self.new_task_description.clone(),
                            );
                        }
                        self.show_toast("Task updated", ToastStyle::Success);
                    }
                    self.editing_task_id = None;
                    self.reset_task_form();
                    self.input_mode = InputMode::Normal;
                    self.refresh_data()?;
                }
            }
            KeyCode::Esc => {
                self.save_current_task_field();
                let is_exploration = self.new_task_mode == crate::store::TaskMode::Exploration;
                if (!self.new_task_description.is_empty() || is_exploration)
                    && let Some(ref task_id) = self.editing_task_id.clone()
                {
                    let fallback = if is_exploration && self.new_task_description.is_empty() {
                        "Exploration session".to_string()
                    } else {
                        fallback_title(&self.new_task_description)
                    };
                    let branch = if self.new_task_branch.is_empty() {
                        None
                    } else {
                        Some(self.new_task_branch.as_str())
                    };
                    let base = if self.new_task_base.is_empty() {
                        None
                    } else {
                        Some(self.new_task_base.as_str())
                    };
                    self.store.update_task(
                        task_id,
                        &fallback,
                        &self.new_task_description,
                        self.new_task_mode,
                        branch,
                        base,
                        self.new_task_push_mode,
                        self.new_task_review_loop,
                    )?;

                    // Create inline subtasks added during edit
                    for subtask_desc in &self.new_task_subtasks {
                        let st_title = fallback_title(subtask_desc);
                        self.store
                            .create_subtask(task_id, &st_title, subtask_desc)?;
                    }

                    self.spawn_title_generation(task_id.clone(), self.new_task_description.clone());
                    self.show_toast("Task draft saved", ToastStyle::Info);
                }
                self.editing_task_id = None;
                self.reset_task_form();
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn handle_task_filter_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter => {
                self.input_mode = InputMode::Normal;
                self.task_index = 0;
            }
            KeyCode::Esc => {
                self.task_filter.clear();
                self.recompute_visible_tasks();
                self.input_mode = InputMode::Normal;
                self.task_index = 0;
            }
            _ => {
                if apply_text_edit(
                    &mut self.task_filter,
                    &mut self.task_filter_cursor,
                    code,
                    modifiers,
                ) {
                    self.recompute_visible_tasks();
                    self.task_index = 0;
                }
            }
        }
        Ok(())
    }

    pub(super) fn handle_subtask_panel_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty()
                    && let Some(task) = self.visible_tasks().get(self.task_index)
                {
                    let task_id = task.id.clone();
                    let desc = std::mem::take(&mut self.input_buffer);
                    let title = fallback_title(&desc);
                    self.store.create_subtask(&task_id, &title, &desc)?;
                    self.subtasks = self
                        .store
                        .list_subtasks_for_task(&task_id)
                        .unwrap_or_default();
                    self.show_toast("Subtask added", ToastStyle::Success);
                }
            }
            KeyCode::Char('d') if self.input_buffer.is_empty() => {
                if let Some(st) = self.subtasks.get(self.subtask_index) {
                    let st_id = st.id.clone();
                    let task_id = st.task_id.clone();
                    self.store.delete_subtask(&st_id)?;
                    self.subtasks = self
                        .store
                        .list_subtasks_for_task(&task_id)
                        .unwrap_or_default();
                    if self.subtask_index >= self.subtasks.len() {
                        self.subtask_index = self.subtasks.len().saturating_sub(1);
                    }
                    self.show_toast("Subtask deleted", ToastStyle::Success);
                }
            }
            KeyCode::Char('j') | KeyCode::Down if self.input_buffer.is_empty() => {
                if !self.subtasks.is_empty() {
                    self.subtask_index =
                        (self.subtask_index + 1).min(self.subtasks.len().saturating_sub(1));
                }
            }
            KeyCode::Char('k') | KeyCode::Up if self.input_buffer.is_empty() => {
                self.subtask_index = self.subtask_index.saturating_sub(1);
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = InputMode::Normal;
                self.refresh_data()?;
            }
            _ => {
                apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
            }
        }
        Ok(())
    }

    pub(super) fn handle_skill_add_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        match code {
            KeyCode::Enter => {
                if !self.input_buffer.is_empty() {
                    let package = self.input_buffer.clone();
                    let global = self.skill_scope_global;
                    let project_path = if global {
                        None
                    } else {
                        self.selected_project().map(|p| p.repo_path.clone())
                    };

                    self.skill_status_message = format!("Installing {package}...");
                    match crate::skills::add_skill(&package, global, project_path.as_deref()) {
                        Ok(_) => {
                            self.skill_status_message = format!("Installed {package}");
                            self.input_mode = InputMode::SkillPanel;
                            self.input_buffer.clear();
                            self.refresh_skills();
                        }
                        Err(e) => {
                            self.skill_status_message = format!("Install failed: {e}");
                        }
                    }
                }
            }
            KeyCode::Esc => {
                self.input_buffer.clear();
                self.input_cursor = 0;
                self.input_mode = InputMode::SkillPanel;
            }
            _ => {
                apply_text_edit(
                    &mut self.input_buffer,
                    &mut self.input_cursor,
                    code,
                    modifiers,
                );
            }
        }
        Ok(())
    }
}

// ── Free functions: key/mouse encoding ──

/// Convert a crossterm key event into the raw bytes a terminal would send.
/// Stack-allocated key byte buffer (avoids heap allocation per keystroke).
/// Maximum escape sequence is 4 bytes (e.g. `\x1b[3~`), and max UTF-8 char is 4 bytes.
pub(crate) struct KeyBytes {
    buf: [u8; 8],
    pub len: usize,
}

impl KeyBytes {
    const fn empty() -> Self {
        Self {
            buf: [0; 8],
            len: 0,
        }
    }

    fn from_slice(s: &[u8]) -> Self {
        let mut buf = [0u8; 8];
        let len = s.len().min(8);
        buf[..len].copy_from_slice(&s[..len]);
        Self { buf, len }
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

pub(crate) fn keycode_to_bytes(code: KeyCode, modifiers: KeyModifiers) -> KeyBytes {
    // Ctrl+letter -> control character (ASCII)
    if modifiers.contains(KeyModifiers::CONTROL)
        && let KeyCode::Char(c) = code
    {
        let ctrl = (c.to_ascii_lowercase() as u8)
            .wrapping_sub(b'a')
            .wrapping_add(1);
        return KeyBytes {
            buf: [ctrl, 0, 0, 0, 0, 0, 0, 0],
            len: 1,
        };
    }

    // Modified special keys (arrows, Home, End, Insert, Delete, Page, F-keys).
    // Uses xterm modifier encoding: \x1b[1;{mod}X for arrow/Home/End,
    // \x1b[N;{mod}~ for tilde-style keys, \x1b[1;{mod}X for F1-F4.
    // mod = 1 + (shift?1:0) + (alt?2:0) + (ctrl?4:0)
    if modifiers != KeyModifiers::NONE
        && !matches!(code, KeyCode::Char(_))
        && let Some(bytes) = modified_special_key(code, modifiers)
    {
        return bytes;
    }

    // Alt+key -> ESC prefix (standard terminal convention)
    if modifiers.contains(KeyModifiers::ALT) {
        let base = keycode_to_bytes_base(code);
        if base.len > 0 {
            let mut buf = [0u8; 8];
            buf[0] = 0x1b;
            let copy_len = base.len.min(7);
            buf[1..=copy_len].copy_from_slice(&base.buf[..copy_len]);
            return KeyBytes {
                buf,
                len: 1 + copy_len,
            };
        }
        return KeyBytes::empty();
    }

    keycode_to_bytes_base(code)
}

/// Encode a special key (arrow, Home, End, etc.) with modifier bits in
/// xterm's `\x1b[1;{mod}X` / `\x1b[N;{mod}~` format.
///
/// Returns `None` for keys that don't have a modified encoding.
fn modified_special_key(code: KeyCode, modifiers: KeyModifiers) -> Option<KeyBytes> {
    let modifier_val = 1u8
        + u8::from(modifiers.contains(KeyModifiers::SHIFT))
        + u8::from(modifiers.contains(KeyModifiers::ALT)) * 2
        + u8::from(modifiers.contains(KeyModifiers::CONTROL)) * 4;

    // Keys using \x1b[1;{mod}{letter} format
    let letter_key = match code {
        KeyCode::Up => Some(b'A'),
        KeyCode::Down => Some(b'B'),
        KeyCode::Right => Some(b'C'),
        KeyCode::Left => Some(b'D'),
        KeyCode::Home => Some(b'H'),
        KeyCode::End => Some(b'F'),
        _ => None,
    };

    if let Some(k) = letter_key {
        let seq = [0x1b, b'[', b'1', b';', b'0' + modifier_val, k, 0, 0];
        return Some(KeyBytes { buf: seq, len: 6 });
    }

    // Keys using \x1b[{N};{mod}~ format (tilde-style)
    let tilde_param = match code {
        KeyCode::Insert => Some(b'2'),
        KeyCode::Delete => Some(b'3'),
        KeyCode::PageUp => Some(b'5'),
        KeyCode::PageDown => Some(b'6'),
        _ => None,
    };

    if let Some(n) = tilde_param {
        let seq = [0x1b, b'[', n, b';', b'0' + modifier_val, b'~', 0, 0];
        return Some(KeyBytes { buf: seq, len: 6 });
    }

    // F1-F4: \x1b[1;{mod}{P-S}
    if let KeyCode::F(n) = code
        && (1..=4).contains(&n)
    {
        let k = b'O' + n; // P=1, Q=2, R=3, S=4
        let seq = [0x1b, b'[', b'1', b';', b'0' + modifier_val, k, 0, 0];
        return Some(KeyBytes { buf: seq, len: 6 });
    }

    // F5-F12: \x1b[{N};{mod}~ (two-digit N requires dynamic formatting)
    if let KeyCode::F(n) = code
        && (5..=12).contains(&n)
    {
        let param = match n {
            5 => "15",
            6 => "17",
            7 => "18",
            8 => "19",
            9 => "20",
            10 => "21",
            11 => "23",
            12 => "24",
            _ => return None,
        };
        let s = format!("\x1b[{param};{modifier_val}~");
        return Some(KeyBytes::from_slice(s.as_bytes()));
    }

    None
}

/// Map a keycode (without modifiers) to its raw terminal bytes.
///
/// Philosophy: forward ALL byte-producing keys to the PTY by default.
/// Only keys intercepted earlier in `handle_session_tab_key` are excluded.
/// Non-byte-producing keys (modifier-only, media, etc.) return empty.
fn keycode_to_bytes_base(code: KeyCode) -> KeyBytes {
    match code {
        KeyCode::Char(c) => {
            let mut buf = [0u8; 8];
            let s = c.encode_utf8(&mut buf[..4]);
            let len = s.len();
            KeyBytes { buf, len }
        }
        KeyCode::Esc => KeyBytes::from_slice(b"\x1b"),
        KeyCode::Enter => KeyBytes::from_slice(b"\r"),
        KeyCode::Backspace => KeyBytes::from_slice(&[0x7f]),
        KeyCode::Tab => KeyBytes::from_slice(b"\t"),
        KeyCode::BackTab => KeyBytes::from_slice(b"\x1b[Z"),
        KeyCode::Up => KeyBytes::from_slice(b"\x1b[A"),
        KeyCode::Down => KeyBytes::from_slice(b"\x1b[B"),
        KeyCode::Right => KeyBytes::from_slice(b"\x1b[C"),
        KeyCode::Left => KeyBytes::from_slice(b"\x1b[D"),
        KeyCode::Home => KeyBytes::from_slice(b"\x1b[H"),
        KeyCode::End => KeyBytes::from_slice(b"\x1b[F"),
        KeyCode::Insert => KeyBytes::from_slice(b"\x1b[2~"),
        KeyCode::Delete => KeyBytes::from_slice(b"\x1b[3~"),
        KeyCode::PageUp => KeyBytes::from_slice(b"\x1b[5~"),
        KeyCode::PageDown => KeyBytes::from_slice(b"\x1b[6~"),
        KeyCode::Null => KeyBytes::from_slice(&[0x00]),
        KeyCode::F(n) => match n {
            1 => KeyBytes::from_slice(b"\x1bOP"),
            2 => KeyBytes::from_slice(b"\x1bOQ"),
            3 => KeyBytes::from_slice(b"\x1bOR"),
            4 => KeyBytes::from_slice(b"\x1bOS"),
            5 => KeyBytes::from_slice(b"\x1b[15~"),
            6 => KeyBytes::from_slice(b"\x1b[17~"),
            7 => KeyBytes::from_slice(b"\x1b[18~"),
            8 => KeyBytes::from_slice(b"\x1b[19~"),
            9 => KeyBytes::from_slice(b"\x1b[20~"),
            10 => KeyBytes::from_slice(b"\x1b[21~"),
            11 => KeyBytes::from_slice(b"\x1b[23~"),
            12 => KeyBytes::from_slice(b"\x1b[24~"),
            _ => KeyBytes::empty(),
        },
        // Modifier-only keys, media keys, etc. don't produce terminal bytes
        _ => KeyBytes::empty(),
    }
}

/// Encode a crossterm mouse event as terminal escape sequences for forwarding
/// to a PTY application that has enabled mouse tracking.
///
/// Returns `None` for event types that the protocol doesn't cover.
///
/// Supports both SGR encoding (`\x1b[<btn;col;rowM/m`) and the legacy
/// default encoding (`\x1b[Mbxy`).  SGR is preferred by modern TUI apps
/// (crossterm/ratatui) because it handles coordinates > 222 and
/// distinguishes press from release.
pub(crate) fn encode_mouse_event(
    kind: &MouseEventKind,
    vt_col: u16,
    vt_row: u16,
    encoding: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    // SGR uses 1-based coordinates
    let x = u32::from(vt_col) + 1;
    let y = u32::from(vt_row) + 1;

    let (button, is_release) = match kind {
        MouseEventKind::Down(MouseButton::Left) => (0u8, false),
        MouseEventKind::Down(MouseButton::Right) => (2, false),
        MouseEventKind::Down(MouseButton::Middle) => (1, false),
        MouseEventKind::Up(MouseButton::Left) => (0, true),
        MouseEventKind::Up(MouseButton::Right) => (2, true),
        MouseEventKind::Up(MouseButton::Middle) => (1, true),
        MouseEventKind::Drag(MouseButton::Left) => (32, false), // motion + left
        MouseEventKind::Drag(MouseButton::Right) => (34, false), // motion + right
        MouseEventKind::Drag(MouseButton::Middle) => (33, false),
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::Moved => (35, false), // motion, no button
        _ => return None,
    };

    match encoding {
        vt100::MouseProtocolEncoding::Sgr => {
            let suffix = if is_release { 'm' } else { 'M' };
            Some(format!("\x1b[<{button};{x};{y}{suffix}").into_bytes())
        }
        // Default and UTF-8 both use the `\x1b[M` prefix format.
        // Default caps at 223; UTF-8 extends to higher values but uses the
        // same structure.  For simplicity we use the same path for both,
        // clamping to what the encoding can represent.
        _ => {
            if is_release {
                // Default encoding: release is button 3
                let cb = 3u8 + 32;
                let cx = u8::try_from(x.min(255))
                    .expect("clamped to 255")
                    .wrapping_add(32);
                let cy = u8::try_from(y.min(255))
                    .expect("clamped to 255")
                    .wrapping_add(32);
                Some(vec![0x1b, b'[', b'M', cb, cx, cy])
            } else {
                let cb = button.wrapping_add(32);
                let cx = u8::try_from(x.min(255))
                    .expect("clamped to 255")
                    .wrapping_add(32);
                let cy = u8::try_from(y.min(255))
                    .expect("clamped to 255")
                    .wrapping_add(32);
                Some(vec![0x1b, b'[', b'M', cb, cx, cy])
            }
        }
    }
}
