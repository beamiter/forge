//! actions — UiState methods extracted from ui (mechanical split, no logic changes)
use adw::prelude::*;
use gtk4::glib;
use gtk4::Orientation;
use libadwaita as adw;
use std::rc::Rc;
use vte4::Format;
use vte4::Terminal;
use vte4::TerminalExt;

use super::*;
use crate::block_view::{RecordNavigationResult, SessionExportFormat, TermView};
use crate::keybindings::{Action, Direction};
use crate::terminal::terminal_working_directory;

const MIN_AI_PANEL_WIDTH: i32 = 240;
const MAX_AI_PANEL_WIDTH: i32 = 1200;
const MIN_AI_WORKSPACE_WIDTH: i32 = 200;

fn apply_ai_panel_width(paned: &gtk4::Paned, requested_width: u32) {
    let total_width = paned.width();
    let Some(position) = restored_ai_panel_position(total_width, requested_width) else {
        return;
    };
    paned.set_position(position);
}

fn restored_ai_panel_position(total_width: i32, requested_width: u32) -> Option<i32> {
    if total_width <= MIN_AI_PANEL_WIDTH + MIN_AI_WORKSPACE_WIDTH {
        return None;
    }
    let available = total_width - MIN_AI_WORKSPACE_WIDTH;
    let panel_width = (requested_width as i32).clamp(MIN_AI_PANEL_WIDTH, available);
    Some(total_width - panel_width)
}

fn ai_panel_width_from_geometry(total_width: i32, position: i32) -> Option<u32> {
    if total_width <= 0 || position < 0 || position >= total_width {
        return None;
    }
    Some((total_width - position).clamp(MIN_AI_PANEL_WIDTH, MAX_AI_PANEL_WIDTH) as u32)
}

fn remote_host_index_for_action(
    index: u8,
    host_count: usize,
    safe_mode: bool,
) -> Result<usize, &'static str> {
    if safe_mode {
        return Err("Remote connections are disabled in safe mode.");
    }
    let index = usize::from(index);
    if index >= host_count {
        return Err("That remote host is no longer configured.");
    }
    Ok(index)
}

fn record_navigation_toast(result: RecordNavigationResult) -> Option<&'static str> {
    match result {
        RecordNavigationResult::LocationUnavailable => {
            Some("This record has no exact terminal location and no retained output snapshot.")
        }
        // SnapshotView opens the read-only dialog instead of toasting.
        RecordNavigationResult::Navigated
        | RecordNavigationResult::NoMatchingRecord
        | RecordNavigationResult::SnapshotView { .. } => None,
    }
}

impl UiState {
    /// Hotkey path for opacity: apply to the window, write through to the
    /// config (same persistence as the settings dialog, so the value survives
    /// restarts like it does in ember/frost), and show toast feedback.
    fn apply_opacity(&self, opacity: f64) {
        self.window_opacity.set(opacity);
        self.window.set_opacity(opacity);
        self.config.borrow_mut().window_opacity = opacity;
        self.persist_config();
        self.show_opacity_toast();
    }

    /// Hotkey feedback: show the current opacity as a percentage. Repeat
    /// presses update the toast in place rather than queueing one per step.
    fn show_opacity_toast(&self) {
        let message = format!("Opacity: {:.0}%", self.window_opacity.get() * 100.0);
        if let Some(toast) = self.opacity_toast.borrow().as_ref() {
            toast.set_title(&message);
            return;
        }
        let toast = adw::Toast::new(&message);
        toast.set_timeout(2);
        let slot = Rc::clone(&self.opacity_toast);
        toast.connect_dismissed(move |_| {
            slot.borrow_mut().take();
        });
        *self.opacity_toast.borrow_mut() = Some(toast.clone());
        self.toast_overlay.add_toast(toast);
    }

    fn report_record_navigation(&self, result: RecordNavigationResult) {
        if let RecordNavigationResult::SnapshotView { record_id } = result {
            self.show_record_snapshot_dialog(record_id);
            return;
        }
        if let Some(message) = record_navigation_toast(result) {
            self.toast_overlay.add_toast(adw::Toast::new(message));
        }
    }

    pub(crate) fn execute_action(&self, action: Action) {
        let font_step = 0.025;
        let opacity_step = 0.025;
        let current_terminal = self.current_terminal();

        match action {
            Action::NewTab => {
                log::info!("New tab");
                let working_directory = current_terminal
                    .as_ref()
                    .and_then(terminal_working_directory);
                let startup = self.config.borrow().startup_commands.clone();
                self.add_new_tab(
                    working_directory,
                    None,
                    None,
                    crate::terminal::InitialCommands::from_config(startup.as_deref()),
                );
            }
            Action::CloseTab => {
                log::info!("Close tab");
                self.remove_current_tab();
            }
            Action::ClosePaneOrTab => {
                log::info!("Close focused pane or tab");
                self.close_focused_pane_or_tab();
            }
            Action::Copy => {
                log::debug!(">>> UI Action::Copy triggered");
                if let Some(term_view) = self.current_term_view() {
                    log::debug!(">>> UI Copy: calling term_view.copy_to_clipboard");
                    term_view.copy_to_clipboard();
                } else {
                    log::debug!(">>> UI Copy: no current term_view, falling back to VTE");
                    if let Some(ref term) = current_terminal {
                        term.copy_clipboard_format(Format::Text);
                    }
                }
            }
            Action::Paste => {
                log::debug!(">>> UI Action::Paste triggered");
                if let Some(term_view) = self.current_term_view() {
                    log::debug!(">>> UI Paste: calling term_view.paste_from_clipboard");
                    term_view.paste_from_clipboard();
                } else {
                    log::debug!(">>> UI Paste: no current term_view, falling back to VTE");
                    if let Some(ref term) = current_terminal {
                        term.paste_clipboard();
                    }
                }
            }
            Action::FontIncrease => {
                log::debug!("Font increase");
                let new_scale = (self.font_scale.get() + font_step).min(10.0);
                // Same apply-and-persist path as the settings dialog, so the
                // hotkey survives restarts like it does in ember/frost.
                self.apply_font_scale(new_scale);
            }
            Action::FontDecrease => {
                log::debug!("Font decrease");
                let new_scale = (self.font_scale.get() - font_step).max(0.1);
                self.apply_font_scale(new_scale);
            }
            Action::FontReset => {
                log::debug!("Font reset");
                self.apply_font_scale(1.0);
            }
            Action::OpacityIncrease => {
                log::debug!("Opacity increase");
                self.apply_opacity((self.window_opacity.get() + opacity_step).clamp(0.01, 1.0));
            }
            Action::OpacityDecrease => {
                log::debug!("Opacity decrease");
                self.apply_opacity((self.window_opacity.get() - opacity_step).clamp(0.01, 1.0));
            }
            Action::ToggleSearch => {
                log::debug!("Toggle search");
                self.toggle_search();
            }
            Action::ToggleCommandPalette => {
                log::debug!("Toggle command palette");
                self.toggle_unified_command_palette();
            }
            Action::ToggleSettings => {
                log::debug!("Toggle settings panel");
                self.toggle_settings_panel();
            }
            Action::ReloadConfig => {
                log::info!("Reload configuration");
                self.reload_config();
            }
            Action::ToggleSidebar => {
                log::debug!("Toggle sidebar");
                self.toggle_sidebar();
            }
            Action::FilterTabs => {
                log::debug!("Filter tabs");
                // The filter only ever lives in the sidebar's Tabs view, so
                // reveal it regardless of where the tab strip is docked.
                self.set_sidebar_visible(true, true);
                self.apply_sidebar_view(crate::config::SidebarView::Tabs, false);
                self.tab_search_entry.set_can_focus(true);
                self.tab_search_entry.set_focusable(true);
                self.tab_search_entry.grab_focus();
            }
            Action::CloseSelectedTabs => {
                log::debug!("Close selected tabs");
                self.close_selected_tabs();
            }
            Action::SplitHorizontal => {
                log::debug!("Split horizontal");
                self.split_current(Orientation::Horizontal);
            }
            Action::SplitVertical => {
                log::debug!("Split vertical");
                self.split_current(Orientation::Vertical);
            }
            Action::PrevTab => {
                self.switch_tab(-1);
            }
            Action::NextTab => {
                self.switch_tab(1);
            }
            Action::ScrollUp => {
                if let Some(term_view) = self.current_term_view() {
                    term_view.scroll_lines(-3);
                } else if let Some(ref term) = current_terminal {
                    if let Some(adj) = term.vadjustment() {
                        let new_val = (adj.value() - adj.step_increment() * 3.0).max(adj.lower());
                        adj.set_value(new_val);
                    }
                }
            }
            Action::ScrollDown => {
                if let Some(term_view) = self.current_term_view() {
                    term_view.scroll_lines(3);
                } else if let Some(ref term) = current_terminal {
                    if let Some(adj) = term.vadjustment() {
                        let max_val = adj.upper() - adj.page_size();
                        let new_val = (adj.value() + adj.step_increment() * 3.0).min(max_val);
                        adj.set_value(new_val);
                    }
                }
            }
            Action::CyclePaneFocusForward => {
                self.cycle_pane_focus(1);
            }
            Action::CyclePaneFocusBackward => {
                self.cycle_pane_focus(-1);
            }
            Action::QuickSwitchTab(n) => {
                let n_pages = self.notebook.n_pages();
                if n_pages > 0 {
                    let target = if n == 9 {
                        n_pages - 1
                    } else {
                        (n as u32).min(n_pages - 1)
                    };
                    self.notebook.set_current_page(Some(target));
                }
            }
            Action::ConnectRemote(index) => {
                let safe_mode = std::env::var_os("FORGE_SAFE_MODE").is_some();
                let hosts = self.config.borrow().remote_hosts.clone();
                match remote_host_index_for_action(index, hosts.len(), safe_mode) {
                    Ok(index) => {
                        self.connect_remote(&hosts[index]);
                    }
                    Err(message) => self.toast_overlay.add_toast(adw::Toast::new(message)),
                }
            }
            Action::ShowRemotePicker => {
                self.show_remote_picker();
            }
            Action::ResizePaneLeft => {
                self.resize_pane(Orientation::Horizontal, -30);
            }
            Action::ResizePaneRight => {
                self.resize_pane(Orientation::Horizontal, 30);
            }
            Action::ResizePaneUp => {
                self.resize_pane(Orientation::Vertical, -30);
            }
            Action::ResizePaneDown => {
                self.resize_pane(Orientation::Vertical, 30);
            }
            Action::TogglePaneZoom => {
                self.toggle_pane_zoom();
            }
            Action::MovePaneToNewTab => {
                self.move_pane_to_new_tab();
            }
            Action::FocusPaneLeft => {
                self.focus_pane_directional(Direction::Left);
            }
            Action::FocusPaneRight => {
                self.focus_pane_directional(Direction::Right);
            }
            Action::FocusPaneUp => {
                self.focus_pane_directional(Direction::Up);
            }
            Action::FocusPaneDown => {
                self.focus_pane_directional(Direction::Down);
            }
            Action::MoveTabLeft => {
                self.move_tab_left();
            }
            Action::MoveTabRight => {
                self.move_tab_right();
            }
            Action::DuplicateTab => {
                self.duplicate_current_tab();
            }
            Action::ToggleTabMarked => {
                self.toggle_current_tab_marked();
            }
            Action::ToggleTabPinned => {
                self.toggle_current_tab_pinned();
            }
            Action::ToggleTabPlacement => {
                self.toggle_tab_placement();
            }
            Action::FilterFailedBlocks => {
                log::info!("Jump to first failed block");
                if let Some(term_view) = self.current_term_view() {
                    self.report_record_navigation(term_view.apply_failed_filter());
                }
            }
            Action::FilterSlowBlocks => {
                log::info!("Jump to first slow block");
                if let Some(term_view) = self.current_term_view() {
                    self.report_record_navigation(term_view.apply_slow_filter());
                }
            }
            Action::FilterPinnedBlocks => {
                log::info!("Jump to first bookmarked block");
                if let Some(term_view) = self.current_term_view() {
                    term_view.apply_pinned_filter();
                }
            }
            Action::ClearBlockFilter => {
                log::info!("Jump to oldest block");
                if let Some(term_view) = self.current_term_view() {
                    term_view.clear_block_filter();
                }
            }
            Action::SelectAllBlocks => {
                log::info!("Select all finished blocks");
                if let Some(term_view) = self.current_term_view() {
                    term_view.select_all_blocks();
                }
            }
            Action::ClearBlocks => {
                log::info!("Clear finished blocks");
                if let Some(term_view) = self.current_term_view() {
                    let count = term_view.clear_blocks();
                    if count > 0 {
                        let plural = if count == 1 { "" } else { "s" };
                        let message = format!(
                            "Cleared {count} block{plural} — \"Undo clear blocks\" restores them."
                        );
                        self.toast_overlay.add_toast(adw::Toast::new(&message));
                    } else if !term_view.supports_block_mutation() {
                        // In Block, "nothing to clear" is self-evident from an
                        // empty pane. In Unified the pane is full of output
                        // that this action will never touch, so silence reads
                        // as a broken menu item.
                        self.toast_overlay.add_toast(adw::Toast::new(
                            "Unified mode keeps no blocks to clear — use the shell's own clear.",
                        ));
                    }
                }
            }
            Action::UndoClearBlocks => {
                log::info!("Undo clear finished blocks");
                if let Some(term_view) = self.current_term_view() {
                    let count = term_view.undo_clear_blocks();
                    let message = if count == 0 {
                        "No cleared blocks to restore.".to_string()
                    } else {
                        let plural = if count == 1 { "" } else { "s" };
                        format!("Restored {count} cleared block{plural}.")
                    };
                    self.toast_overlay.add_toast(adw::Toast::new(&message));
                }
            }
            Action::ReinputSelectedCommands => {
                log::info!("Reinput selected commands");
                if let Some(term_view) = self.current_term_view() {
                    term_view.reinput_selected_commands();
                }
            }
            Action::JumpToPrevPinned => {
                if let Some(term_view) = self.current_term_view() {
                    term_view.jump_to_pinned(-1);
                }
            }
            Action::JumpToNextPinned => {
                if let Some(term_view) = self.current_term_view() {
                    term_view.jump_to_pinned(1);
                }
            }
            Action::JumpToPrevFailed => {
                if let Some(term_view) = self.current_term_view() {
                    self.report_record_navigation(term_view.jump_to_failed(-1));
                }
            }
            Action::JumpToNextFailed => {
                if let Some(term_view) = self.current_term_view() {
                    self.report_record_navigation(term_view.jump_to_failed(1));
                }
            }
            Action::ExportSessionMarkdown => {
                self.export_current_session(SessionExportFormat::Markdown);
            }
            Action::ExportSessionJson => {
                self.export_current_session(SessionExportFormat::Json);
            }
            Action::ToggleDebugDashboard => {
                log::debug!("Toggle debug dashboard");
                self.toggle_debug_dashboard();
            }
            Action::ToggleAiPanel => {
                log::debug!("Toggle AI panel");
                self.toggle_ai_panel();
            }
            Action::OpenAiPanel => {
                log::debug!("Open AI panel");
                self.open_ai_panel();
            }
            Action::AskAiAboutSelectedBlock => {
                log::debug!("Ask AI about selected block");
                self.ask_ai_about_selected_block();
            }
            Action::OpenAgent => self.toggle_agent_panel(),
            Action::HistoryPalette => {
                log::debug!("Show history palette");
                self.show_unified_command_palette(crate::palette::PaletteMode::History);
            }
            Action::CrossBlockSearch => {
                log::debug!("Show cross-block search palette");
                self.show_cross_block_search();
            }
            Action::WorkflowsPalette => {
                log::debug!("Show workflows palette");
                self.show_unified_command_palette(crate::palette::PaletteMode::Workflows);
            }
            Action::OpenWelcome => self.open_welcome_notebook(),
            Action::InstallJsh => {
                log::info!("Install or update jsh");
                self.install_or_update_jsh();
            }
        }
    }

    /// Apply the right-side AI chat panel preference. Settings uses this path
    /// too, so changing "Show AI Chats at Startup" updates the live GTK
    /// layout instead of only changing the next-launch state.
    pub(crate) fn set_ai_panel_visible(&self, visible: bool, persist: bool) {
        let visible = visible && self.config.borrow().ai_enabled;
        let attached = self.ai_paned.end_child().is_some();
        let config_changed = self.config.borrow().ai_panel_visible != visible;
        if self.ai_panel_visible.get() == visible && attached == visible {
            self.config.borrow_mut().ai_panel_visible = visible;
            if visible {
                self.restore_ai_panel_width();
            }
            if persist && config_changed {
                self.persist_config();
            }
            return;
        }
        if !visible {
            // Capture the divider before detaching the end child; once hidden,
            // Paned no longer exposes the panel's allocated width.
            self.capture_ai_panel_width();
        }
        self.ai_panel_visible.set(visible);
        if visible {
            self.ai_paned.set_end_child(Some(&self.ai_panel.root));
            self.restore_ai_panel_width();
        } else {
            self.ai_paned.set_end_child(None::<&gtk4::Widget>);
        }
        self.config.borrow_mut().ai_panel_visible = visible;
        if persist {
            self.persist_config();
        }
    }

    /// Show or hide the right-side AI chat panel. Persists the choice in
    /// `config.ai_panel_visible` so the panel state survives restart.
    pub(crate) fn toggle_ai_panel(&self) {
        let next = !self.ai_panel_visible.get();
        if next && !self.config.borrow().ai_enabled {
            self.show_ai_error("AI features are disabled in Settings or safe mode.");
            return;
        }
        self.set_ai_panel_visible(next, true);
        if self.ai_panel_visible.get() {
            self.ai_panel.focus_input();
        } else {
            self.focus_current_terminal();
        }
    }

    /// Backward-compatible one-way AI panel action. Repeating its shortcut
    /// keeps the panel open (and focuses its composer); only the canonical
    /// `toggle_ai_panel` action is allowed to close it.
    pub(crate) fn open_ai_panel(&self) {
        if !self.config.borrow().ai_enabled {
            self.show_ai_error("AI features are disabled in Settings or safe mode.");
            return;
        }
        self.set_ai_panel_visible(true, true);
        self.ai_panel.focus_input();
    }

    /// Restore the configured end-child width after GTK has allocated the
    /// Paned. The idle retry covers startup, config reload, and re-showing the
    /// panel before the current layout pass has completed.
    pub(crate) fn restore_ai_panel_width(&self) {
        if !self.ai_panel_visible.get() {
            return;
        }
        let requested_width = self.config.borrow().ai_panel_width;
        self.ai_panel_width_restoring.set(true);
        apply_ai_panel_width(&self.ai_paned, requested_width);

        let paned = self.ai_paned.clone();
        let visible = self.ai_panel_visible.clone();
        let restoring = self.ai_panel_width_restoring.clone();
        glib::idle_add_local_once(move || {
            if visible.get() {
                apply_ai_panel_width(&paned, requested_width);
            }
            glib::idle_add_local_once(move || restoring.set(false));
        });
    }

    /// Copy the currently allocated AI width into Config. Callers decide when
    /// to flush Config so drag notifications can be debounced into one write.
    pub(crate) fn capture_ai_panel_width(&self) -> bool {
        if !self.ai_panel_visible.get() || self.ai_panel_width_restoring.get() {
            return false;
        }
        let total_width = self.ai_paned.width();
        let position = self.ai_paned.position();
        let Some(measured) = ai_panel_width_from_geometry(total_width, position) else {
            return false;
        };
        let mut config = self.config.borrow_mut();
        if config.ai_panel_width == measured {
            return false;
        }
        config.ai_panel_width = measured;
        true
    }

    /// Grab the selected block's context (cmd + output + cwd + exit) from
    /// the active TermView and hand it to the AI panel. Opens the panel
    /// first if it's hidden; no-ops cleanly when nothing's selected or the
    /// active tab is VTE-mode.
    pub(crate) fn ask_ai_about_selected_block(&self) {
        if !self.config.borrow().ai_enabled {
            self.show_ai_error("AI features are disabled in Settings or safe mode.");
            return;
        }
        let Some(term_view) = self.current_term_view() else {
            log::debug!("AI: no active block-mode tab");
            self.show_ai_error(
                "Ask selected Block requires an active Block-mode pane. Switch to Block mode and select a finished command.",
            );
            return;
        };
        let Some(ctx) = term_view.selected_block_context(80) else {
            log::debug!("AI: no block selected");
            self.show_ai_error(
                "Select a finished command Block first, then ask AI about it again.",
            );
            return;
        };
        self.ask_ai_about_block_context(ctx);
    }

    pub(crate) fn connect_block_ai_action(&self, term_view: &Rc<TermView>) {
        let ui = self.clone();
        term_view.connect_ask_ai_about_block(move |context| {
            ui.ask_ai_about_block_context(context);
        });
    }

    fn ask_ai_about_block_context(&self, ctx: crate::ai::BlockContext) {
        if !self.config.borrow().ai_enabled {
            self.show_ai_error("AI features are disabled in Settings or safe mode.");
            return;
        }
        if !self.ai_panel_visible.get() {
            self.toggle_ai_panel();
        }
        self.ai_panel.ask_about_block(ctx);
    }

    /// Write the active Block pane's session to disk and tell the user where it
    /// went. Export is a fire-and-forget action with no visible side effect in
    /// the terminal, so a silent failure would be indistinguishable from a dead
    /// palette entry — both outcomes get a dialog.
    pub(crate) fn export_current_session(&self, format: SessionExportFormat) {
        let Some(term_view) = self.current_term_view() else {
            log::debug!("export: no Block pane in the active tab");
            self.show_session_export_result(
                "Session export unavailable",
                "Session export needs a Block-mode pane; the conventional VTE backend keeps no blocks.",
            );
            return;
        };
        match term_view.export_session_to_file(format) {
            Ok(path) => {
                log::info!("Session exported to {}", path.display());
                self.show_session_export_result(
                    "Session exported",
                    &format!("Wrote {}", path.display()),
                );
            }
            Err(error) => {
                log::warn!("Session export failed: {error}");
                self.show_session_export_result(
                    "Session export failed",
                    &format!("Could not write the export: {error}"),
                );
            }
        }
    }

    fn show_session_export_result(&self, heading: &str, message: &str) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(message));
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.present(Some(&self.window));
    }

    pub(crate) fn show_ai_error(&self, message: &str) {
        let message = crate::review_input::safe_multiline_display(message, 16 * 1024);
        let dialog = adw::AlertDialog::new(Some("AI unavailable"), Some(&message));
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.present(Some(&self.window));
    }

    pub(crate) fn focus_current_terminal(&self) {
        if let Some(page) = self.notebook.current_page() {
            if let Some(widget) = self.notebook.nth_page(Some(page)) {
                self.focus_terminal_in_page(&widget);
            }
        }
    }

    /// Focus the active leaf through the recursive typed pane tree.
    pub(crate) fn focus_terminal_in_page(&self, widget: &gtk4::Widget) {
        if let Some(node) = PaneNode::from_widget(widget) {
            node.grab_focus();
        }
    }

    /// Return the page's exact Block controller, when the active leaf uses
    /// block mode. This mirrors `terminal_in_page` so tab activation never has to
    /// discover the live surface by walking through read-only snapshot VTEs.
    pub(crate) fn term_view_in_page(&self, widget: &gtk4::Widget) -> Option<Rc<TermView>> {
        PaneNode::from_widget(widget)
            .and_then(|node| node.active_leaf())
            .and_then(|leaf| leaf.block_view())
    }

    /// Return the page's exact live input surface.
    ///
    /// Block pages contain read-only VTE snapshots in addition to the active
    /// input VTE, so callers must use the typed pane controller rather than
    /// walking the GTK widget tree.
    pub(crate) fn terminal_in_page(&self, widget: &gtk4::Widget) -> Option<Terminal> {
        PaneNode::from_widget(widget).and_then(|node| node.active_terminal())
    }

    pub(crate) fn current_terminal(&self) -> Option<Terminal> {
        self.notebook
            .current_page()
            .and_then(|page_num| self.notebook.nth_page(Some(page_num)))
            .and_then(|widget| self.terminal_in_page(&widget))
    }

    pub(crate) fn current_pane_leaf(&self) -> Option<PaneLeaf> {
        self.notebook
            .current_page()
            .and_then(|page_num| self.notebook.nth_page(Some(page_num)))
            .and_then(|widget| PaneNode::from_widget(&widget))
            .and_then(|node| node.active_leaf())
    }

    pub(crate) fn current_term_view(&self) -> Option<Rc<TermView>> {
        self.current_pane_leaf().and_then(|leaf| leaf.block_view())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ai_panel_width_from_geometry, record_navigation_toast, remote_host_index_for_action,
        restored_ai_panel_position,
    };
    use crate::block_view::RecordNavigationResult;

    #[test]
    fn ai_panel_geometry_preserves_workspace_and_clamps_configured_limits() {
        assert_eq!(restored_ai_panel_position(800, 360), Some(440));
        assert_eq!(restored_ai_panel_position(800, 1200), Some(200));
        assert_eq!(restored_ai_panel_position(800, 100), Some(560));
        assert_eq!(restored_ai_panel_position(440, 360), None);

        assert_eq!(ai_panel_width_from_geometry(800, 440), Some(360));
        assert_eq!(ai_panel_width_from_geometry(2000, 100), Some(1200));
        assert_eq!(ai_panel_width_from_geometry(800, 800), None);
    }

    #[test]
    fn indexed_remote_actions_fail_closed_for_safe_mode_and_missing_hosts() {
        assert_eq!(remote_host_index_for_action(0, 1, false), Ok(0));
        assert_eq!(
            remote_host_index_for_action(1, 1, false),
            Err("That remote host is no longer configured.")
        );
        assert_eq!(
            remote_host_index_for_action(0, 1, true),
            Err("Remote connections are disabled in safe mode.")
        );
    }

    #[test]
    fn unavailable_record_target_has_action_feedback() {
        let message = record_navigation_toast(RecordNavigationResult::LocationUnavailable)
            .expect("an unavailable exact location must not be a silent no-op");
        assert!(message.contains("no retained output snapshot"));
        assert_eq!(
            record_navigation_toast(RecordNavigationResult::Navigated),
            None
        );
        assert_eq!(
            record_navigation_toast(RecordNavigationResult::SnapshotView { record_id: 7 }),
            None,
            "the snapshot dialog, not a toast, answers this result"
        );
    }
}
