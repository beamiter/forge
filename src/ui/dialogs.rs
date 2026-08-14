//! Bounded, accessible dialogs and palettes for remote, history, search, and settings UI.
use adw::prelude::*;
use gtk4::gdk::Key;
use gtk4::gdk::ModifierType;
use gtk4::glib;
use gtk4::pango::FontDescription;
use gtk4::{Adjustment, Label, ListBox, Orientation, Scale, ScrolledWindow};
use gtk4::{EventControllerKey, GestureClick, SearchEntry};
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use vte4::Format;
use vte4::Terminal;
use vte4::TerminalExt;

use super::*;
use crate::block_view::RecordNavigationResult;
use crate::keybindings::Action;
use crate::terminal::open_uri;

/// Rebuilds the Remote Hosts rows from the config after it changes. Held in a
/// cell because the handlers that need to call it (delete confirmations, the
/// add/edit dialog) are created by the closure that does the rebuilding.
type RemoteHostsRefresh = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

/// Which saved host the host dialog is about to overwrite: its index, plus the
/// name it had when the dialog opened. The name is what makes the index safe to
/// act on — the file can be reloaded behind an open dialog, and writing back to
/// a stale index would silently edit a different host.
type RemoteHostEditTarget = (usize, String);

const CROSS_BLOCK_SEARCH_LIMIT: usize = 500;
const CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES: usize = 8 * 1024;
const CROSS_BLOCK_SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
/// A ListBox owns one widget tree per row; unlike ListView it does not recycle
/// off-screen rows. Keep this palette intentionally small until it moves to a
/// virtualized model, regardless of the much larger on-disk retention limit.
const HISTORY_PALETTE_ROW_LIMIT: usize = 500;

fn remote_picker_guard(safe_mode: bool, host_count: usize) -> Result<(), &'static str> {
    if safe_mode {
        Err("Remote connections are disabled in safe mode.")
    } else if host_count == 0 {
        Err("No remote hosts are configured. Add one in Settings → Remote Hosts.")
    } else {
        Ok(())
    }
}

fn cross_block_search_dialog_title() -> &'static str {
    "Search Blocks"
}

fn cross_block_search_idle_status() -> &'static str {
    "Type to search across blocks."
}

fn cross_block_search_pending_status() -> &'static str {
    "Searching blocks…"
}

fn cross_block_search_query_error(query: &str) -> Option<&'static str> {
    (query.len() > CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES)
        .then_some("Query is too long (maximum 8 KiB).")
}

fn cross_block_search_status_for_match_count(total: usize) -> String {
    if total == 0 {
        "No matches.".to_string()
    } else if total == CROSS_BLOCK_SEARCH_LIMIT {
        format!("{CROSS_BLOCK_SEARCH_LIMIT} matches (capped) — refine your query.")
    } else {
        format!("{total} matches")
    }
}

fn cross_block_search_jump_unavailable_status() -> &'static str {
    "This result is searchable, but its exact terminal location is not available yet."
}

/// What the cross-block palette does with one activated hit. Every arm of
/// [`RecordNavigationResult`] resolves to exactly one of these: a record the
/// view could reach, or could only show a snapshot of, always closes the
/// palette; only a record it can do neither for keeps it open with a status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossBlockJumpOutcome {
    Close,
    ShowSnapshot(u64),
    KeepOpen,
}

fn cross_block_jump_outcome(result: RecordNavigationResult) -> CrossBlockJumpOutcome {
    match result {
        RecordNavigationResult::Navigated => CrossBlockJumpOutcome::Close,
        RecordNavigationResult::SnapshotView { record_id } => {
            CrossBlockJumpOutcome::ShowSnapshot(record_id)
        }
        RecordNavigationResult::LocationUnavailable | RecordNavigationResult::NoMatchingRecord => {
            CrossBlockJumpOutcome::KeepOpen
        }
    }
}

fn record_snapshot_dialog_title() -> &'static str {
    "Output Snapshot"
}

fn record_snapshot_unavailable_message() -> &'static str {
    "This record's output snapshot is no longer retained."
}

/// One-line outcome header for the snapshot dialog. Identity and outcome come
/// from the parser-fed completed record, never from a terminal surface.
fn record_snapshot_status_line(view: &crate::block_view::RecordSnapshotView) -> String {
    let mut status = if view.is_background {
        "Background output".to_string()
    } else {
        match view.exit_code {
            Some(code) => format!("Exit code {code}"),
            None => "Exit code unknown (the shell reported none)".to_string(),
        }
    };
    if let Some(duration_ms) = view.duration_ms {
        status.push_str(&format!(
            " · {}",
            crate::block_view::format_block_duration(duration_ms)
        ));
    }
    status
}

fn clear_list_box(list_box: &ListBox) {
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }
}

impl UiState {
    pub(crate) async fn confirm_close_with_processes(
        window: &adw::ApplicationWindow,
        heading: &str,
        close_label: &str,
        process_info: &str,
    ) -> bool {
        let dialog = adw::MessageDialog::builder()
            .heading(heading)
            .body(format!(
                "The following foreground process(es) are still running:\n\n{}\n\nClosing will terminate them.",
                process_info
            ))
            .transient_for(window)
            .modal(true)
            .build();

        dialog.add_response("cancel", "Cancel");
        dialog.add_response("close", close_label);
        dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        dialog.choose_future().await == "close"
    }

    pub(crate) async fn confirm_close_tab_with_process(
        window: &adw::ApplicationWindow,
        process_info: &str,
    ) -> bool {
        Self::confirm_close_with_processes(
            window,
            "Close tab with running process?",
            "Close Tab",
            process_info,
        )
        .await
    }

    pub(crate) fn toggle_sidebar(&self) {
        self.set_sidebar_visible(!self.sidebar.is_visible(), true);
    }

    /// Apply sidebar visibility and optionally persist the user's choice.
    pub(crate) fn set_sidebar_visible(&self, visible: bool, persist: bool) {
        self.sidebar.set_visible(visible);
        if persist {
            self.config.borrow_mut().sidebar_visible = visible;
            self.persist_config();
        }
    }

    pub(crate) fn toggle_command_palette(&self) {
        let dialog_to_close = self.command_palette_dialog.borrow_mut().take();
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            return;
        }

        let bound_actions = self.keybinding_map.borrow().all_bound_actions();
        // Include non-keyboard actions at end
        let extra_hints: &[(&str, &str)] = &[
            ("Double-click tab", "Rename tab"),
            ("Ctrl+Click link", "Open hyperlink"),
        ];

        let dialog = adw::Dialog::builder()
            .title("Command Palette")
            .content_width(480)
            .content_height(480)
            .build();

        let header_bar = adw::HeaderBar::new();
        let filter_entry = SearchEntry::new();
        filter_entry.set_placeholder_text(Some("Search commands..."));
        filter_entry.set_hexpand(true);

        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::Single);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(12);
        list_box.set_margin_end(12);
        list_box.set_margin_bottom(12);

        // Store action data for filtering and execution
        let actions_data: Rc<Vec<(Option<Action>, String, String)>> = Rc::new(
            bound_actions
                .iter()
                .map(|(action, binding)| {
                    (Some(*action), action.name().to_string(), binding.clone())
                })
                .chain(
                    extra_hints
                        .iter()
                        .map(|(shortcut, desc)| (None, desc.to_string(), shortcut.to_string())),
                )
                .collect(),
        );

        for (_, description, binding) in actions_data.iter() {
            let row = adw::ActionRow::builder()
                .title(description.as_str())
                .activatable(true)
                .build();
            if !binding.is_empty() {
                let key_label = Label::new(Some(binding));
                key_label.add_css_class("dim-label");
                row.add_suffix(&key_label);
            }
            list_box.append(&row);
        }

        // Select the first row by default
        if let Some(first_row) = list_box.row_at_index(0) {
            list_box.select_row(Some(&first_row));
        }

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&list_box)
            .build();

        let search_box = gtk4::Box::new(Orientation::Vertical, 0);
        filter_entry.set_margin_start(12);
        filter_entry.set_margin_end(12);
        filter_entry.set_margin_top(8);
        filter_entry.set_margin_bottom(8);
        search_box.append(&filter_entry);
        search_box.append(&scrolled);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&search_box));
        dialog.set_child(Some(&toolbar_view));

        // Filter rows based on search text
        let list_box_for_filter = list_box.clone();
        let actions_data_for_filter = actions_data.clone();
        filter_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_lowercase();
            let mut first_visible: Option<gtk4::ListBoxRow> = None;
            for (idx, (_, desc, binding)) in actions_data_for_filter.iter().enumerate() {
                if let Some(row) = list_box_for_filter.row_at_index(idx as i32) {
                    let visible = query.is_empty()
                        || desc.to_lowercase().contains(&query)
                        || binding.to_lowercase().contains(&query);
                    row.set_visible(visible);
                    if visible && first_visible.is_none() {
                        first_visible = Some(row);
                    }
                }
            }
            // Select first visible row
            if let Some(row) = first_visible {
                list_box_for_filter.select_row(Some(&row));
            }
        });

        // Execute action on row activation (double-click or Enter via row activate)
        let ui_for_activate = self.clone();
        let actions_data_for_activate = actions_data.clone();
        let dialog_for_activate = dialog.clone();
        list_box.connect_row_activated(move |_, row| {
            let idx = row.index() as usize;
            if let Some((Some(action), _, _)) = actions_data_for_activate.get(idx) {
                let action = *action;
                dialog_for_activate.force_close();
                ui_for_activate.execute_action(action);
            }
        });

        // Key controller: Escape to close, Enter to execute selected, up/down to navigate
        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_ref = self.command_palette_dialog.clone();
        let ui_for_key = self.clone();
        let list_box_for_key = list_box.clone();
        let actions_data_for_key = actions_data.clone();
        let dialog_for_key = dialog.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if keyval == Key::Escape
                || (matches!(keyval, Key::P | Key::p)
                    && state.contains(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK))
            {
                let dialog_to_close = dialog_ref.borrow_mut().take();
                if let Some(d) = dialog_to_close {
                    d.force_close();
                }
                return true.into();
            }
            if matches!(keyval, Key::Return | Key::KP_Enter) {
                if let Some(row) = list_box_for_key.selected_row() {
                    let idx = row.index() as usize;
                    if let Some((Some(action), _, _)) = actions_data_for_key.get(idx) {
                        let action = *action;
                        dialog_for_key.force_close();
                        ui_for_key.execute_action(action);
                    }
                }
                return true.into();
            }
            // Up/Down arrow navigate the list while keeping focus on the search entry
            if keyval == Key::Down {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(-1);
                let mut next = current + 1;
                while let Some(row) = list_box_for_key.row_at_index(next) {
                    if row.is_visible() {
                        list_box_for_key.select_row(Some(&row));
                        break;
                    }
                    next += 1;
                }
                return true.into();
            }
            if keyval == Key::Up {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(0);
                let mut prev = current - 1;
                while prev >= 0 {
                    if let Some(row) = list_box_for_key.row_at_index(prev) {
                        if row.is_visible() {
                            list_box_for_key.select_row(Some(&row));
                            break;
                        }
                    }
                    prev -= 1;
                }
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        // Clear tracking when dialog is closed
        let dialog_ref = self.command_palette_dialog.clone();
        dialog.connect_closed(move |_| {
            *dialog_ref.borrow_mut() = None;
        });

        *self.command_palette_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
        filter_entry.grab_focus();
    }

    /// Fuzzy picker over `config.remote_hosts`. Enter / click connects.
    pub(crate) fn show_remote_picker(&self) {
        // Toggle: a second invocation closes an open picker.
        let dialog_to_close = self.remote_picker_dialog.borrow_mut().take();
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            return;
        }

        let hosts: Rc<Vec<crate::config::RemoteHost>> =
            Rc::new(self.config.borrow().remote_hosts.clone());
        if let Err(message) =
            remote_picker_guard(std::env::var_os("FORGE_SAFE_MODE").is_some(), hosts.len())
        {
            log::warn!("[remote] {message}");
            self.toast_overlay.add_toast(adw::Toast::new(message));
            return;
        }

        let dialog = adw::Dialog::builder()
            .title("Connect to Remote Host")
            .content_width(480)
            .content_height(480)
            .build();

        let header_bar = adw::HeaderBar::new();
        let filter_entry = SearchEntry::new();
        filter_entry.set_placeholder_text(Some("Search hosts..."));
        filter_entry.set_hexpand(true);

        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::Single);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(12);
        list_box.set_margin_end(12);
        list_box.set_margin_bottom(12);

        // Searchable haystack per row: "name user@host".
        let haystacks: Rc<Vec<String>> = Rc::new(
            hosts
                .iter()
                .map(|h| {
                    let target = match &h.user {
                        Some(u) => format!("{u}@{}", h.host),
                        None => h.host.clone(),
                    };
                    format!("{} {}", h.name, target).to_lowercase()
                })
                .collect(),
        );

        for h in hosts.iter() {
            let target = match &h.user {
                Some(u) => format!("{u}@{}", h.host),
                None => h.host.clone(),
            };
            let name = crate::review_input::safe_inline_display(&h.name, 256);
            let target = crate::review_input::safe_inline_display(&target, 512);
            let row = adw::ActionRow::builder()
                .title(name.as_str())
                .subtitle(target.as_str())
                .activatable(true)
                .build();
            list_box.append(&row);
        }
        if let Some(first_row) = list_box.row_at_index(0) {
            list_box.select_row(Some(&first_row));
        }

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&list_box)
            .build();

        let search_box = gtk4::Box::new(Orientation::Vertical, 0);
        filter_entry.set_margin_start(12);
        filter_entry.set_margin_end(12);
        filter_entry.set_margin_top(8);
        filter_entry.set_margin_bottom(8);
        search_box.append(&filter_entry);
        search_box.append(&scrolled);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&search_box));
        dialog.set_child(Some(&toolbar_view));

        // Substring filter over the haystack.
        let list_box_for_filter = list_box.clone();
        let haystacks_for_filter = haystacks.clone();
        filter_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_lowercase();
            let mut first_visible: Option<gtk4::ListBoxRow> = None;
            for (idx, hay) in haystacks_for_filter.iter().enumerate() {
                if let Some(row) = list_box_for_filter.row_at_index(idx as i32) {
                    let visible = query.is_empty() || hay.contains(&query);
                    row.set_visible(visible);
                    if visible && first_visible.is_none() {
                        first_visible = Some(row);
                    }
                }
            }
            if let Some(row) = first_visible {
                list_box_for_filter.select_row(Some(&row));
            } else {
                // Do not leave a now-hidden command selected: Enter must never
                // insert a result the filter says does not exist.
                list_box_for_filter.unselect_all();
            }
        });

        let connect = {
            let ui = self.clone();
            let hosts = hosts.clone();
            move |idx: usize| {
                if let Some(h) = hosts.get(idx) {
                    ui.connect_remote(h);
                }
            }
        };

        let connect_for_activate = connect.clone();
        let dialog_for_activate = dialog.clone();
        list_box.connect_row_activated(move |_, row| {
            let idx = row.index() as usize;
            dialog_for_activate.force_close();
            connect_for_activate(idx);
        });

        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_ref = self.remote_picker_dialog.clone();
        let list_box_for_key = list_box.clone();
        let dialog_for_key = dialog.clone();
        let connect_for_key = connect.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, _state| {
            if keyval == Key::Escape {
                let dialog_to_close = dialog_ref.borrow_mut().take();
                if let Some(d) = dialog_to_close {
                    d.force_close();
                }
                return true.into();
            }
            if matches!(keyval, Key::Return | Key::KP_Enter) {
                if let Some(row) = list_box_for_key
                    .selected_row()
                    .filter(|row| row.is_visible())
                {
                    let idx = row.index() as usize;
                    dialog_for_key.force_close();
                    connect_for_key(idx);
                }
                return true.into();
            }
            if keyval == Key::Down {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(-1);
                let mut next = current + 1;
                while let Some(row) = list_box_for_key.row_at_index(next) {
                    if row.is_visible() {
                        list_box_for_key.select_row(Some(&row));
                        break;
                    }
                    next += 1;
                }
                return true.into();
            }
            if keyval == Key::Up {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(0);
                let mut prev = current - 1;
                while prev >= 0 {
                    if let Some(row) = list_box_for_key.row_at_index(prev) {
                        if row.is_visible() {
                            list_box_for_key.select_row(Some(&row));
                            break;
                        }
                    }
                    prev -= 1;
                }
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        let dialog_ref = self.remote_picker_dialog.clone();
        dialog.connect_closed(move |_| {
            *dialog_ref.borrow_mut() = None;
        });

        *self.remote_picker_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
        filter_entry.grab_focus();
    }

    /// Palette over the active Block tab plus the lightweight cross-session
    /// history index. Enter inserts into either backend without auto-running.
    pub(crate) fn show_history_palette(&self) {
        let dialog_to_close = self.history_palette_dialog.borrow_mut().take();
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            return;
        }

        let Some(pane) = self.current_pane_leaf() else {
            log::debug!("[history] no active terminal pane");
            return;
        };
        let mut history = pane
            .block_view()
            // Probe one extra entry so the status line can say that the
            // widget-backed view reached its display budget. The clone itself
            // is bounded before leaving TermView.
            .map(|view| view.command_history_bounded(HISTORY_PALETTE_ROW_LIMIT + 1))
            .unwrap_or_default();
        let mut display_limited = history.len() > HISTORY_PALETTE_ROW_LIMIT;
        let mut history_read_failed = false;
        history.truncate(HISTORY_PALETTE_ROW_LIMIT);
        let mut seen: std::collections::HashSet<String> = history.iter().cloned().collect();
        {
            let config = self.config.borrow();
            if config.command_history_enabled {
                if let Some(path) = config.command_history_path.as_deref() {
                    let configured_limit = config.command_history_max_entries as usize;
                    let read_limit = configured_limit.min(HISTORY_PALETTE_ROW_LIMIT + 1);
                    match crate::command_history::read_recent_with_status(
                        std::path::Path::new(path),
                        read_limit,
                    ) {
                        Ok(recent) => {
                            display_limited |= recent.tail_truncated;
                            let records = recent.records;
                            if records.len() == read_limit && configured_limit > read_limit {
                                display_limited = true;
                            }
                            for record in records {
                                if !seen.insert(record.command.clone()) {
                                    continue;
                                }
                                if history.len() == HISTORY_PALETTE_ROW_LIMIT {
                                    display_limited = true;
                                    break;
                                }
                                history.push(record.command);
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            // A fresh installation has no history file yet.
                            // That is an empty state, not a storage failure.
                        }
                        Err(error) => {
                            history_read_failed = true;
                            let error =
                                crate::review_input::safe_inline_display(&error.to_string(), 512);
                            let toast = adw::Toast::new(&format!(
                                "Command history could not be read: {error}"
                            ));
                            toast.set_timeout(8);
                            self.toast_overlay.add_toast(toast);
                        }
                    }
                }
            }
        }
        let history: Rc<Vec<String>> = Rc::new(history);
        if history.is_empty() {
            log::debug!("[history] no finished commands to show");
            if !history_read_failed {
                let message = if display_limited {
                    "No readable commands in the recent 4 MiB history window."
                } else {
                    "No command history yet."
                };
                let toast = adw::Toast::new(message);
                toast.set_timeout(4);
                self.toast_overlay.add_toast(toast);
            }
            return;
        }

        let dialog = adw::Dialog::builder()
            .title("Command History")
            .content_width(560)
            .content_height(480)
            .build();

        let header_bar = adw::HeaderBar::new();
        let filter_entry = SearchEntry::new();
        filter_entry.set_placeholder_text(Some("Filter history…"));
        filter_entry
            .update_property(&[gtk4::accessible::Property::Label("Filter command history")]);
        filter_entry.set_hexpand(true);

        let initial_status = if display_limited {
            format!("Showing the {HISTORY_PALETTE_ROW_LIMIT} most recent commands (display limit).")
        } else {
            format!("{} recent commands", history.len())
        };
        let status_label = Label::new(Some(&initial_status));
        status_label.add_css_class("dim-label");
        status_label.set_accessible_role(gtk4::AccessibleRole::Status);
        status_label.set_xalign(0.0);
        status_label.set_margin_start(12);
        status_label.set_margin_end(12);
        status_label.set_margin_bottom(6);

        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::Single);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(12);
        list_box.set_margin_end(12);
        list_box.set_margin_bottom(12);

        // Lowercase haystack mirrors the displayed list for substring filter.
        let haystacks: Rc<Vec<String>> =
            Rc::new(history.iter().map(|c| c.to_lowercase()).collect());

        for cmd in history.iter() {
            // Long commands wrap inside the row; keep the first line as the
            // title so the palette stays scannable.
            let first_line = cmd.lines().next().unwrap_or(cmd);
            let row = adw::ActionRow::builder()
                .title(first_line)
                .activatable(true)
                .build();
            list_box.append(&row);
        }
        if let Some(first_row) = list_box.row_at_index(0) {
            list_box.select_row(Some(&first_row));
        }

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&list_box)
            .build();

        let search_box = gtk4::Box::new(Orientation::Vertical, 0);
        filter_entry.set_margin_start(12);
        filter_entry.set_margin_end(12);
        filter_entry.set_margin_top(8);
        filter_entry.set_margin_bottom(8);
        search_box.append(&filter_entry);
        search_box.append(&status_label);
        search_box.append(&scrolled);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&search_box));
        dialog.set_child(Some(&toolbar_view));

        let list_box_for_filter = list_box.clone();
        let haystacks_for_filter = haystacks.clone();
        let status_for_filter = status_label.clone();
        filter_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_lowercase();
            let mut first_visible: Option<gtk4::ListBoxRow> = None;
            let mut visible_count = 0usize;
            for (idx, hay) in haystacks_for_filter.iter().enumerate() {
                if let Some(row) = list_box_for_filter.row_at_index(idx as i32) {
                    let visible = query.is_empty() || hay.contains(&query);
                    row.set_visible(visible);
                    if visible && first_visible.is_none() {
                        first_visible = Some(row);
                    }
                    visible_count += usize::from(visible);
                }
            }
            if let Some(row) = first_visible {
                list_box_for_filter.select_row(Some(&row));
            } else {
                // Do not leave a now-hidden command selected: Enter must never
                // insert a result the filter says does not exist.
                list_box_for_filter.unselect_all();
            }
            if query.is_empty() {
                let status = if display_limited {
                    format!(
                        "Showing the {HISTORY_PALETTE_ROW_LIMIT} most recent commands (display limit)."
                    )
                } else {
                    format!("{} recent commands", haystacks_for_filter.len())
                };
                status_for_filter.set_text(&status);
            } else if visible_count == 0 {
                if display_limited {
                    status_for_filter.set_text(&format!(
                        "No matches within the {HISTORY_PALETTE_ROW_LIMIT} most recent commands (display limit)."
                    ));
                } else {
                    status_for_filter.set_text("No matching commands.");
                }
            } else {
                let status = if display_limited {
                    format!(
                        "{visible_count} matches within the {HISTORY_PALETTE_ROW_LIMIT} most recent commands (display limit)."
                    )
                } else {
                    format!("{visible_count} matching commands")
                };
                status_for_filter.set_text(&status);
            }
        });

        // Paste the selected command into the live VTE. Does NOT append a
        // trailing newline — user reviews/edits, then presses Enter — which
        // matches how bash's reverse-i-search behaves.
        let paste = {
            let history = history.clone();
            let pane = pane.clone();
            let ui = self.clone();
            move |idx: usize| {
                if let Some(cmd) = history.get(idx) {
                    ui.insert_review_text(&pane, cmd);
                }
            }
        };

        let paste_for_activate = paste.clone();
        let dialog_for_activate = dialog.clone();
        list_box.connect_row_activated(move |_, row| {
            let idx = row.index() as usize;
            dialog_for_activate.force_close();
            paste_for_activate(idx);
        });

        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_ref = self.history_palette_dialog.clone();
        let list_box_for_key = list_box.clone();
        let dialog_for_key = dialog.clone();
        let paste_for_key = paste.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            // Escape, or the same chord that opened the palette, closes it.
            if keyval == Key::Escape
                || (matches!(keyval, Key::H | Key::h)
                    && state.contains(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK))
            {
                let dialog_to_close = dialog_ref.borrow_mut().take();
                if let Some(d) = dialog_to_close {
                    d.force_close();
                }
                return true.into();
            }
            if matches!(keyval, Key::Return | Key::KP_Enter) {
                if let Some(row) = list_box_for_key
                    .selected_row()
                    .filter(|row| row.is_visible())
                {
                    let idx = row.index() as usize;
                    dialog_for_key.force_close();
                    paste_for_key(idx);
                }
                return true.into();
            }
            if keyval == Key::Down {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(-1);
                let mut next = current + 1;
                while let Some(row) = list_box_for_key.row_at_index(next) {
                    if row.is_visible() {
                        list_box_for_key.select_row(Some(&row));
                        break;
                    }
                    next += 1;
                }
                return true.into();
            }
            if keyval == Key::Up {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(0);
                let mut prev = current - 1;
                while prev >= 0 {
                    if let Some(row) = list_box_for_key.row_at_index(prev) {
                        if row.is_visible() {
                            list_box_for_key.select_row(Some(&row));
                            break;
                        }
                    }
                    prev -= 1;
                }
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        let dialog_ref = self.history_palette_dialog.clone();
        dialog.connect_closed(move |_| {
            *dialog_ref.borrow_mut() = None;
        });

        *self.history_palette_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
        filter_entry.grab_focus();
    }

    /// Cross-block search palette. Debounced search-as-you-type over every
    /// finished block's command line + cached ANSI-stripped output; each hit
    /// gets a flat row (cmd preview as title, "Lnn: snippet" as subtitle).
    /// Enter scrolls the target block into view and lights its VTE search
    /// highlighter on the chord-shifted hit so the user can step further with
    /// the existing find-next chord.
    ///
    /// Default mode is case-insensitive substring; ".*" toggle switches to
    /// regex. Hit count is capped at 500 to keep the palette responsive on
    /// massive scrollbacks (`cargo build` etc.).
    pub(crate) fn show_cross_block_search(&self) {
        let dialog_to_close = self.cross_block_search_dialog.borrow_mut().take();
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            return;
        }

        let Some(term_view) = self.current_term_view() else {
            log::debug!("[xsearch] no active block-mode tab");
            return;
        };

        let dialog = adw::Dialog::builder()
            .title(cross_block_search_dialog_title())
            .content_width(720)
            .content_height(520)
            .build();

        let header_bar = adw::HeaderBar::new();
        let regex_toggle = gtk4::ToggleButton::builder()
            .label(".*")
            .tooltip_text("Treat the query as a regular expression")
            .build();
        header_bar.pack_end(&regex_toggle);

        let filter_entry = SearchEntry::new();
        filter_entry.set_placeholder_text(Some("Search across blocks…"));
        filter_entry.set_hexpand(true);

        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::Single);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(12);
        list_box.set_margin_end(12);
        list_box.set_margin_bottom(12);

        let status_label = Label::new(None);
        status_label.add_css_class("dim-label");
        status_label.set_accessible_role(gtk4::AccessibleRole::Status);
        status_label.set_xalign(0.0);
        status_label.set_margin_start(12);
        status_label.set_margin_end(12);
        status_label.set_margin_bottom(6);

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&list_box)
            .build();

        let search_box = gtk4::Box::new(Orientation::Vertical, 0);
        filter_entry.set_margin_start(12);
        filter_entry.set_margin_end(12);
        filter_entry.set_margin_top(8);
        filter_entry.set_margin_bottom(8);
        search_box.append(&filter_entry);
        search_box.append(&status_label);
        search_box.append(&scrolled);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&search_box));
        dialog.set_child(Some(&toolbar_view));

        // Hits live in a RefCell so both the live-filter closure and the
        // activation closure see the current pass; rebuilt on every
        // keystroke / regex-toggle change.
        let hits: Rc<RefCell<Vec<crate::block_view::CrossBlockHit>>> =
            Rc::new(RefCell::new(Vec::new()));
        let pending_rebuild: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let search_generation = Rc::new(Cell::new(0u64));

        let rebuild = {
            let term_view = term_view.clone();
            let list_box = list_box.clone();
            let hits = hits.clone();
            let status_label = status_label.clone();
            let filter_entry = filter_entry.clone();
            let regex_toggle = regex_toggle.clone();
            Rc::new(move || {
                let query = filter_entry.text().to_string();
                let is_regex = regex_toggle.is_active();

                clear_list_box(&list_box);
                if query.is_empty() {
                    hits.borrow_mut().clear();
                    status_label.set_text(cross_block_search_idle_status());
                    return;
                }
                if let Some(message) = cross_block_search_query_error(&query) {
                    hits.borrow_mut().clear();
                    status_label.set_text(message);
                    return;
                }

                match term_view.cross_block_search(&query, is_regex, CROSS_BLOCK_SEARCH_LIMIT) {
                    Ok(results) => {
                        let total = results.len();
                        status_label.set_text(&cross_block_search_status_for_match_count(total));
                        for hit in results.iter() {
                            let can_jump =
                                term_view.can_jump_to_record(hit.block_id, hit.is_output);
                            let surface = if hit.is_output { "out" } else { "cmd" };
                            let mut subtitle = format!(
                                "{surface} L{}: {}",
                                hit.line_no,
                                glib::markup_escape_text(&hit.line_text)
                            );
                            if !can_jump {
                                subtitle.push_str(" — location unavailable");
                            }
                            let row = adw::ActionRow::builder()
                                .title(glib::markup_escape_text(&hit.cmd_preview).as_str())
                                .subtitle(&subtitle)
                                .activatable(can_jump)
                                .build();
                            list_box.append(&row);
                        }
                        *hits.borrow_mut() = results;
                        if let Some(first_row) = list_box.row_at_index(0) {
                            list_box.select_row(Some(&first_row));
                        }
                    }
                    Err(e) => {
                        hits.borrow_mut().clear();
                        clear_list_box(&list_box);
                        status_label.set_text(&format!("Bad regex: {e}"));
                    }
                }
            })
        };

        let schedule_rebuild = {
            let pending_rebuild = pending_rebuild.clone();
            let search_generation = search_generation.clone();
            let rebuild = rebuild.clone();
            let hits = hits.clone();
            let list_box = list_box.clone();
            let status_label = status_label.clone();
            let filter_entry = filter_entry.clone();
            Rc::new(move || {
                let generation = search_generation.get().wrapping_add(1);
                search_generation.set(generation);
                if let Some(source) = pending_rebuild.borrow_mut().take() {
                    source.remove();
                }

                clear_list_box(&list_box);
                hits.borrow_mut().clear();
                if filter_entry.text().is_empty() {
                    status_label.set_text(cross_block_search_idle_status());
                    return;
                }
                if let Some(message) = cross_block_search_query_error(filter_entry.text().as_str())
                {
                    status_label.set_text(message);
                    return;
                }
                status_label.set_text(cross_block_search_pending_status());

                let pending_rebuild = pending_rebuild.clone();
                let search_generation = search_generation.clone();
                let rebuild = rebuild.clone();
                let pending_rebuild_slot = pending_rebuild.clone();
                let pending_rebuild_clear = pending_rebuild.clone();
                let source = glib::timeout_add_local(CROSS_BLOCK_SEARCH_DEBOUNCE, move || {
                    if search_generation.get() == generation {
                        rebuild();
                        // Only the current generation owns the stored source.
                        // A stale callback must never clear a newer timeout.
                        pending_rebuild_clear.borrow_mut().take();
                    }
                    glib::ControlFlow::Break
                });
                *pending_rebuild_slot.borrow_mut() = Some(source);
            })
        };

        // Initial state.
        status_label.set_text(cross_block_search_idle_status());

        let rebuild_for_change = schedule_rebuild.clone();
        filter_entry.connect_search_changed(move |_| {
            rebuild_for_change();
        });

        let rebuild_for_toggle = schedule_rebuild.clone();
        regex_toggle.connect_toggled(move |_| {
            rebuild_for_toggle();
        });

        // Jump-to-hit: take the target record's best available surface AND
        // turn on its per-VTE search highlight at the matching hit. Closes the
        // palette so the user lands on the record they picked.
        let jump = {
            let term_view = term_view.clone();
            let hits = hits.clone();
            let filter_entry = filter_entry.clone();
            let regex_toggle = regex_toggle.clone();
            let status_label = status_label.clone();
            move |idx: usize| -> CrossBlockJumpOutcome {
                let pattern = filter_entry.text().to_string();
                let is_regex = regex_toggle.is_active();
                let hit = match hits.borrow().get(idx) {
                    Some(h) => h.clone(),
                    None => return CrossBlockJumpOutcome::KeepOpen,
                };
                let outcome = cross_block_jump_outcome(
                    term_view.navigate_to_record_id(hit.block_id, hit.is_output),
                );
                match outcome {
                    CrossBlockJumpOutcome::Close => {
                        // The surface has already scrolled and taken focus. A
                        // highlight that cannot be set is not a reason to
                        // strand this modal over it.
                        term_view.focus_match_in_block(
                            hit.block_id,
                            &pattern,
                            is_regex,
                            hit.is_output,
                        );
                    }
                    CrossBlockJumpOutcome::KeepOpen => {
                        status_label.set_text(cross_block_search_jump_unavailable_status());
                    }
                    CrossBlockJumpOutcome::ShowSnapshot(_) => {}
                }
                outcome
            }
        };

        // The snapshot view replaces this palette rather than stacking over
        // it, so the close always precedes the present.
        let apply_jump_outcome = {
            let ui = self.clone();
            let dialog = dialog.clone();
            Rc::new(move |outcome: CrossBlockJumpOutcome| match outcome {
                CrossBlockJumpOutcome::Close => dialog.force_close(),
                CrossBlockJumpOutcome::ShowSnapshot(record_id) => {
                    dialog.force_close();
                    ui.show_record_snapshot_dialog(record_id);
                }
                CrossBlockJumpOutcome::KeepOpen => {}
            })
        };

        let jump_for_activate = jump.clone();
        let apply_for_activate = apply_jump_outcome.clone();
        list_box.connect_row_activated(move |_, row| {
            let idx = row.index() as usize;
            apply_for_activate(jump_for_activate(idx));
        });

        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_ref = self.cross_block_search_dialog.clone();
        let list_box_for_key = list_box.clone();
        let jump_for_key = jump.clone();
        let apply_for_key = apply_jump_outcome.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if keyval == Key::Escape
                || (matches!(keyval, Key::G | Key::g)
                    && state.contains(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK))
            {
                let dialog_to_close = dialog_ref.borrow_mut().take();
                if let Some(d) = dialog_to_close {
                    d.force_close();
                }
                return true.into();
            }
            if matches!(keyval, Key::Return | Key::KP_Enter) {
                if let Some(row) = list_box_for_key.selected_row() {
                    let idx = row.index() as usize;
                    apply_for_key(jump_for_key(idx));
                }
                return true.into();
            }
            if keyval == Key::Down {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(-1);
                let mut next = current + 1;
                while let Some(row) = list_box_for_key.row_at_index(next) {
                    if row.is_visible() {
                        list_box_for_key.select_row(Some(&row));
                        break;
                    }
                    next += 1;
                }
                return true.into();
            }
            if keyval == Key::Up {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(0);
                let mut prev = current - 1;
                while prev >= 0 {
                    if let Some(row) = list_box_for_key.row_at_index(prev) {
                        if row.is_visible() {
                            list_box_for_key.select_row(Some(&row));
                            break;
                        }
                    }
                    prev -= 1;
                }
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        let dialog_ref = self.cross_block_search_dialog.clone();
        let pending_rebuild_for_close = pending_rebuild.clone();
        dialog.connect_closed(move |_| {
            if let Some(source) = pending_rebuild_for_close.borrow_mut().take() {
                source.remove();
            }
            *dialog_ref.borrow_mut() = None;
        });

        *self.cross_block_search_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
        filter_entry.grab_focus();
    }

    pub(crate) fn toggle_debug_dashboard(&self) {
        let dialog_to_close = self.debug_dashboard_dialog.borrow_mut().take();
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            return;
        }

        let dialog = adw::Dialog::builder()
            .title("Debug Dashboard")
            .content_width(480)
            .content_height(560)
            .build();

        let header_bar = adw::HeaderBar::new();
        let refresh_btn = gtk4::Button::from_icon_name("view-refresh-symbolic");
        refresh_btn.set_tooltip_text(Some("Refresh"));
        refresh_btn.update_property(&[gtk4::accessible::Property::Label(
            "Refresh debug information",
        )]);
        header_bar.pack_start(&refresh_btn);

        let content = gtk4::Box::new(Orientation::Vertical, 18);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);

        // Populate application/session diagnostics plus the active Block
        // backend's PTY/viewport snapshot. The app-level sections remain useful
        // in conventional VTE mode instead of presenting an empty dashboard.
        let ui_for_populate = self.clone();
        let populate = Rc::new(move |content: &gtk4::Box| {
            while let Some(child) = content.first_child() {
                content.remove(&child);
            }
            let tab_count = ui_for_populate.notebook.n_pages();
            let total_panes: usize = (0..tab_count)
                .filter_map(|index| ui_for_populate.notebook.nth_page(Some(index)))
                .map(|page| {
                    PaneNode::from_widget(&page)
                        .map(|node| node.leaves().len())
                        .unwrap_or(1)
                })
                .sum();
            let active_page = ui_for_populate.notebook.current_page();
            let active_widget =
                active_page.and_then(|index| ui_for_populate.notebook.nth_page(Some(index)));
            let active_title = active_widget
                .as_ref()
                .and_then(|page| crate::state::tab_label_text(&ui_for_populate.notebook, page))
                .unwrap_or_default();
            let active_panes = active_widget
                .as_ref()
                .and_then(PaneNode::from_widget)
                .map(|node| node.leaves().len())
                .unwrap_or(0);
            let config = ui_for_populate.config.borrow();
            let mut sections = vec![
                (
                    "Session".to_string(),
                    vec![
                        ("Tabs".to_string(), tab_count.to_string()),
                        ("Total panes".to_string(), total_panes.to_string()),
                        ("Active tab".to_string(), active_title),
                        ("Panes in active tab".to_string(), active_panes.to_string()),
                        (
                            "Zoomed".to_string(),
                            ui_for_populate.zoom_state.borrow().is_some().to_string(),
                        ),
                    ],
                ),
                (
                    "Appearance".to_string(),
                    vec![
                        ("Theme".to_string(), config.theme_name.clone()),
                        ("Font".to_string(), config.font_desc.clone()),
                        (
                            "Font scale".to_string(),
                            format!("{:.3}", ui_for_populate.font_scale.get()),
                        ),
                        (
                            "Opacity".to_string(),
                            format!("{:.2}", ui_for_populate.window_opacity.get()),
                        ),
                        (
                            "Terminal mode".to_string(),
                            config.terminal_mode.as_str().to_string(),
                        ),
                        (
                            "Scrollback".to_string(),
                            config.terminal_scrollback_lines.to_string(),
                        ),
                    ],
                ),
                (
                    "Config".to_string(),
                    vec![
                        (
                            "Keybindings".to_string(),
                            ui_for_populate
                                .keybinding_map
                                .borrow()
                                .bindings
                                .len()
                                .to_string(),
                        ),
                        (
                            "Remote hosts".to_string(),
                            config.remote_hosts.len().to_string(),
                        ),
                        (
                            "Startup commands".to_string(),
                            config.startup_commands.clone().unwrap_or_default(),
                        ),
                    ],
                ),
            ];
            drop(config);
            if let Some(term_view) = ui_for_populate.current_term_view() {
                sections.extend(
                    term_view
                        .debug_info()
                        .into_iter()
                        .map(|(section, rows)| (format!("Block · {section}"), rows)),
                );
            } else {
                sections.push((
                    "Backend".to_string(),
                    vec![(
                        "Block diagnostics".to_string(),
                        "not available for a VTE pane".to_string(),
                    )],
                ));
            };
            for (section, rows) in sections {
                let group = adw::PreferencesGroup::new();
                group.set_title(&section);
                for (key, value) in rows {
                    let row = adw::ActionRow::builder().title(key.as_str()).build();
                    let value_label = Label::new(Some(&value));
                    value_label.add_css_class("dim-label");
                    value_label.set_selectable(true);
                    value_label.set_xalign(1.0);
                    row.add_suffix(&value_label);
                    group.add(&row);
                }
                content.append(&group);
            }
        });
        populate(&content);

        let content_for_refresh = content.clone();
        let populate_for_refresh = populate.clone();
        refresh_btn.connect_clicked(move |_| {
            populate_for_refresh(&content_for_refresh);
        });

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&content)
            .build();

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&scrolled));
        dialog.set_child(Some(&toolbar_view));

        // Escape or F12 closes the dashboard.
        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_ref = self.debug_dashboard_dialog.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == Key::Escape || keyval == Key::F12 {
                let dialog_to_close = dialog_ref.borrow_mut().take();
                if let Some(d) = dialog_to_close {
                    d.force_close();
                }
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        let dialog_ref = self.debug_dashboard_dialog.clone();
        dialog.connect_closed(move |_| {
            *dialog_ref.borrow_mut() = None;
        });

        *self.debug_dashboard_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
    }

    pub(crate) fn toggle_settings_panel(&self) {
        let dialog_to_close = self.settings_dialog.borrow_mut().take();
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            return;
        }

        let dialog = adw::PreferencesDialog::new();
        dialog.set_title("Settings");

        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title("Appearance");

        let config = self.config.borrow();

        // --- Theme ---
        let theme_names: Vec<String> = self
            .available_themes
            .iter()
            .map(|t| t.name.clone())
            .collect();
        let theme_model =
            gtk4::StringList::new(&theme_names.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let theme_row = adw::ComboRow::builder()
            .title("Theme")
            .model(&theme_model)
            .build();
        let current_theme_idx = self
            .available_themes
            .iter()
            .position(|t| t.name == config.theme_name)
            .unwrap_or(0);
        theme_row.set_selected(current_theme_idx as u32);
        group.add(&theme_row);

        // --- Font (monospace fonts from Pango) ---
        let pango_ctx = self.window.pango_context();
        let families = pango_ctx.list_families();
        let mut mono_fonts: Vec<String> = families
            .iter()
            .filter(|f| f.is_monospace())
            .map(|f| f.name().to_string())
            .collect();
        mono_fonts.sort_by_key(|a| a.to_lowercase());

        let current_font_desc = FontDescription::from_string(&config.font_desc);
        let current_family = current_font_desc
            .family()
            .map(|f| f.to_string())
            .unwrap_or_default();

        let font_model =
            gtk4::StringList::new(&mono_fonts.iter().map(|s| s.as_str()).collect::<Vec<_>>());
        let font_row = adw::ComboRow::builder()
            .title("Font")
            .model(&font_model)
            .build();
        let current_font_idx = mono_fonts
            .iter()
            .position(|f| f == &current_family)
            .unwrap_or(0);
        font_row.set_selected(current_font_idx as u32);
        group.add(&font_row);

        // --- Font Size ---
        let current_size = current_font_desc.size() as f64 / gtk4::pango::SCALE as f64;
        let font_size_adj = Adjustment::new(current_size, 6.0, 72.0, 1.0, 4.0, 0.0);
        let font_size_row = adw::SpinRow::new(Some(&font_size_adj), 1.0, 0);
        font_size_row.set_title("Font Size");
        group.add(&font_size_row);

        // --- Font Scale ---
        let font_scale_adj = Adjustment::new(self.font_scale.get(), 0.1, 10.0, 0.025, 0.1, 0.0);
        let font_scale_row = adw::SpinRow::new(Some(&font_scale_adj), 0.025, 3);
        font_scale_row.set_title("Font Scale");
        group.add(&font_scale_row);

        // --- Opacity ---
        let opacity_row = adw::ActionRow::builder().title("Opacity").build();
        let opacity_scale = Scale::with_range(Orientation::Horizontal, 0.01, 1.0, 0.025);
        opacity_scale.set_value(self.window_opacity.get());
        opacity_scale.set_hexpand(true);
        opacity_scale.set_draw_value(true);
        opacity_scale.set_value_pos(gtk4::PositionType::Left);
        opacity_scale.set_format_value_func(|_, value| format!("{:.0}%", value * 100.0));
        opacity_row.add_suffix(&opacity_scale);
        group.add(&opacity_row);

        // --- Scrollback ---
        let scrollback_adj = Adjustment::new(
            config.terminal_scrollback_lines as f64,
            0.0,
            1_000_000.0,
            100.0,
            1000.0,
            0.0,
        );
        let scrollback_row = adw::SpinRow::new(Some(&scrollback_adj), 100.0, 0);
        scrollback_row.set_title("Scrollback Lines");
        group.add(&scrollback_row);

        let terminal_group = adw::PreferencesGroup::new();
        terminal_group.set_title("Terminal & Blocks");
        let terminal_mode_model =
            gtk4::StringList::new(&["Block", "VTE compatibility", "Unified (experimental)"]);
        let terminal_mode_row = adw::ComboRow::builder()
            .title("Terminal Backend")
            .subtitle("Applies to new and restored local panes")
            .model(&terminal_mode_model)
            .selected(match config.terminal_mode {
                crate::config::TerminalMode::Block => 0,
                crate::config::TerminalMode::Vte => 1,
                crate::config::TerminalMode::Unified => 2,
            })
            .build();
        let safe_mode = std::env::var_os("FORGE_SAFE_MODE").is_some();
        terminal_mode_row.set_sensitive(!safe_mode);
        terminal_group.add(&terminal_mode_row);

        let block_compact_row = adw::SwitchRow::builder()
            .title("Compact Block Layout")
            .subtitle("Use denser spacing in new Block panes")
            .active(config.block_compact)
            .build();
        block_compact_row.set_sensitive(!safe_mode);
        terminal_group.add(&block_compact_row);

        let command_history_row = adw::SwitchRow::builder()
            .title("Command History Index")
            .subtitle("Store commands, cwd and status; never terminal output")
            .active(config.command_history_enabled)
            .build();
        command_history_row.set_sensitive(!safe_mode);
        terminal_group.add(&command_history_row);

        let ascii_organism_row = adw::SwitchRow::builder()
            .title("ASCII Organism")
            .subtitle("Show the local, no-LLM organism in new Block panes")
            .active(config.ascii_organism_enabled)
            .build();
        ascii_organism_row.set_sensitive(!safe_mode);
        terminal_group.add(&ascii_organism_row);

        let ascii_organism_motion_model =
            gtk4::StringList::new(&["Automatic", "Full", "Calm", "Static"]);
        let ascii_organism_motion_row = adw::ComboRow::builder()
            .title("Organism Motion")
            .subtitle("Automatic follows the desktop animation preference")
            .model(&ascii_organism_motion_model)
            .selected(match config.ascii_organism_motion {
                None => 0,
                Some(crate::config::OrganismMotion::Full) => 1,
                Some(crate::config::OrganismMotion::Calm) => 2,
                Some(crate::config::OrganismMotion::Static) => 3,
            })
            .build();
        ascii_organism_motion_row.set_sensitive(!safe_mode && config.ascii_organism_enabled);
        terminal_group.add(&ascii_organism_motion_row);

        let privacy_group = adw::PreferencesGroup::new();
        privacy_group.set_title("Features & Privacy");
        let notifications_row = adw::SwitchRow::builder()
            .title("Long-command Notifications")
            .active(config.notify_long_blocks)
            .build();
        notifications_row.set_sensitive(!safe_mode);
        privacy_group.add(&notifications_row);

        let remote_clipboard_row = adw::SwitchRow::builder()
            .title("Allow OSC 52 Clipboard Writes")
            .subtitle("Enable only for trusted local and remote programs")
            .active(config.allow_remote_clipboard_write)
            .build();
        remote_clipboard_row.set_sensitive(!safe_mode);
        privacy_group.add(&remote_clipboard_row);

        let ai_group = adw::PreferencesGroup::new();
        ai_group.set_title("AI & Agent");
        ai_group.set_description(Some(
            "Environment variables take priority. Keys entered here are stored in a private ai.key file, never in config.toml",
        ));
        let ai_enabled_row = adw::SwitchRow::builder()
            .title("Enable AI Features")
            .active(config.ai_enabled)
            .build();
        ai_enabled_row.set_sensitive(!safe_mode);
        ai_group.add(&ai_enabled_row);

        let ai_panel_visible_row = adw::SwitchRow::builder()
            .title("Show AI Chats at Startup")
            .subtitle("Keep the persistent right-side chat panel open")
            .active(config.ai_panel_visible)
            .build();
        ai_panel_visible_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&ai_panel_visible_row);

        let ai_panel_width_adj =
            Adjustment::new(config.ai_panel_width as f64, 240.0, 1200.0, 10.0, 50.0, 0.0);
        let ai_panel_width_row = adw::SpinRow::new(Some(&ai_panel_width_adj), 10.0, 0);
        ai_panel_width_row.set_title("AI Chats Width");
        ai_panel_width_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&ai_panel_width_row);

        let agent_enabled_row = adw::SwitchRow::builder()
            .title("Enable Approval-gated Agent")
            .subtitle("Every proposed command remains editable and requires approval")
            .active(config.agent_enabled)
            .build();
        agent_enabled_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&agent_enabled_row);

        let agent_auto_row = adw::SwitchRow::builder()
            .title("Automatic Agent Execution Retired")
            .subtitle(
                "Every proposal requires explicit approval; command text cannot prove what aliases, helpers, or flags will execute",
            )
            .active(false)
            .build();
        agent_auto_row.set_sensitive(false);
        ai_group.add(&agent_auto_row);

        let correction_enabled_row = adw::SwitchRow::builder()
            .title("Correct Mistyped Block Commands")
            .subtitle(
                "Offer an editable correction after typo-like failures; never run automatically",
            )
            .active(config.command_correction_enabled)
            .build();
        correction_enabled_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&correction_enabled_row);

        let provider_model = gtk4::StringList::new(&["Anthropic", "OpenAI-compatible", "Ollama"]);
        let provider_row = adw::ComboRow::builder()
            .title("Provider")
            .model(&provider_model)
            .selected(match config.ai_provider.as_str() {
                "openai-compatible" => 1,
                "ollama" => 2,
                _ => 0,
            })
            .build();
        provider_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&provider_row);

        let model_row = adw::EntryRow::new();
        model_row.set_title("Model");
        model_row.set_text(&config.ai_model);
        model_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&model_row);

        let base_url_row = adw::EntryRow::new();
        base_url_row.set_title("Base URL");
        base_url_row.set_text(&config.ai_base_url);
        base_url_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&base_url_row);

        let api_key_row = adw::PasswordEntryRow::builder()
            .title("API Key — enter a new value and press Apply")
            .show_apply_button(true)
            .build();
        api_key_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&api_key_row);

        let max_tokens_adj = Adjustment::new(
            config.ai_max_tokens as f64,
            64.0,
            32_768.0,
            64.0,
            512.0,
            0.0,
        );
        let max_tokens_row = adw::SpinRow::new(Some(&max_tokens_adj), 64.0, 0);
        max_tokens_row.set_title("Maximum Response Tokens");
        max_tokens_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&max_tokens_row);

        let agent_turns_adj =
            Adjustment::new(config.agent_max_turns as f64, 1.0, 100.0, 1.0, 5.0, 0.0);
        let agent_turns_row = adw::SpinRow::new(Some(&agent_turns_adj), 1.0, 0);
        agent_turns_row.set_title("Agent Turn Limit");
        agent_turns_row.set_sensitive(!safe_mode && config.ai_enabled && config.agent_enabled);
        ai_group.add(&agent_turns_row);

        let stream_row = adw::SwitchRow::builder()
            .title("Stream Chat Responses")
            .subtitle("Show AI chat replies incrementally while they are generated")
            .active(config.ai_stream)
            .build();
        stream_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&stream_row);

        let redact_row = adw::SwitchRow::builder()
            .title("Redact Common Secrets")
            .subtitle("Apply before terminal context is sent to a provider")
            .active(config.ai_redact_secrets)
            .build();
        redact_row.set_sensitive(!safe_mode && config.ai_enabled);
        ai_group.add(&redact_row);

        let remote_group = adw::PreferencesGroup::new();
        remote_group.set_title("Remote Hosts");
        remote_group.set_description(Some(
            "Targets for the Ctrl+Shift+S picker. Advanced fields (ssh_args, session, deploy_artifact) are edited in config.toml",
        ));
        let add_host_btn = gtk4::Button::from_icon_name("list-add-symbolic");
        add_host_btn.set_tooltip_text(Some("Add Remote Host"));
        add_host_btn.update_property(&[gtk4::accessible::Property::Label("Add remote host")]);
        add_host_btn.add_css_class("flat");
        add_host_btn.set_valign(gtk4::Align::Center);
        remote_group.set_header_suffix(Some(&add_host_btn));
        remote_group.set_sensitive(!safe_mode);

        page.add(&group);
        page.add(&terminal_group);
        page.add(&privacy_group);
        page.add(&ai_group);
        page.add(&remote_group);
        dialog.add(&page);

        drop(config);

        // --- Signal: Theme ---
        let ui = self.clone();
        let themes = self.available_themes.clone();
        theme_row.connect_notify_local(Some("selected"), move |row, _| {
            let idx = row.selected() as usize;
            if let Some(theme) = themes.get(idx) {
                ui.apply_theme(theme);
                ui.persist_config();
            }
        });

        // --- Signal: Font ---
        let ui = self.clone();
        let mono_fonts_clone = mono_fonts.clone();
        let font_size_row_clone = font_size_row.clone();
        font_row.connect_notify_local(Some("selected"), move |row, _| {
            let idx = row.selected() as usize;
            if let Some(family) = mono_fonts_clone.get(idx) {
                let size = font_size_row_clone.value() as i32;
                let new_desc = format!("{} {}", family, size);
                ui.config.borrow_mut().font_desc = new_desc;
                ui.apply_font_all();
                ui.persist_config();
            }
        });

        // --- Signal: Font Size ---
        let ui = self.clone();
        let mono_fonts_clone2 = mono_fonts;
        let font_row_clone = font_row.clone();
        font_size_row.connect_notify_local(Some("value"), move |row, _| {
            let idx = font_row_clone.selected() as usize;
            let family = mono_fonts_clone2
                .get(idx)
                .map(|s| s.as_str())
                .unwrap_or("Monospace");
            let size = row.value() as i32;
            let new_desc = format!("{} {}", family, size);
            ui.config.borrow_mut().font_desc = new_desc;
            ui.apply_font_all();
            ui.persist_config();
        });

        // --- Signal: Font Scale ---
        let ui = self.clone();
        font_scale_row.connect_notify_local(Some("value"), move |row, _| {
            ui.apply_font_scale(row.value());
        });

        // --- Signal: Opacity ---
        let ui = self.clone();
        opacity_scale.connect_value_changed(move |scale| {
            let val = scale.value();
            ui.window_opacity.set(val);
            ui.window.set_opacity(val);
            ui.config.borrow_mut().window_opacity = val;
            ui.persist_config();
        });

        // --- Signal: Scrollback ---
        let ui = self.clone();
        scrollback_row.connect_notify_local(Some("value"), move |row, _| {
            let val = row.value() as u32;
            ui.config.borrow_mut().terminal_scrollback_lines = val;
            ui.apply_scrollback_all();
            ui.persist_config();
        });

        let ui = self.clone();
        block_compact_row.connect_active_notify(move |row| {
            ui.config.borrow_mut().block_compact = row.is_active();
            ui.sync_block_configs();
            ui.persist_config();
        });

        let ui = self.clone();
        terminal_mode_row.connect_selected_notify(move |row| {
            ui.config.borrow_mut().terminal_mode = match row.selected() {
                0 => crate::config::TerminalMode::Block,
                2 => crate::config::TerminalMode::Unified,
                _ => crate::config::TerminalMode::Vte,
            };
            ui.persist_config();
        });

        let ui = self.clone();
        command_history_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            let mut config = ui.config.borrow_mut();
            config.command_history_enabled = enabled;
            if enabled && config.command_history_path.is_none() {
                config.command_history_path = Some(crate::config::default_command_history_path());
            }
            drop(config);
            ui.sync_block_configs();
            ui.persist_config();
        });

        let motion_for_enabled = ascii_organism_motion_row.clone();
        let ui = self.clone();
        ascii_organism_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            ui.config.borrow_mut().ascii_organism_enabled = enabled;
            motion_for_enabled.set_sensitive(enabled);
            ui.persist_config();
        });

        let ui = self.clone();
        ascii_organism_motion_row.connect_selected_notify(move |row| {
            ui.config.borrow_mut().ascii_organism_motion = match row.selected() {
                1 => Some(crate::config::OrganismMotion::Full),
                2 => Some(crate::config::OrganismMotion::Calm),
                3 => Some(crate::config::OrganismMotion::Static),
                _ => None,
            };
            ui.persist_config();
        });

        let ui = self.clone();
        notifications_row.connect_active_notify(move |row| {
            ui.config.borrow_mut().notify_long_blocks = row.is_active();
            ui.sync_block_configs();
            ui.persist_config();
        });

        let ui = self.clone();
        remote_clipboard_row.connect_active_notify(move |row| {
            ui.config.borrow_mut().allow_remote_clipboard_write = row.is_active();
            ui.sync_block_configs();
            ui.persist_config();
        });

        let dependent_rows: Vec<gtk4::Widget> = vec![
            ai_panel_visible_row.clone().upcast(),
            ai_panel_width_row.clone().upcast(),
            agent_enabled_row.clone().upcast(),
            correction_enabled_row.clone().upcast(),
            provider_row.clone().upcast(),
            model_row.clone().upcast(),
            base_url_row.clone().upcast(),
            api_key_row.clone().upcast(),
            max_tokens_row.clone().upcast(),
            stream_row.clone().upcast(),
            redact_row.clone().upcast(),
        ];
        let agent_turns_for_ai = agent_turns_row.clone();
        let agent_enabled_for_ai = agent_enabled_row.clone();
        let ai_panel_visible_for_ai = ai_panel_visible_row.clone();
        let ui = self.clone();
        ai_enabled_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            ui.config.borrow_mut().ai_enabled = enabled;
            for dependent in &dependent_rows {
                dependent.set_sensitive(!safe_mode && enabled);
            }
            agent_turns_for_ai
                .set_sensitive(!safe_mode && enabled && agent_enabled_for_ai.is_active());
            if !enabled {
                ai_panel_visible_for_ai.set_active(false);
                ui.set_ai_panel_visible(false, false);
            }
            ui.sync_agent_toggle();
            ui.persist_config();
        });

        let ui = self.clone();
        ai_panel_visible_row.connect_active_notify(move |row| {
            ui.set_ai_panel_visible(row.is_active(), true);
        });

        let ui = self.clone();
        ai_panel_width_row.connect_value_notify(move |row| {
            let width = (row.value().round() as u32).clamp(240, 1200);
            let changed = ui.config.borrow().ai_panel_width != width;
            ui.config.borrow_mut().ai_panel_width = width;
            if ui.ai_panel_visible.get() {
                ui.restore_ai_panel_width();
            }
            if changed {
                ui.persist_config();
            }
        });

        let turns_for_agent = agent_turns_row.clone();
        let ui = self.clone();
        agent_enabled_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            ui.config.borrow_mut().agent_enabled = enabled;
            turns_for_agent.set_sensitive(enabled);
            ui.sync_agent_toggle();
            ui.persist_config();
        });

        let ui = self.clone();
        correction_enabled_row.connect_active_notify(move |row| {
            ui.config.borrow_mut().command_correction_enabled = row.is_active();
            ui.persist_config();
        });

        // `Editable::set_text()` may emit `changed` for the intermediate empty
        // value while it replaces the old contents.  Provider changes update
        // these two rows programmatically, so suppress their ordinary edit
        // handlers until the matching Config fields have been replaced as one
        // coherent set.  Otherwise the intermediate empty model is validated
        // and produces a spurious "Settings were not saved" error.
        let syncing_ai_defaults = Rc::new(Cell::new(false));
        let model_for_provider = model_row.clone();
        let base_for_provider = base_url_row.clone();
        let syncing_for_provider = syncing_ai_defaults.clone();
        let ui = self.clone();
        provider_row.connect_selected_notify(move |row| {
            let provider = match row.selected() {
                1 => crate::ai::Provider::OpenAiCompatible,
                2 => crate::ai::Provider::Ollama,
                _ => crate::ai::Provider::Anthropic,
            };
            syncing_for_provider.set(true);
            model_for_provider.set_text(provider.default_model());
            base_for_provider.set_text(provider.default_base_url());
            let mut config = ui.config.borrow_mut();
            config.ai_provider = provider.as_config_value().to_string();
            config.ai_model = provider.default_model().to_string();
            config.ai_base_url = provider.default_base_url().to_string();
            drop(config);
            syncing_for_provider.set(false);
            ui.persist_config();
        });

        let syncing_for_model = syncing_ai_defaults.clone();
        let ui = self.clone();
        model_row.connect_changed(move |row| {
            if syncing_for_model.get() {
                return;
            }
            ui.config.borrow_mut().ai_model = row.text().to_string();
            ui.persist_config();
        });

        let syncing_for_base = syncing_ai_defaults;
        let ui = self.clone();
        base_url_row.connect_changed(move |row| {
            if syncing_for_base.get() {
                return;
            }
            ui.config.borrow_mut().ai_base_url = row.text().to_string();
            ui.persist_config();
        });

        let ui = self.clone();
        api_key_row.connect_apply(move |row| {
            let path = ui
                .config
                .borrow()
                .ai_api_key_file_configured
                .clone()
                .unwrap_or_else(crate::config::default_ai_api_key_path);
            if let Err(error) = crate::ai::write_api_key_file(&path, row.text().as_str()) {
                ui.show_config_error("API Key was not saved", &error.to_string());
                return;
            }
            row.set_text("");
            row.set_title("API Key stored — enter a new value to replace it");
            let mut config = ui.config.borrow_mut();
            config.ai_api_key_file_configured = Some(path.clone());
            if crate::config::ai_api_key_file_env_override().is_none() {
                config.ai_api_key_file = Some(path);
            }
            drop(config);
            ui.ai_panel.refresh_config_display();
            ui.persist_config();
        });

        let ui = self.clone();
        max_tokens_row.connect_value_notify(move |row| {
            ui.config.borrow_mut().ai_max_tokens = row.value() as u32;
            ui.persist_config();
        });

        let ui = self.clone();
        agent_turns_row.connect_value_notify(move |row| {
            ui.config.borrow_mut().agent_max_turns = row.value() as u32;
            ui.persist_config();
        });

        let ui = self.clone();
        stream_row.connect_active_notify(move |row| {
            ui.config.borrow_mut().ai_stream = row.is_active();
            ui.persist_config();
        });

        let ui = self.clone();
        redact_row.connect_active_notify(move |row| {
            ui.config.borrow_mut().ai_redact_secrets = row.is_active();
            ui.ai_panel.refresh_persisted_privacy();
            ui.persist_config();
        });

        // --- Remote hosts: rows rebuilt from the model after every change ---
        let host_rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));
        let populate_cell: RemoteHostsRefresh = Rc::new(RefCell::new(None));
        let ui_for_hosts = self.clone();
        let group_for_hosts = remote_group.clone();
        let rows_for_hosts = host_rows.clone();
        let populate_for_delete = populate_cell.clone();
        let populate_hosts: Rc<dyn Fn()> = Rc::new(move || {
            for row in rows_for_hosts.borrow_mut().drain(..) {
                group_for_hosts.remove(&row);
            }
            let hosts = ui_for_hosts.config.borrow().remote_hosts.clone();
            if hosts.is_empty() {
                let row = adw::ActionRow::builder()
                    .title("No remote hosts configured")
                    .subtitle("Add an ssh destination or a running container")
                    .build();
                row.set_use_markup(false);
                row.set_sensitive(false);
                group_for_hosts.add(&row);
                rows_for_hosts.borrow_mut().push(row);
                return;
            }
            for (index, host) in hosts.into_iter().enumerate() {
                let host_display = crate::review_input::safe_inline_display(&host.name, 1024);
                let transport = if host.docker { "docker" } else { "ssh" };
                let target = match &host.user {
                    Some(user) => format!("{user}@{}", host.host),
                    None => host.host.clone(),
                };
                let mut subtitle =
                    format!("{transport} · {target} · deploy {}", host.deploy.as_str());
                // The dialog has no widget for these, so say they are there
                // rather than let an edit look like it silently dropped them.
                if !host.ssh_args.is_empty() {
                    subtitle.push_str(&format!(" · ssh_args {}", host.ssh_args.join(" ")));
                }
                let row = adw::ActionRow::builder()
                    .title(&host_display)
                    .subtitle(crate::review_input::safe_inline_display(
                        &subtitle,
                        4 * 1024,
                    ))
                    .build();
                row.set_use_markup(false);
                let edit_btn = gtk4::Button::from_icon_name("document-edit-symbolic");
                edit_btn.add_css_class("flat");
                edit_btn.set_valign(gtk4::Align::Center);
                edit_btn.set_tooltip_text(Some("Edit Host"));
                edit_btn.update_property(&[gtk4::accessible::Property::Label(&format!(
                    "Edit remote host {host_display}"
                ))]);
                row.add_suffix(&edit_btn);
                let delete_btn = gtk4::Button::from_icon_name("user-trash-symbolic");
                delete_btn.add_css_class("flat");
                delete_btn.set_valign(gtk4::Align::Center);
                delete_btn.set_tooltip_text(Some("Remove Host"));
                delete_btn.update_property(&[gtk4::accessible::Property::Label(&format!(
                    "Remove remote host {host_display}"
                ))]);
                row.add_suffix(&delete_btn);

                let ui_for_edit = ui_for_hosts.clone();
                let populate_for_edit = populate_for_delete.clone();
                let edit_name = host.name.clone();
                edit_btn.connect_clicked(move |_| {
                    ui_for_edit.present_remote_host_dialog(
                        Some((index, edit_name.clone())),
                        populate_for_edit.clone(),
                    );
                });

                let ui = ui_for_hosts.clone();
                let populate_ref = populate_for_delete.clone();
                let name = host.name.clone();
                delete_btn.connect_clicked(move |_| {
                    let display = crate::review_input::safe_inline_display(&name, 1024);
                    let dialog = adw::AlertDialog::new(
                        Some("Remove this host?"),
                        Some(&format!(
                            "“{display}” will be removed from config.toml. Nothing on the destination is touched."
                        )),
                    );
                    dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
                    let ui_for_response = ui.clone();
                    let populate_ref = populate_ref.clone();
                    let name = name.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response != "remove" {
                            return;
                        }
                        {
                            let mut config = ui_for_response.config.borrow_mut();
                            // The index can go stale if the file was reloaded
                            // behind the panel; fall back to matching the name.
                            match config.remote_hosts.get(index) {
                                Some(host) if host.name == name => {
                                    config.remote_hosts.remove(index);
                                }
                                _ => config.remote_hosts.retain(|h| h.name != name),
                            }
                        }
                        ui_for_response.persist_config();
                        let populate = populate_ref.borrow().clone();
                        if let Some(populate) = populate {
                            populate();
                        }
                    });
                    dialog.present(Some(&ui.window));
                });
                group_for_hosts.add(&row);
                rows_for_hosts.borrow_mut().push(row);
            }
        });
        *populate_cell.borrow_mut() = Some(populate_hosts.clone());
        populate_hosts();

        let ui_for_add = self.clone();
        let populate_for_add = populate_cell;
        add_host_btn.connect_clicked(move |_| {
            ui_for_add.present_remote_host_dialog(None, populate_for_add.clone());
        });

        // Key controller: Ctrl+Shift+O to close
        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_ref = self.settings_dialog.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if matches!(keyval, Key::O | Key::o)
                && state.contains(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK)
            {
                let dialog_to_close = dialog_ref.borrow_mut().take();
                if let Some(d) = dialog_to_close {
                    d.force_close();
                }
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        let dialog_ref = self.settings_dialog.clone();
        dialog.connect_closed(move |_| {
            *dialog_ref.borrow_mut() = None;
        });

        *self.settings_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
    }

    /// The Add/Edit Remote Host dialog. One dialog for both so the accepted
    /// grammar is stated once: a rule enforced on add but not on edit is a rule
    /// the next config load quietly deletes the host over.
    ///
    /// `editing` names the saved host to overwrite; `None` appends a new one.
    fn present_remote_host_dialog(
        &self,
        editing: Option<RemoteHostEditTarget>,
        populate: RemoteHostsRefresh,
    ) {
        // Fields the dialog cannot show — ssh_args, session, remote_shell,
        // login_shell, multiplex, deploy_artifact — are read here and written
        // back untouched. Rebuilding an entry from the visible rows alone would
        // drop a `-p 2222` or a pinned session the moment someone corrected a
        // typo in the name, and nothing on screen would show it happened.
        let existing = editing.as_ref().and_then(|(index, name)| {
            let config = self.config.borrow();
            let resolved = match config.remote_hosts.get(*index) {
                Some(host) if &host.name == name => Some(*index),
                // The file can be reloaded behind an open panel; fall back to
                // matching the name, exactly as the delete path does.
                _ => config.remote_hosts.iter().position(|h| &h.name == name),
            };
            resolved.map(|index| (index, config.remote_hosts[index].clone()))
        });

        let dialog = adw::Dialog::builder()
            .title(if editing.is_some() {
                "Edit Remote Host"
            } else {
                "Add Remote Host"
            })
            .content_width(420)
            .build();

        let name_row = adw::EntryRow::new();
        name_row.set_title("Name (optional)");
        let host_row = adw::EntryRow::new();
        host_row.set_title("Host / container");
        let user_row = adw::EntryRow::new();
        user_row.set_title("User (optional)");
        let docker_row = adw::SwitchRow::builder()
            .title("Docker Container")
            .subtitle("Attach to a running container with docker exec instead of ssh")
            .build();
        let deploy_model = gtk4::StringList::new(&["Off", "Persist", "Incognito"]);
        let deploy_row = adw::ComboRow::builder()
            .title("Deploy jsh")
            .subtitle("Put a jsh on the destination for the life of the session")
            .model(&deploy_model)
            .build();

        if let Some((_, host)) = &existing {
            name_row.set_text(&host.name);
            host_row.set_text(&host.host);
            user_row.set_text(host.user.as_deref().unwrap_or(""));
            docker_row.set_active(host.docker);
            deploy_row.set_selected(match host.deploy {
                jterm_core::jsh_remote::Deploy::Persist => 1,
                jterm_core::jsh_remote::Deploy::Incognito => 2,
                _ => 0,
            });
        }

        let list = ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::None);
        list.add_css_class("boxed-list");
        list.append(&name_row);
        list.append(&host_row);
        list.append(&user_row);
        list.append(&docker_row);
        list.append(&deploy_row);

        let error_label = Label::new(None);
        error_label.add_css_class("error");
        error_label.set_wrap(true);
        error_label.set_xalign(0.0);
        error_label.set_visible(false);

        let content = gtk4::Box::new(Orientation::Vertical, 12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.append(&list);

        // Advanced fields survive an edit, but saying so beats hoping the user
        // trusts it: this is the panel where they would expect to lose them.
        if let Some((_, host)) = &existing {
            let mut kept = Vec::new();
            if !host.ssh_args.is_empty() {
                kept.push(format!("ssh_args = {:?}", host.ssh_args));
            }
            if let Some(session) = &host.session {
                kept.push(format!("session = {session:?}"));
            }
            if host.remote_shell != "jsh" {
                kept.push(format!("remote_shell = {:?}", host.remote_shell));
            }
            if !host.login_shell {
                kept.push("login_shell = false".to_string());
            }
            if !host.multiplex {
                kept.push("multiplex = false".to_string());
            }
            if let Some(artifact) = &host.deploy_artifact {
                kept.push(format!("deploy_artifact = {artifact:?}"));
            }
            if !kept.is_empty() {
                let note = Label::new(Some(&crate::review_input::safe_inline_display(
                    &format!("Kept as configured: {}", kept.join(", ")),
                    4 * 1024,
                )));
                note.add_css_class("dim-label");
                note.set_wrap(true);
                note.set_xalign(0.0);
                content.append(&note);
            }
        }
        content.append(&error_label);

        let header = adw::HeaderBar::new();
        header.set_show_start_title_buttons(false);
        header.set_show_end_title_buttons(false);
        let cancel_btn = gtk4::Button::with_label("Cancel");
        let confirm_btn = gtk4::Button::with_label(if editing.is_some() { "Save" } else { "Add" });
        confirm_btn.add_css_class("suggested-action");
        header.pack_start(&cancel_btn);
        header.pack_end(&confirm_btn);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&content));
        dialog.set_child(Some(&toolbar_view));

        let dialog_for_cancel = dialog.clone();
        cancel_btn.connect_clicked(move |_| {
            dialog_for_cancel.close();
        });

        let ui = self.clone();
        let dialog_for_confirm = dialog.clone();
        let was_editing = editing.is_some();
        confirm_btn.connect_clicked(move |_| {
            let name = name_row.text().trim().to_string();
            let host = host_row.text().trim().to_string();
            let user = user_row.text().trim().to_string();
            // Mirrors parse_remote_hosts so a host accepted here always
            // survives a reload of the saved file.
            let result: Result<(Option<usize>, crate::config::RemoteHost), &'static str> = (|| {
                if was_editing && existing.is_none() {
                    return Err("This host is no longer in the configuration.");
                }
                if host.is_empty() {
                    return Err("Host is required.");
                }
                if !crate::config::remote_field_is_safe(&host)
                    || host.starts_with('-')
                    || host.chars().any(char::is_whitespace)
                {
                    return Err(
                        "Host must not begin with '-' or contain whitespace or control characters.",
                    );
                }
                let user = if user.is_empty() {
                    None
                } else {
                    if !crate::config::remote_field_is_safe(&user)
                        || user.contains('@')
                        || user.chars().any(char::is_whitespace)
                    {
                        return Err("User must not contain '@', whitespace or control characters.");
                    }
                    Some(user.clone())
                };
                if !name.is_empty() && !crate::config::remote_field_is_safe(&name) {
                    return Err("Name must not contain control characters.");
                }
                let display = if name.is_empty() {
                    host.clone()
                } else {
                    name.clone()
                };
                let target = existing.as_ref().map(|(index, _)| *index);
                let config = ui.config.borrow();
                if target.is_none() && config.remote_hosts.len() >= crate::config::MAX_REMOTE_HOSTS
                {
                    return Err("The remote host limit is reached.");
                }
                // The host being edited is not its own duplicate; without the
                // index test no edit that keeps the name could be saved.
                if config
                    .remote_hosts
                    .iter()
                    .enumerate()
                    .any(|(index, h)| h.name == display && Some(index) != target)
                {
                    return Err("A host with this name already exists.");
                }
                drop(config);
                let previous = existing.as_ref().map(|(_, host)| host);
                Ok((
                    target,
                    crate::config::RemoteHost {
                        name: display,
                        host: host.clone(),
                        user,
                        docker: docker_row.is_active(),
                        deploy_artifact: previous.and_then(|h| h.deploy_artifact.clone()),
                        remote_shell: previous
                            .map(|h| h.remote_shell.clone())
                            .unwrap_or_else(|| "jsh".into()),
                        session: previous.and_then(|h| h.session.clone()),
                        ssh_args: previous.map(|h| h.ssh_args.clone()).unwrap_or_default(),
                        login_shell: previous.is_none_or(|h| h.login_shell),
                        multiplex: previous.is_none_or(|h| h.multiplex),
                        deploy: match deploy_row.selected() {
                            1 => jterm_core::jsh_remote::Deploy::Persist,
                            2 => jterm_core::jsh_remote::Deploy::Incognito,
                            _ => jterm_core::jsh_remote::Deploy::Off,
                        },
                    },
                ))
            })(
            );
            match result {
                Err(message) => {
                    error_label.set_text(message);
                    error_label.set_visible(true);
                }
                Ok((target, new_host)) => {
                    {
                        let mut config = ui.config.borrow_mut();
                        match target {
                            // Replaced in place so the host keeps its position
                            // in the picker; remove-then-push would move it to
                            // the end on every edit.
                            Some(index) if index < config.remote_hosts.len() => {
                                config.remote_hosts[index] = new_host;
                            }
                            _ => config.remote_hosts.push(new_host),
                        }
                    }
                    ui.persist_config();
                    let populate = populate.borrow().clone();
                    if let Some(populate) = populate {
                        populate();
                    }
                    dialog_for_confirm.close();
                }
            }
        });

        dialog.present(Some(&self.window));
    }

    /// Set up right-click context menu for a terminal.
    pub(crate) fn setup_context_menu(&self, terminal: &Terminal) {
        let right_click = GestureClick::new();
        right_click.set_button(3); // Right mouse button

        let ui = self.clone();
        let term = terminal.clone();
        right_click.connect_pressed(move |gesture, _n_press, x, y| {
            gesture.set_state(gtk4::EventSequenceState::Claimed);

            // Plain Popover + Buttons: the GAction-based PopoverMenu dispatch does
            // not fire in this GTK build, so direct connect_clicked closures are used.
            let remote_hosts = ui.config.borrow().remote_hosts.clone();
            let link_uri: Option<String> = term.check_match_at(x, y).0.map(|s| s.to_string());

            let popover = gtk4::Popover::new();
            popover.set_parent(&term);
            popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
            popover.set_has_arrow(false);

            let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            vbox.add_css_class("menu");

            let make_item = |label: &str| -> gtk4::Button {
                let btn = gtk4::Button::with_label(label);
                btn.set_has_frame(false);
                btn.set_halign(gtk4::Align::Fill);
                if let Some(child) = btn.child() {
                    child.set_halign(gtk4::Align::Start);
                }
                btn.add_css_class("flat");
                btn
            };

            // Copy
            {
                let item = make_item("Copy");
                let popover_c = popover.clone();
                let term_copy = term.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    term_copy.copy_clipboard_format(Format::Text);
                });
                vbox.append(&item);
            }

            // Paste
            {
                let item = make_item("Paste");
                let popover_c = popover.clone();
                let ui_paste = ui.clone();
                let term_paste_target = term.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    // In block mode the visible VTE is not attached to the shell
                    // PTY.  Use the same view-aware path as Ctrl+Shift+V so
                    // multiline and bracketed paste reach the real session. Focus
                    // the clicked surface first so VTE split panes keep their
                    // existing "paste into this pane" behavior.
                    term_paste_target.grab_focus();
                    ui_paste.execute_action(Action::Paste);
                });
                vbox.append(&item);
            }

            // Split Right
            {
                let item = make_item("Split Right");
                let popover_c = popover.clone();
                let ui_split_h = ui.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    ui_split_h.split_current(Orientation::Horizontal);
                });
                vbox.append(&item);
            }

            // Split Down
            {
                let item = make_item("Split Down");
                let popover_c = popover.clone();
                let ui_split_v = ui.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    ui_split_v.split_current(Orientation::Vertical);
                });
                vbox.append(&item);
            }

            // New Tab
            {
                let item = make_item("New Tab");
                let popover_c = popover.clone();
                let ui_new_tab = ui.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    ui_new_tab.execute_action(Action::NewTab);
                });
                vbox.append(&item);
            }

            // Close Pane
            {
                let item = make_item("Close Pane");
                let popover_c = popover.clone();
                let ui_close = ui.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    ui_close.execute_action(Action::ClosePaneOrTab);
                });
                vbox.append(&item);
            }

            // Remote connect items
            for h in remote_hosts.iter() {
                let item = make_item(&format!("Connect: {}", h.name));
                let popover_c = popover.clone();
                let ui_remote = ui.clone();
                let host = h.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    ui_remote.connect_remote(&host);
                });
                vbox.append(&item);
            }

            // Open Link (only when a hyperlink is under the cursor)
            if let Some(uri) = link_uri {
                let item = make_item("Open Link");
                let popover_c = popover.clone();
                item.connect_clicked(move |_| {
                    popover_c.popdown();
                    open_uri(&uri);
                });
                vbox.append(&item);
            }

            popover.set_child(Some(&vbox));

            popover.connect_closed(move |p| {
                p.unparent();
            });

            popover.popup();
        });

        terminal.add_controller(right_click);
    }

    /// Workflows palette — fuzzy-filterable list of saved command
    /// templates from `~/.config/forge/workflows/`. Enter on a row
    /// either writes the command directly (no args) or opens an
    /// args-entry dialog. Same toggle-to-close model as the other
    /// palettes: re-pressing Ctrl+Shift+M with the palette open closes it.
    pub(crate) fn show_workflows_palette(&self) {
        let dialog_to_close = self.workflows_palette_dialog.borrow_mut().take();
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            return;
        }

        let Some(pane) = self.current_pane_leaf() else {
            log::debug!("[workflows] no active terminal pane");
            return;
        };

        let workflows: Rc<Vec<crate::workflows::Workflow>> = Rc::new(crate::workflows::load_all());
        if workflows.is_empty() {
            log::debug!(
                "[workflows] no workflows in {}",
                crate::workflows::workflows_dir().display()
            );
            // Toast-like hint via a one-shot message dialog. Otherwise the
            // user gets no feedback at all and concludes the chord is dead.
            let dialog = adw::MessageDialog::builder()
                .heading("No workflows yet")
                .body(format!(
                    "Add `*.toml`, `*.yaml`, or `*.yml` workflow files to:\n\n{}",
                    crate::workflows::workflows_dir().display()
                ))
                .build();
            dialog.add_response("ok", "OK");
            dialog.set_transient_for(Some(&self.window));
            dialog.present();
            return;
        }

        let dialog = adw::Dialog::builder()
            .title("Workflows")
            .content_width(620)
            .content_height(480)
            .build();

        let header_bar = adw::HeaderBar::new();
        let filter_entry = SearchEntry::new();
        filter_entry.set_placeholder_text(Some("Filter workflows…"));
        filter_entry.set_hexpand(true);

        let list_box = ListBox::new();
        list_box.set_selection_mode(gtk4::SelectionMode::Single);
        list_box.add_css_class("boxed-list");
        list_box.set_margin_start(12);
        list_box.set_margin_end(12);
        list_box.set_margin_bottom(12);

        // Haystack = name + description + command, all lowercased.
        let haystacks: Rc<Vec<String>> = Rc::new(
            workflows
                .iter()
                .map(|w| {
                    format!(
                        "{} {} {} {}",
                        w.name,
                        w.description,
                        w.command,
                        w.tags.join(" ")
                    )
                    .to_lowercase()
                })
                .collect(),
        );

        for wf in workflows.iter() {
            let subtitle = if wf.description.is_empty() {
                wf.command.clone()
            } else {
                wf.description.clone()
            };
            let row = adw::ActionRow::builder()
                .title(&wf.name)
                .subtitle(&subtitle)
                .activatable(true)
                .build();
            list_box.append(&row);
        }
        if let Some(first_row) = list_box.row_at_index(0) {
            list_box.select_row(Some(&first_row));
        }

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&list_box)
            .build();

        let search_box = gtk4::Box::new(Orientation::Vertical, 0);
        filter_entry.set_margin_start(12);
        filter_entry.set_margin_end(12);
        filter_entry.set_margin_top(8);
        filter_entry.set_margin_bottom(8);
        search_box.append(&filter_entry);
        search_box.append(&scrolled);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&search_box));
        dialog.set_child(Some(&toolbar_view));

        let list_box_for_filter = list_box.clone();
        let haystacks_for_filter = haystacks.clone();
        filter_entry.connect_search_changed(move |entry| {
            let query = entry.text().to_string().to_lowercase();
            let mut first_visible: Option<gtk4::ListBoxRow> = None;
            for (idx, hay) in haystacks_for_filter.iter().enumerate() {
                if let Some(row) = list_box_for_filter.row_at_index(idx as i32) {
                    let visible = query.is_empty() || hay.contains(&query);
                    row.set_visible(visible);
                    if visible && first_visible.is_none() {
                        first_visible = Some(row);
                    }
                }
            }
            if let Some(row) = first_visible {
                list_box_for_filter.select_row(Some(&row));
            }
        });

        // Pick is the only verb here: either write the command directly
        // (no args) or hand off to the args dialog. Cloning the Vec is
        // cheap relative to the dialog work that follows.
        let workflows_for_pick = workflows.clone();
        let ui_self = self.clone();
        let pane_for_pick = pane.clone();
        let pick = Rc::new(move |idx: usize| {
            let Some(wf) = workflows_for_pick.get(idx).cloned() else {
                return;
            };
            if wf.args.is_empty() {
                ui_self.insert_review_text(&pane_for_pick, &wf.command);
            } else {
                ui_self.show_workflow_args_dialog(wf, pane_for_pick.clone());
            }
        });

        let pick_for_activate = pick.clone();
        let dialog_for_activate = dialog.clone();
        list_box.connect_row_activated(move |_, row| {
            let idx = row.index() as usize;
            dialog_for_activate.force_close();
            pick_for_activate(idx);
        });

        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_ref = self.workflows_palette_dialog.clone();
        let list_box_for_key = list_box.clone();
        let dialog_for_key = dialog.clone();
        let pick_for_key = pick.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if keyval == Key::Escape
                || (matches!(keyval, Key::M | Key::m)
                    && state.contains(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK))
            {
                let dialog_to_close = dialog_ref.borrow_mut().take();
                if let Some(d) = dialog_to_close {
                    d.force_close();
                }
                return true.into();
            }
            if matches!(keyval, Key::Return | Key::KP_Enter) {
                if let Some(row) = list_box_for_key.selected_row() {
                    let idx = row.index() as usize;
                    dialog_for_key.force_close();
                    pick_for_key(idx);
                }
                return true.into();
            }
            if keyval == Key::Down {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(-1);
                let mut next = current + 1;
                while let Some(row) = list_box_for_key.row_at_index(next) {
                    if row.is_visible() {
                        list_box_for_key.select_row(Some(&row));
                        break;
                    }
                    next += 1;
                }
                return true.into();
            }
            if keyval == Key::Up {
                let current = list_box_for_key
                    .selected_row()
                    .map(|r| r.index())
                    .unwrap_or(0);
                let mut prev = current - 1;
                while prev >= 0 {
                    if let Some(row) = list_box_for_key.row_at_index(prev) {
                        if row.is_visible() {
                            list_box_for_key.select_row(Some(&row));
                            break;
                        }
                    }
                    prev -= 1;
                }
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        let dialog_ref = self.workflows_palette_dialog.clone();
        dialog.connect_closed(move |_| {
            *dialog_ref.borrow_mut() = None;
        });

        *self.workflows_palette_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
        filter_entry.grab_focus();
    }

    /// Modal arg-entry dialog for a workflow. One Entry per arg, default
    /// pre-filled; "Insert command" substitutes and writes the resolved command into
    /// the live PTY (without a trailing newline — user reviews and hits
    /// Enter). Cancel/Escape exits without touching the terminal.
    pub(crate) fn show_workflow_args_dialog(
        &self,
        wf: crate::workflows::Workflow,
        pane: crate::ui::PaneLeaf,
    ) {
        let dialog = adw::Dialog::builder()
            .title(format!("Workflow: {}", wf.name))
            .content_width(520)
            .build();

        let header_bar = adw::HeaderBar::new();
        let body = gtk4::Box::new(Orientation::Vertical, 8);
        body.set_margin_start(16);
        body.set_margin_end(16);
        body.set_margin_top(12);
        body.set_margin_bottom(12);

        if !wf.description.is_empty() {
            let desc = Label::new(Some(&wf.description));
            desc.set_xalign(0.0);
            desc.set_wrap(true);
            desc.add_css_class("dim-label");
            body.append(&desc);
        }

        // Preview of the template (so the user sees which placeholders
        // they're filling). Monospace-leaning.
        let preview = Label::new(Some(&wf.command));
        preview.set_xalign(0.0);
        preview.set_wrap(true);
        preview.set_selectable(true);
        preview.add_css_class("monospace");
        body.append(&preview);

        // One row per arg.
        let entries: Rc<RefCell<Vec<(String, gtk4::Entry)>>> = Rc::new(RefCell::new(Vec::new()));
        for arg in wf.args.iter() {
            let row = adw::EntryRow::builder()
                .title(&arg.name)
                .text(&arg.default)
                .build();
            if !arg.description.is_empty() {
                row.set_tooltip_text(Some(&arg.description));
            }
            body.append(&row);
            // EntryRow doesn't expose a stable `Entry` handle in this
            // gtk-rs version, so we stash a gtk4::Entry mirror that we
            // bind two-way. Simpler than digging the inner Entry out.
            let entry = gtk4::Entry::new();
            entry.set_text(&arg.default);
            entry.set_visible(false);
            body.append(&entry);
            {
                let entry_clone = entry.clone();
                row.connect_changed(move |r| {
                    entry_clone.set_text(&r.text());
                });
            }
            entries.borrow_mut().push((arg.name.clone(), entry));
        }

        let run_btn = gtk4::Button::with_label("Insert command");
        run_btn.add_css_class("suggested-action");
        run_btn.set_halign(gtk4::Align::End);
        run_btn.set_margin_top(8);
        body.append(&run_btn);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&body));
        dialog.set_child(Some(&toolbar_view));

        let entries_for_run = entries.clone();
        let pane_for_run = pane.clone();
        let ui_for_run = self.clone();
        let dialog_for_run = dialog.clone();
        let template = wf.command.clone();
        run_btn.connect_clicked(move |_| {
            let bindings: Vec<(String, String)> = entries_for_run
                .borrow()
                .iter()
                .map(|(n, e)| (n.clone(), e.text().to_string()))
                .collect();
            match crate::workflows::substitute(&template, &bindings) {
                Ok(resolved) => {
                    dialog_for_run.force_close();
                    ui_for_run.insert_review_text(&pane_for_run, &resolved);
                }
                Err(error) => {
                    log::warn!("refusing unsafe workflow substitution: {error}");
                    let alert =
                        adw::AlertDialog::new(Some("Command was not inserted"), Some(&error));
                    alert.add_response("ok", "OK");
                    alert.set_default_response(Some("ok"));
                    alert.present(Some(&dialog_for_run));
                }
            }
        });

        // Escape closes; Ctrl+Enter from any field inserts the command for
        // keyboard-only operation. It deliberately never sends Enter to PTY.
        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_for_key = dialog.clone();
        let run_btn_for_key = run_btn.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, state| {
            if keyval == Key::Escape {
                dialog_for_key.force_close();
                return true.into();
            }
            if matches!(keyval, Key::Return | Key::KP_Enter)
                && state.contains(ModifierType::CONTROL_MASK)
            {
                run_btn_for_key.emit_clicked();
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        dialog.present(Some(&self.window));
    }

    /// Read-only view of a completed record's bounded output snapshot:
    /// command identity plus outcome from the record, the snapshot text in a
    /// selectable, scrollable TextView. Deliberately not a terminal surface —
    /// nothing here replays bytes or accepts input.
    pub(crate) fn show_record_snapshot_dialog(&self, record_id: u64) {
        let Some(term_view) = self.current_term_view() else {
            return;
        };
        let Some(view) = term_view.record_snapshot_view(record_id) else {
            // The budget can evict a snapshot between navigation and
            // presentation; answer with the honest message, not an empty pane.
            self.toast_overlay
                .add_toast(adw::Toast::new(record_snapshot_unavailable_message()));
            return;
        };

        let dialog = adw::Dialog::builder()
            .title(record_snapshot_dialog_title())
            .content_width(640)
            .content_height(480)
            .build();
        let header_bar = adw::HeaderBar::new();

        let body = gtk4::Box::new(Orientation::Vertical, 8);
        body.set_margin_start(16);
        body.set_margin_end(16);
        body.set_margin_top(12);
        body.set_margin_bottom(12);

        if !view.cmd.is_empty() {
            let command = Label::new(Some(&view.cmd));
            command.set_xalign(0.0);
            command.set_wrap(true);
            command.set_selectable(true);
            command.add_css_class("monospace");
            body.append(&command);
        }

        let status = Label::new(Some(&record_snapshot_status_line(&view)));
        status.set_xalign(0.0);
        status.add_css_class("dim-label");
        body.append(&status);

        if let Some(note) = view.truncation_note() {
            let note_label = Label::new(Some(&note));
            note_label.set_xalign(0.0);
            note_label.add_css_class("dim-label");
            body.append(&note_label);
        }

        let text = gtk4::TextView::new();
        text.buffer().set_text(&view.output);
        text.set_editable(false);
        text.set_monospace(true);
        text.set_top_margin(8);
        text.set_bottom_margin(8);
        text.set_left_margin(8);
        text.set_right_margin(8);
        let scrolled = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Automatic)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .vexpand(true)
            .child(&text)
            .build();
        body.append(&scrolled);

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&body));
        dialog.set_child(Some(&toolbar_view));

        let key_controller = EventControllerKey::new();
        key_controller.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let dialog_for_key = dialog.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == Key::Escape {
                dialog_for_key.force_close();
                return true.into();
            }
            false.into()
        });
        dialog.add_controller(key_controller);

        dialog.present(Some(&self.window));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cross_block_jump_outcome, cross_block_search_dialog_title, cross_block_search_idle_status,
        cross_block_search_jump_unavailable_status, cross_block_search_pending_status,
        cross_block_search_query_error, cross_block_search_status_for_match_count,
        record_snapshot_dialog_title, record_snapshot_status_line,
        record_snapshot_unavailable_message, remote_picker_guard, CrossBlockJumpOutcome,
        CROSS_BLOCK_SEARCH_LIMIT, CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES,
    };
    use crate::block_view::{RecordNavigationResult, RecordSnapshotView};

    #[test]
    fn remote_picker_reports_safe_mode_and_empty_config() {
        assert_eq!(
            remote_picker_guard(true, 1),
            Err("Remote connections are disabled in safe mode.")
        );
        assert_eq!(
            remote_picker_guard(false, 0),
            Err("No remote hosts are configured. Add one in Settings → Remote Hosts.")
        );
        assert!(remote_picker_guard(false, 1).is_ok());
    }

    /// The palette must dispatch on the whole navigation ladder, not on
    /// "did it scroll": a record whose retained snapshot produced the hit is
    /// reachable, and only a record with neither location nor snapshot keeps
    /// the palette open with the unavailable status.
    #[test]
    fn cross_block_jump_dispatches_every_navigation_outcome() {
        assert_eq!(
            cross_block_jump_outcome(RecordNavigationResult::Navigated),
            CrossBlockJumpOutcome::Close
        );
        assert_eq!(
            cross_block_jump_outcome(RecordNavigationResult::SnapshotView { record_id: 42 }),
            CrossBlockJumpOutcome::ShowSnapshot(42)
        );
        assert_eq!(
            cross_block_jump_outcome(RecordNavigationResult::LocationUnavailable),
            CrossBlockJumpOutcome::KeepOpen
        );
        assert_eq!(
            cross_block_jump_outcome(RecordNavigationResult::NoMatchingRecord),
            CrossBlockJumpOutcome::KeepOpen
        );
    }

    #[test]
    fn cross_block_search_copy_stays_generic_and_consistent() {
        assert_eq!(cross_block_search_dialog_title(), "Search Blocks");
        assert_eq!(
            cross_block_search_idle_status(),
            "Type to search across blocks."
        );
        assert_eq!(cross_block_search_pending_status(), "Searching blocks…");
        assert_eq!(cross_block_search_status_for_match_count(0), "No matches.");
        assert_eq!(
            cross_block_search_status_for_match_count(CROSS_BLOCK_SEARCH_LIMIT),
            "500 matches (capped) — refine your query."
        );
        assert_eq!(cross_block_search_status_for_match_count(37), "37 matches");
        assert_eq!(
            cross_block_search_jump_unavailable_status(),
            "This result is searchable, but its exact terminal location is not available yet."
        );
    }

    /// The snapshot dialog's header values come from the completed record;
    /// unknown outcomes stay explicit and background output names itself.
    #[test]
    fn record_snapshot_dialog_copy_states_outcome_and_retention_honestly() {
        assert_eq!(record_snapshot_dialog_title(), "Output Snapshot");
        assert_eq!(
            record_snapshot_unavailable_message(),
            "This record's output snapshot is no longer retained."
        );

        let view = |exit_code, duration_ms, is_background| RecordSnapshotView {
            cmd: "cargo test".to_string(),
            exit_code,
            duration_ms,
            is_background,
            output: "ok".to_string(),
            truncated: false,
        };
        assert_eq!(
            record_snapshot_status_line(&view(Some(0), Some(1_500), false)),
            "Exit code 0 · 1.5s"
        );
        assert_eq!(
            record_snapshot_status_line(&view(None, None, false)),
            "Exit code unknown (the shell reported none)"
        );
        assert_eq!(
            record_snapshot_status_line(&view(None, Some(250), true)),
            "Background output · 250ms"
        );
    }

    #[test]
    fn cross_block_search_rejects_queries_over_eight_kibibytes() {
        assert_eq!(
            cross_block_search_query_error(&"x".repeat(CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES)),
            None
        );
        assert_eq!(
            cross_block_search_query_error(&"x".repeat(CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES + 1)),
            Some("Query is too long (maximum 8 KiB).")
        );
        assert_eq!(
            cross_block_search_query_error(&"界".repeat(CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES / 3)),
            None
        );
        assert!(cross_block_search_query_error(
            &"界".repeat(CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES / 3 + 1)
        )
        .is_some());
    }
}
