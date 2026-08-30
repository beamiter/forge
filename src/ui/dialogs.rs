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
type CrossBlockScheduleRebuild = Rc<dyn Fn(bool)>;
type CrossBlockScheduleRebuildSlot = Rc<RefCell<Option<std::rc::Weak<dyn Fn(bool)>>>>;

/// Which saved host the host dialog is about to overwrite: its index, plus the
/// name it had when the dialog opened. The name is what makes the index safe to
/// act on — the file can be reloaded behind an open dialog, and writing back to
/// a stale index would silently edit a different host.
type RemoteHostEditTarget = (usize, String);

const CROSS_BLOCK_SEARCH_LIMIT: usize = 500;
const CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES: usize = 8 * 1024;
const CROSS_BLOCK_SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);
const CROSS_BLOCK_SEARCH_REFRESH_INTERVAL: Duration = Duration::from_millis(500);
const CROSS_BLOCK_SEARCH_PAGE_STEP: usize = 10;
const CROSS_BLOCK_SEARCH_CONTROLS_MARGIN: i32 = 12;
const CROSS_BLOCK_SEARCH_CONTROL_SPACING: i32 = 6;
/// A ListBox owns one widget tree per row; unlike ListView it does not recycle
/// off-screen rows. Keep this palette intentionally small until it moves to a
/// virtualized model, regardless of the much larger on-disk retention limit.
const HISTORY_PALETTE_ROW_LIMIT: usize = 500;
/// The standalone workflow overlay uses the same drawn-row/navigation cap as
/// ember and frost. Forge keeps command-template search enabled because that
/// is its established recall behaviour; the policy is explicit rather than
/// an uncapped `ListBox` built from every on-disk entry.
const WORKFLOW_PALETTE_POLICY: crate::workflows::PickerPolicy =
    crate::workflows::PickerPolicy::new(15, true);
const WORKFLOW_PALETTE_LABEL_BYTES: usize = 256;
const WORKFLOW_PALETTE_PREVIEW_BYTES: usize = 4 * 1024;

fn render_workflow_palette_rows(list: &ListBox, workflows: &[crate::workflows::Workflow]) {
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
    for workflow in workflows {
        let title = jterm_core::review_input::safe_inline_display(
            &workflow.name,
            WORKFLOW_PALETTE_LABEL_BYTES,
        );
        let subtitle = if workflow.description.is_empty() {
            &workflow.command
        } else {
            &workflow.description
        };
        let subtitle =
            jterm_core::review_input::safe_inline_display(subtitle, WORKFLOW_PALETTE_PREVIEW_BYTES);
        let row = adw::ActionRow::builder()
            .title(&title)
            .subtitle(&subtitle)
            .activatable(true)
            .build();
        list.append(&row);
    }
    if let Some(first_row) = list.row_at_index(0) {
        list.select_row(Some(&first_row));
    }
}

/// `AdwPreferencesGroup:title` is rendered as Pango markup. Escape every
/// caller-provided title so ordinary characters such as `&` stay visible and
/// cannot make GTK reject the label as malformed markup.
fn preferences_group_title(title: &str) -> glib::GString {
    glib::markup_escape_text(title)
}

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
    "Type to search. F5 refreshes; Ctrl+Shift+B bookmarks; Shift+Enter jumps and advances."
}

fn cross_block_search_pending_status() -> &'static str {
    "Searching blocks…"
}

fn cross_block_search_refresh_status() -> &'static str {
    "Refreshing blocks…"
}

fn cross_block_bookmarked_empty_status(
    reason: crate::block_view::BookmarkedSearchEmptyReason,
) -> &'static str {
    use crate::block_view::BookmarkedSearchEmptyReason as Reason;
    match reason {
        Reason::NoRetainedBookmarks => "No bookmarked blocks in retained history.",
        Reason::MetadataMismatch => "No bookmarked blocks match all selected filters.",
        Reason::NoRetainedTextInScope => "No bookmarked blocks with retained text in this scope.",
        Reason::QueryNoMatches => "No matches in bookmarked blocks.",
    }
}

/// Manual refresh owns only an unmodified F5. Modified function keys remain
/// available to the terminal/application below this capture controller.
fn cross_block_search_is_plain_refresh_key(key: Key, state: ModifierType) -> bool {
    key == Key::F5
        && !state.intersects(
            ModifierType::CONTROL_MASK
                | ModifierType::SHIFT_MASK
                | ModifierType::ALT_MASK
                | ModifierType::SUPER_MASK
                | ModifierType::HYPER_MASK
                | ModifierType::META_MASK,
        )
}

fn cross_block_search_is_bookmark_shortcut(key: Key, state: ModifierType) -> bool {
    matches!(key, Key::b | Key::B)
        && state.contains(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK)
        && !state.intersects(
            ModifierType::ALT_MASK
                | ModifierType::SUPER_MASK
                | ModifierType::HYPER_MASK
                | ModifierType::META_MASK,
        )
}

fn cross_block_bookmark_copy(active: bool) -> (&'static str, &'static str) {
    if active {
        ("★", "Remove bookmark from this block")
    } else {
        ("☆", "Bookmark this block for this running session")
    }
}

fn cross_block_bookmark_confirmation(active: bool) -> &'static str {
    if active {
        "Bookmarked block."
    } else {
        "Removed bookmark."
    }
}

fn cross_block_bookmark_unavailable_status() -> &'static str {
    "That block is no longer retained."
}

fn update_cross_block_bookmark_button(button: &gtk4::ToggleButton, active: bool) {
    let (label, description) = cross_block_bookmark_copy(active);
    button.set_active(active);
    button.set_label(label);
    button.set_tooltip_text(Some(description));
    // Ctrl+Shift+B targets the selected result, which can differ from the
    // suffix button that currently owns keyboard focus. Do not advertise that
    // dialog-level action as a shortcut on this specific button.
    button.update_property(&[gtk4::accessible::Property::Label(description)]);
}

fn update_cross_block_bookmark_buttons(
    buttons: &[(u64, gtk4::ToggleButton)],
    record_id: u64,
    active: bool,
) {
    for (_, button) in buttons.iter().filter(|(id, _)| *id == record_id) {
        update_cross_block_bookmark_button(button, active);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossBlockRefreshKeyDecision {
    Refresh,
    SuppressRepeat,
    Propagate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossBlockBookmarkKeyDecision {
    Toggle,
    SuppressRepeat,
    Propagate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossBlockEnterKeyRoute {
    Propagate,
    ConfirmResult,
    Other,
}

/// Only the query and a result row opt into picker-wide confirmation. This
/// allowlist makes every other focusable widget safe by default, including
/// AdwHeaderBar's implicit window Close control and controls added later.
fn cross_block_enter_key_route(key: Key, confirmation_focused: bool) -> CrossBlockEnterKeyRoute {
    if !matches!(key, Key::Return | Key::KP_Enter) {
        CrossBlockEnterKeyRoute::Other
    } else if confirmation_focused {
        CrossBlockEnterKeyRoute::ConfirmResult
    } else {
        CrossBlockEnterKeyRoute::Propagate
    }
}

fn cross_block_focus_confirms_result(
    focused: Option<&gtk4::Widget>,
    query: &gtk4::SearchEntry,
    results: &gtk4::ListBox,
) -> bool {
    let Some(focused) = focused else {
        return false;
    };
    let query = query.upcast_ref::<gtk4::Widget>();
    let results = results.upcast_ref::<gtk4::Widget>();
    focused == query
        || focused.is_ancestor(query)
        || focused == results
        || (focused.is::<gtk4::ListBoxRow>() && focused.is_ancestor(results))
}

/// Claim an action-like physical B press through release. A valid chord toggles
/// once; auto-repeat and modifier-release repeats remain consumed. Plain text B
/// remains fully repeatable in the query entry.
#[derive(Debug, Default)]
struct CrossBlockBookmarkKeyLatch {
    held: Option<(u32, bool)>,
}

impl CrossBlockBookmarkKeyLatch {
    fn press(
        &mut self,
        key: Key,
        keycode: u32,
        state: ModifierType,
    ) -> CrossBlockBookmarkKeyDecision {
        if !matches!(key, Key::b | Key::B) {
            return CrossBlockBookmarkKeyDecision::Propagate;
        }
        if let Some((held_keycode, claimed)) = self.held {
            if held_keycode == keycode {
                return if claimed {
                    CrossBlockBookmarkKeyDecision::SuppressRepeat
                } else {
                    CrossBlockBookmarkKeyDecision::Propagate
                };
            }
        }

        let exact = cross_block_search_is_bookmark_shortcut(key, state);
        // Freeze the first physical press's route. An exact action stays
        // consumed through release; ordinary/invalid text stays pass-through
        // even if its modifiers change later.
        self.held = Some((keycode, exact));
        if exact {
            CrossBlockBookmarkKeyDecision::Toggle
        } else {
            CrossBlockBookmarkKeyDecision::Propagate
        }
    }

    fn release(&mut self, keycode: u32) {
        if self.held.is_some_and(|(held, _)| held == keycode) {
            self.held = None;
        }
    }

    fn reset(&mut self) {
        self.held = None;
    }
}

/// GTK does not expose an auto-repeat flag here, so latch F5 from its first
/// press through its physical release. Every F5 press claims the latch even
/// when modified: releasing the modifier while F5 remains down must not turn a
/// propagated chord into a late manual refresh.
#[derive(Debug, Default)]
struct CrossBlockRefreshKeyLatch {
    f5_held: bool,
}

impl CrossBlockRefreshKeyLatch {
    fn press(&mut self, key: Key, state: ModifierType) -> CrossBlockRefreshKeyDecision {
        if key != Key::F5 {
            return CrossBlockRefreshKeyDecision::Propagate;
        }

        let was_held = self.f5_held;
        self.f5_held = true;
        if !cross_block_search_is_plain_refresh_key(key, state) {
            CrossBlockRefreshKeyDecision::Propagate
        } else if was_held {
            CrossBlockRefreshKeyDecision::SuppressRepeat
        } else {
            CrossBlockRefreshKeyDecision::Refresh
        }
    }

    fn release(&mut self, key: Key) {
        if key == Key::F5 {
            self.f5_held = false;
        }
    }

    fn reset(&mut self) {
        self.f5_held = false;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossBlockRefreshFrameDecision {
    PaintStatus,
    Rebuild,
}

/// A manual rebuild waits across one complete frame-clock interval. The first
/// tick deliberately does no work, allowing the already-updated status label
/// to paint and be announced; the second tick may perform the synchronous
/// bounded scan without making “Refreshing blocks…” an unobservable transient.
#[derive(Debug, Default)]
struct CrossBlockRefreshFrameGate {
    status_frame_seen: bool,
}

impl CrossBlockRefreshFrameGate {
    fn tick(&mut self) -> CrossBlockRefreshFrameDecision {
        if std::mem::replace(&mut self.status_frame_seen, true) {
            CrossBlockRefreshFrameDecision::Rebuild
        } else {
            CrossBlockRefreshFrameDecision::PaintStatus
        }
    }
}

fn cross_block_search_query_error(query: &str) -> Option<&'static str> {
    (query.len() > CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES)
        .then_some("Query is too long (maximum 8 KiB).")
}

fn cross_block_search_has_intent(
    query: &str,
    failed_only: bool,
    slow_only: bool,
    background_only: bool,
    bookmarked_only: bool,
) -> bool {
    !query.is_empty() || failed_only || slow_only || background_only || bookmarked_only
}

fn cross_block_search_compact_layout(
    matching_row: &gtk4::Box,
    filter_row: &gtk4::Box,
) -> ScrolledWindow {
    // Theme, font, translation and accessibility scaling all change natural
    // widget widths. Never infer reachability from guessed pixels: preserve
    // the two semantic rows and expose any excess width through a real GTK
    // horizontal adjustment; vertical overflow belongs to the result list.
    let compact_controls = gtk4::Box::new(Orientation::Vertical, 4);
    compact_controls.add_css_class("toolbar");
    compact_controls.set_margin_start(CROSS_BLOCK_SEARCH_CONTROLS_MARGIN);
    compact_controls.set_margin_end(CROSS_BLOCK_SEARCH_CONTROLS_MARGIN);
    compact_controls.set_margin_top(4);
    compact_controls.set_margin_bottom(4);
    compact_controls.append(matching_row);
    compact_controls.append(filter_row);

    let compact_scroll = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Automatic)
        .vscrollbar_policy(gtk4::PolicyType::Never)
        .child(&compact_controls)
        .build();
    compact_scroll.set_propagate_natural_height(true);
    compact_scroll
}

/// Keep the current dialog claimed until its own `closed` signal; callers must
/// not depend on whether `force_close` emits that signal synchronously. This
/// closes the fast close/release/open window for a second instance.
fn cross_block_search_dialog_for_close(
    dialog_ref: &RefCell<Option<adw::Dialog>>,
) -> Option<adw::Dialog> {
    dialog_ref.borrow().clone()
}

fn clear_cross_block_search_dialog_claim<T: PartialEq>(
    dialog_ref: &RefCell<Option<T>>,
    closed: &T,
) -> bool {
    let mut claimed = dialog_ref.borrow_mut();
    if claimed
        .as_ref()
        .is_some_and(|claimed_dialog| claimed_dialog == closed)
    {
        *claimed = None;
        true
    } else {
        false
    }
}

/// Mirror a user edit into the shared workflow form, but ignore the synchronous
/// `changed` signal emitted while Reset is only bringing the widget back in
/// sync with a state already committed through `ArgsForm::clear`.
fn record_workflow_arg_entry_change(
    form: &mut crate::workflows::ArgsForm,
    index: usize,
    text: &str,
    programmatic_sync: bool,
) {
    if !programmatic_sync {
        form.set(index, text);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CrossBlockSearchMemory {
    query: String,
    options: crate::block_view::CrossBlockSearchOptions,
    scope: crate::block_view::CrossBlockSearchScope,
    failed_only: bool,
    slow_only: bool,
    background_only: bool,
    bookmarked_only: bool,
}

fn cross_block_search_memory(
    query: &str,
    options: crate::block_view::CrossBlockSearchOptions,
    scope: crate::block_view::CrossBlockSearchScope,
    failed_only: bool,
    slow_only: bool,
    background_only: bool,
    bookmarked_only: bool,
) -> CrossBlockSearchMemory {
    CrossBlockSearchMemory {
        // An invalid oversized query stays visible until close, but retaining
        // it would defeat the dialog's explicit memory bound. Reopen clean.
        query: if query.len() <= CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES {
            query.to_string()
        } else {
            String::new()
        },
        options,
        scope,
        failed_only,
        slow_only,
        background_only,
        bookmarked_only,
    }
}

fn cross_block_search_status(total: usize, selected: Option<usize>) -> String {
    if total == 0 {
        "No matches.".to_string()
    } else {
        let noun = if total == 1 { "match" } else { "matches" };
        let position = selected
            .filter(|index| *index < total)
            .map(|index| format!("{} of ", index + 1))
            .unwrap_or_default();
        if total == CROSS_BLOCK_SEARCH_LIMIT {
            format!("{position}{total} {noun} (capped) — refine your query.")
        } else {
            format!("{position}{total} {noun}")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrossBlockSelectionMove {
    First,
    Previous,
    Next,
    PagePrevious,
    PageNext,
    Last,
}

fn cross_block_selection_index(
    current: Option<usize>,
    total: usize,
    movement: CrossBlockSelectionMove,
) -> Option<usize> {
    if total == 0 {
        return None;
    }
    let Some(current) = current.filter(|index| *index < total) else {
        return Some(match movement {
            CrossBlockSelectionMove::Previous | CrossBlockSelectionMove::Last => total - 1,
            _ => 0,
        });
    };
    Some(match movement {
        CrossBlockSelectionMove::First => 0,
        CrossBlockSelectionMove::Previous => (current + total - 1) % total,
        CrossBlockSelectionMove::Next => (current + 1) % total,
        CrossBlockSelectionMove::PagePrevious => {
            current.saturating_sub(CROSS_BLOCK_SEARCH_PAGE_STEP)
        }
        CrossBlockSelectionMove::PageNext => current
            .saturating_add(CROSS_BLOCK_SEARCH_PAGE_STEP)
            .min(total - 1),
        CrossBlockSelectionMove::Last => total - 1,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CrossBlockSelectionAnchor {
    hit: crate::block_view::CrossBlockHit,
    index: usize,
}

/// Restore an open picker's highlight after a record-version refresh. Exact
/// stable identity wins; if retention removed that row, keep the nearest
/// surviving rank instead of disorientingly jumping all the way to the top.
/// A new search intent passes no anchor and deliberately starts at row zero.
fn cross_block_refresh_selection_index(
    results: &[crate::block_view::CrossBlockHit],
    anchor: Option<&CrossBlockSelectionAnchor>,
) -> usize {
    if results.is_empty() {
        return 0;
    }
    anchor
        .and_then(|anchor| results.iter().position(|hit| hit == &anchor.hit))
        .or_else(|| anchor.map(|anchor| anchor.index.min(results.len() - 1)))
        .unwrap_or(0)
}

/// Keep keyboard selection visible while focus remains in the query entry.
fn scroll_cross_block_row_into_view(scrolled: &gtk4::ScrolledWindow, row: &impl IsA<gtk4::Widget>) {
    let Some(bounds) = row.compute_bounds(scrolled) else {
        return;
    };
    let adjustment = scrolled.vadjustment();
    let page = adjustment.page_size();
    let top = bounds.y() as f64;
    let bottom = top + bounds.height() as f64;
    let shift = if top < 0.0 {
        top
    } else if bottom > page {
        bottom - page
    } else {
        return;
    };
    let target = (adjustment.value() + shift).clamp(
        adjustment.lower(),
        (adjustment.upper() - page).max(adjustment.lower()),
    );
    adjustment.set_value(target);
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

/// Continuous review is possible only after an exact live terminal jump.
/// Snapshot-only results still open their snapshot dialog, and unavailable
/// results stay selected with their error status instead of silently moving.
fn cross_block_should_step(outcome: CrossBlockJumpOutcome, requested: bool) -> bool {
    requested && outcome == CrossBlockJumpOutcome::Close
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
        self.invalidate_file_tree_remote_follow();
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

        // Validate borrowed runtime objects before cloning or deriving any UI
        // text. Row activation owns that same validated clone, so filtering an
        // invalid draft cannot redirect it to another configured host.
        let hosts: Rc<Vec<crate::config::RemoteHost>> = Rc::new(
            self.config
                .borrow()
                .remote_hosts
                .iter()
                .take(crate::config::MAX_REMOTE_HOSTS)
                .filter(|host| crate::config::validate_remote_host(host).is_ok())
                .cloned()
                .collect(),
        );
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
            let name = jterm_core::review_input::safe_inline_display(&h.name, 256);
            let target = jterm_core::review_input::safe_inline_display(&target, 512);
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
                if let Some(host) = hosts.get(idx) {
                    ui.connect_remote(host);
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
                    match jterm_core::command_history::read_recent_with_status(
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
                            let error = jterm_core::review_input::safe_inline_display(
                                &error.to_string(),
                                512,
                            );
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
    pub(crate) fn close_cross_block_search(&self) -> bool {
        let dialog_to_close = cross_block_search_dialog_for_close(&self.cross_block_search_dialog);
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            true
        } else {
            false
        }
    }

    pub(crate) fn show_cross_block_search(&self) {
        if self.close_cross_block_search() {
            return;
        }

        let Some(term_view) = self.current_term_view() else {
            log::debug!("[xsearch] no active block-mode tab");
            return;
        };
        let remembered = self.cross_block_search_memory.borrow().clone();

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
        let case_toggle = gtk4::ToggleButton::builder()
            .label("Aa")
            .tooltip_text("Match case")
            .build();
        let whole_word_toggle = gtk4::ToggleButton::builder()
            .label("W")
            .tooltip_text("Match whole words")
            .build();
        let scope_dropdown = gtk4::DropDown::from_strings(&["All", "Cmd", "Out"]);
        scope_dropdown.set_tooltip_text(Some("Search all text, commands only, or output only"));
        let refresh_button = gtk4::Button::from_icon_name("view-refresh-symbolic");
        refresh_button.set_tooltip_text(Some("Refresh block search results (F5)"));
        refresh_button.update_property(&[
            gtk4::accessible::Property::Label("Refresh block search results"),
            gtk4::accessible::Property::KeyShortcuts("F5"),
        ]);
        let reset_button = gtk4::Button::with_label("Reset");
        reset_button.set_tooltip_text(Some("Reset query, matching options, scope, and filters"));
        header_bar.pack_start(&refresh_button);
        header_bar.pack_start(&reset_button);
        let failed_toggle = gtk4::ToggleButton::builder()
            .label("Failed")
            .tooltip_text("Only genuinely failed blocks (not user-interrupted commands)")
            .build();
        let slow_toggle = gtk4::ToggleButton::builder()
            .label("Slow")
            .tooltip_text("Only blocks that ran at least as long as the slow-block threshold")
            .build();
        let background_toggle = gtk4::ToggleButton::builder()
            .label("Background")
            .tooltip_text("Only commandless output emitted while the prompt was idle")
            .build();
        let bookmarked_toggle = gtk4::ToggleButton::builder()
            .label("Bookmarked")
            .tooltip_text(
                "Only blocks bookmarked in this pane for this running session; combines with other selected filters",
            )
            .build();
        bookmarked_toggle.update_property(&[
            gtk4::accessible::Property::Label("Bookmarked"),
            gtk4::accessible::Property::Description(
                "Only blocks bookmarked in this pane for this running session; combines with other selected filters",
            ),
        ]);

        // Keep only the two actions beside the title. The previous single-line
        // HeaderBar accumulated controls whose natural width could push targets
        // off-screen. Two compact content rows keep matching/scope and metadata
        // controls in stable Tab order inside actual horizontal overflow.
        let matching_row =
            gtk4::Box::new(Orientation::Horizontal, CROSS_BLOCK_SEARCH_CONTROL_SPACING);
        let matching_label = Label::new(Some("Match"));
        matching_label.add_css_class("dim-label");
        matching_row.append(&matching_label);
        matching_row.append(&case_toggle);
        matching_row.append(&regex_toggle);
        matching_row.append(&whole_word_toggle);
        let scope_label = Label::new(Some("Scope"));
        scope_label.add_css_class("dim-label");
        matching_row.append(&scope_label);
        matching_row.append(&scope_dropdown);

        let filter_row =
            gtk4::Box::new(Orientation::Horizontal, CROSS_BLOCK_SEARCH_CONTROL_SPACING);
        let filter_label = Label::new(Some("Filter"));
        filter_label.add_css_class("dim-label");
        filter_row.append(&filter_label);
        filter_row.append(&failed_toggle);
        filter_row.append(&slow_toggle);
        filter_row.append(&bookmarked_toggle);
        filter_row.append(&background_toggle);
        let compact_scroll = cross_block_search_compact_layout(&matching_row, &filter_row);

        let filter_entry = SearchEntry::new();
        filter_entry.set_placeholder_text(Some("Search across blocks…"));
        filter_entry.set_hexpand(true);
        case_toggle.set_active(remembered.options.case_sensitive);
        regex_toggle.set_active(remembered.options.regex);
        whole_word_toggle.set_active(remembered.options.whole_word);
        scope_dropdown.set_selected(remembered.scope.index());
        failed_toggle.set_active(remembered.failed_only);
        slow_toggle.set_active(remembered.slow_only);
        background_toggle.set_active(remembered.background_only);
        bookmarked_toggle.set_active(remembered.bookmarked_only);
        filter_entry.set_text(&remembered.query);

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
        toolbar_view.add_top_bar(&compact_scroll);
        toolbar_view.set_content(Some(&search_box));
        dialog.set_child(Some(&toolbar_view));

        // Hits live in a RefCell so both the live-filter closure and the
        // activation closure see the current pass; rebuilt on every
        // keystroke / regex-toggle change.
        let hits: Rc<RefCell<Vec<crate::block_view::CrossBlockHit>>> =
            Rc::new(RefCell::new(Vec::new()));
        let row_bookmark_buttons: Rc<RefCell<Vec<(u64, gtk4::ToggleButton)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let retained_hit: Rc<RefCell<Option<CrossBlockSelectionAnchor>>> =
            Rc::new(RefCell::new(None));
        let pending_rebuild: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
        let pending_manual_refresh: Rc<RefCell<Option<gtk4::TickCallbackId>>> =
            Rc::new(RefCell::new(None));
        let search_generation = Rc::new(Cell::new(0u64));
        let observed_version = Rc::new(Cell::new(term_view.cross_block_search_version()));
        let schedule_rebuild_slot: CrossBlockScheduleRebuildSlot = Rc::new(RefCell::new(None));

        {
            let hits = hits.clone();
            let status_label = status_label.clone();
            list_box.connect_row_selected(move |_, row| {
                let total = hits.borrow().len();
                status_label.set_text(&cross_block_search_status(
                    total,
                    row.map(|row| row.index() as usize),
                ));
            });
        }

        let rebuild = {
            let term_view = term_view.clone();
            let list_box = list_box.clone();
            let hits = hits.clone();
            let row_bookmark_buttons = row_bookmark_buttons.clone();
            let status_label = status_label.clone();
            let filter_entry = filter_entry.clone();
            let regex_toggle = regex_toggle.clone();
            let case_toggle = case_toggle.clone();
            let whole_word_toggle = whole_word_toggle.clone();
            let scope_dropdown = scope_dropdown.clone();
            let failed_toggle = failed_toggle.clone();
            let slow_toggle = slow_toggle.clone();
            let background_toggle = background_toggle.clone();
            let bookmarked_toggle = bookmarked_toggle.clone();
            let retained_hit = retained_hit.clone();
            let observed_version = observed_version.clone();
            let schedule_rebuild_slot = schedule_rebuild_slot.clone();
            Rc::new(move || {
                let query = filter_entry.text().to_string();
                // Navigation remains available while the short refresh
                // debounce runs. Re-snapshot at execution time so a Down/Up
                // pressed during that window wins over the earlier anchor.
                let retained_hit = retained_hit.borrow_mut().take().map(|scheduled| {
                    list_box
                        .selected_row()
                        .and_then(|row| {
                            let index = row.index() as usize;
                            hits.borrow()
                                .get(index)
                                .cloned()
                                .map(|hit| CrossBlockSelectionAnchor { hit, index })
                        })
                        .unwrap_or(scheduled)
                });
                let options = crate::block_view::CrossBlockSearchOptions {
                    case_sensitive: case_toggle.is_active(),
                    regex: regex_toggle.is_active(),
                    whole_word: whole_word_toggle.is_active(),
                };
                let filters = crate::block_view::BlockFilters {
                    failed_only: failed_toggle.is_active(),
                    slow_only: slow_toggle.is_active(),
                    background_only: background_toggle.is_active(),
                    bookmarked_only: bookmarked_toggle.is_active(),
                    slow_threshold_ms: crate::block_view::SLOW_BLOCK_THRESHOLD_MS,
                    ..Default::default()
                };
                clear_list_box(&list_box);
                row_bookmark_buttons.borrow_mut().clear();
                if !cross_block_search_has_intent(
                    &query,
                    filters.failed_only,
                    filters.slow_only,
                    filters.background_only,
                    filters.bookmarked_only,
                ) {
                    hits.borrow_mut().clear();
                    status_label.set_text(cross_block_search_idle_status());
                    return;
                }
                if let Some(message) = cross_block_search_query_error(&query) {
                    hits.borrow_mut().clear();
                    status_label.set_text(message);
                    return;
                }

                let scope =
                    crate::block_view::CrossBlockSearchScope::from_index(scope_dropdown.selected());
                match term_view.cross_block_search_in_scope(
                    &query,
                    options,
                    scope,
                    CROSS_BLOCK_SEARCH_LIMIT,
                    &filters,
                ) {
                    Ok(results) => {
                        let total = results.len();
                        let status = if total == 0 {
                            term_view
                                .bookmarked_search_empty_reason(&query, scope, &filters)
                                .map(cross_block_bookmarked_empty_status)
                                .map(str::to_string)
                                .unwrap_or_else(|| cross_block_search_status(total, None))
                        } else {
                            cross_block_search_status(total, None)
                        };
                        status_label.set_text(&status);
                        let jumpable = term_view.jumpable_search_hits(&results);
                        for hit in results.iter() {
                            let can_jump = jumpable.contains(&(hit.block_id, hit.is_output));
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
                            let bookmark_button = gtk4::ToggleButton::new();
                            bookmark_button.add_css_class("flat");
                            bookmark_button.set_valign(gtk4::Align::Center);
                            update_cross_block_bookmark_button(
                                &bookmark_button,
                                term_view.is_record_bookmarked(hit.block_id),
                            );
                            row_bookmark_buttons
                                .borrow_mut()
                                .push((hit.block_id, bookmark_button.clone()));
                            {
                                let term_view = term_view.clone();
                                let filter_entry = filter_entry.clone();
                                let status_label = status_label.clone();
                                let observed_version = observed_version.clone();
                                let schedule_rebuild_slot = schedule_rebuild_slot.clone();
                                let row_bookmark_buttons = row_bookmark_buttons.clone();
                                let bookmarked_toggle = bookmarked_toggle.clone();
                                let record_id = hit.block_id;
                                bookmark_button.connect_clicked(move |button| {
                                    if let Some(active) =
                                        term_view.toggle_record_bookmark(record_id)
                                    {
                                        update_cross_block_bookmark_buttons(
                                            &row_bookmark_buttons.borrow(),
                                            record_id,
                                            active,
                                        );
                                        observed_version
                                            .set(term_view.cross_block_search_version());
                                        if bookmarked_toggle.is_active() {
                                            if let Some(schedule) = schedule_rebuild_slot
                                                .borrow()
                                                .as_ref()
                                                .and_then(std::rc::Weak::upgrade)
                                            {
                                                schedule(true);
                                            }
                                        }
                                        status_label.announce(
                                            cross_block_bookmark_confirmation(active),
                                            gtk4::AccessibleAnnouncementPriority::Medium,
                                        );
                                    } else {
                                        let active = term_view.is_record_bookmarked(record_id);
                                        update_cross_block_bookmark_buttons(
                                            &row_bookmark_buttons.borrow(),
                                            record_id,
                                            active,
                                        );
                                        // Keep the clicked widget authoritative even if a
                                        // defensive stale row was not present in the index.
                                        update_cross_block_bookmark_button(button, active);
                                        let message = cross_block_bookmark_unavailable_status();
                                        status_label.set_text(message);
                                        status_label.announce(
                                            message,
                                            gtk4::AccessibleAnnouncementPriority::Medium,
                                        );
                                    }
                                    filter_entry.grab_focus();
                                });
                            }
                            row.add_suffix(&bookmark_button);
                            list_box.append(&row);
                        }
                        let selected =
                            cross_block_refresh_selection_index(&results, retained_hit.as_ref());
                        *hits.borrow_mut() = results;
                        if let Some(row) = list_box.row_at_index(selected as i32) {
                            list_box.select_row(Some(&row));
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

        let schedule_rebuild: CrossBlockScheduleRebuild = {
            let pending_rebuild = pending_rebuild.clone();
            let pending_manual_refresh = pending_manual_refresh.clone();
            let search_generation = search_generation.clone();
            let rebuild = rebuild.clone();
            let hits = hits.clone();
            let list_box = list_box.clone();
            let status_label = status_label.clone();
            let filter_entry = filter_entry.clone();
            let retained_hit = retained_hit.clone();
            let failed_toggle = failed_toggle.clone();
            let slow_toggle = slow_toggle.clone();
            let background_toggle = background_toggle.clone();
            let bookmarked_toggle = bookmarked_toggle.clone();
            Rc::new(move |preserve_selection: bool| {
                let generation = search_generation.get().wrapping_add(1);
                search_generation.set(generation);
                if let Some(source) = pending_rebuild.borrow_mut().take() {
                    source.remove();
                }
                if let Some(tick) = pending_manual_refresh.borrow_mut().take() {
                    tick.remove();
                }

                *retained_hit.borrow_mut() = if preserve_selection {
                    list_box.selected_row().and_then(|row| {
                        let index = row.index() as usize;
                        hits.borrow()
                            .get(index)
                            .cloned()
                            .map(|hit| CrossBlockSelectionAnchor { hit, index })
                    })
                } else {
                    None
                };
                if !cross_block_search_has_intent(
                    filter_entry.text().as_str(),
                    failed_toggle.is_active(),
                    slow_toggle.is_active(),
                    background_toggle.is_active(),
                    bookmarked_toggle.is_active(),
                ) {
                    clear_list_box(&list_box);
                    hits.borrow_mut().clear();
                    status_label.set_text(cross_block_search_idle_status());
                    return;
                }
                if let Some(message) = cross_block_search_query_error(filter_entry.text().as_str())
                {
                    clear_list_box(&list_box);
                    hits.borrow_mut().clear();
                    status_label.set_text(message);
                    return;
                }
                if preserve_selection {
                    status_label.set_text(cross_block_search_refresh_status());
                } else {
                    clear_list_box(&list_box);
                    hits.borrow_mut().clear();
                    status_label.set_text(cross_block_search_pending_status());
                }

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
        *schedule_rebuild_slot.borrow_mut() = Some(Rc::downgrade(&schedule_rebuild));

        // Initial state.
        status_label.set_text(cross_block_search_idle_status());

        let rebuild_for_change = schedule_rebuild.clone();
        filter_entry.connect_search_changed(move |_| {
            rebuild_for_change(false);
        });

        let rebuild_for_toggle = schedule_rebuild.clone();
        let filter_entry_for_toggle = filter_entry.clone();
        regex_toggle.connect_toggled(move |_| {
            rebuild_for_toggle(false);
            filter_entry_for_toggle.grab_focus();
        });
        let rebuild_for_toggle = schedule_rebuild.clone();
        let filter_entry_for_toggle = filter_entry.clone();
        case_toggle.connect_toggled(move |_| {
            rebuild_for_toggle(false);
            filter_entry_for_toggle.grab_focus();
        });
        let rebuild_for_toggle = schedule_rebuild.clone();
        let filter_entry_for_toggle = filter_entry.clone();
        whole_word_toggle.connect_toggled(move |_| {
            rebuild_for_toggle(false);
            filter_entry_for_toggle.grab_focus();
        });
        let rebuild_for_scope = schedule_rebuild.clone();
        let filter_entry_for_scope = filter_entry.clone();
        scope_dropdown.connect_selected_notify(move |_| {
            rebuild_for_scope(false);
            filter_entry_for_scope.grab_focus();
        });
        let rebuild_for_filter = schedule_rebuild.clone();
        let filter_entry_for_filter = filter_entry.clone();
        failed_toggle.connect_toggled(move |_| {
            rebuild_for_filter(false);
            filter_entry_for_filter.grab_focus();
        });
        let rebuild_for_filter = schedule_rebuild.clone();
        let filter_entry_for_filter = filter_entry.clone();
        slow_toggle.connect_toggled(move |_| {
            rebuild_for_filter(false);
            filter_entry_for_filter.grab_focus();
        });
        let rebuild_for_filter = schedule_rebuild.clone();
        let filter_entry_for_filter = filter_entry.clone();
        background_toggle.connect_toggled(move |_| {
            rebuild_for_filter(false);
            filter_entry_for_filter.grab_focus();
        });
        let rebuild_for_filter = schedule_rebuild.clone();
        let filter_entry_for_filter = filter_entry.clone();
        bookmarked_toggle.connect_toggled(move |_| {
            rebuild_for_filter(false);
            filter_entry_for_filter.grab_focus();
        });

        {
            let filter_entry = filter_entry.clone();
            let case_toggle = case_toggle.clone();
            let regex_toggle = regex_toggle.clone();
            let whole_word_toggle = whole_word_toggle.clone();
            let scope_dropdown = scope_dropdown.clone();
            let failed_toggle = failed_toggle.clone();
            let slow_toggle = slow_toggle.clone();
            let background_toggle = background_toggle.clone();
            let bookmarked_toggle = bookmarked_toggle.clone();
            reset_button.connect_clicked(move |_| {
                filter_entry.set_text("");
                case_toggle.set_active(false);
                regex_toggle.set_active(false);
                whole_word_toggle.set_active(false);
                scope_dropdown.set_selected(crate::block_view::CrossBlockSearchScope::All.index());
                failed_toggle.set_active(false);
                slow_toggle.set_active(false);
                background_toggle.set_active(false);
                bookmarked_toggle.set_active(false);
                filter_entry.grab_focus();
            });
        }

        // Forge/Anvil dialogs are otherwise snapshots of the moment they
        // opened. Probe only the cheap finalized-record identity and keep the
        // selected stable hit while rebuilding after completion/retention
        // churn. Query text and terminal output are not cloned by the probe.
        {
            let term_view = term_view.clone();
            let observed_version = observed_version.clone();
            let pending_rebuild = pending_rebuild.clone();
            let pending_manual_refresh = pending_manual_refresh.clone();
            let search_generation = search_generation.clone();
            let retained_hit = retained_hit.clone();
            let list_box = list_box.clone();
            let hits = hits.clone();
            let status_label = status_label.clone();
            let filter_entry = filter_entry.clone();
            let rebuild = rebuild.clone();
            refresh_button.connect_clicked(move |_| {
                // The button is the single manual-refresh path. Synchronize
                // the cheap probe first, cancel any pending debounced intent,
                // retain the stable row, then cross one painted frame before
                // rebuilding synchronously.
                observed_version.set(term_view.cross_block_search_version());
                search_generation.set(search_generation.get().wrapping_add(1));
                if let Some(source) = pending_rebuild.borrow_mut().take() {
                    source.remove();
                }
                if let Some(tick) = pending_manual_refresh.borrow_mut().take() {
                    tick.remove();
                }
                *retained_hit.borrow_mut() = list_box.selected_row().and_then(|row| {
                    let index = row.index() as usize;
                    hits.borrow()
                        .get(index)
                        .cloned()
                        .map(|hit| CrossBlockSelectionAnchor { hit, index })
                });
                let refresh_status = cross_block_search_refresh_status();
                status_label.set_text(refresh_status);
                status_label.announce(refresh_status, gtk4::AccessibleAnnouncementPriority::Medium);
                filter_entry.grab_focus();

                // Tick 1 permits the status update above to paint. Tick 2
                // performs the synchronous rebuild; generation and explicit
                // cancellation prevent a delayed manual refresh from racing a
                // newer query/filter intent or a second click.
                let generation = search_generation.get();
                let frame_gate = Rc::new(RefCell::new(CrossBlockRefreshFrameGate::default()));
                let frame_gate_for_tick = frame_gate.clone();
                let pending_clear = pending_manual_refresh.clone();
                let pending_slot = pending_manual_refresh.clone();
                let search_generation_for_tick = search_generation.clone();
                let rebuild_for_tick = rebuild.clone();
                let tick = status_label.add_tick_callback(move |_, _| {
                    match frame_gate_for_tick.borrow_mut().tick() {
                        CrossBlockRefreshFrameDecision::PaintStatus => glib::ControlFlow::Continue,
                        CrossBlockRefreshFrameDecision::Rebuild => {
                            if search_generation_for_tick.get() == generation {
                                rebuild_for_tick();
                            }
                            pending_clear.borrow_mut().take();
                            glib::ControlFlow::Break
                        }
                    }
                });
                *pending_slot.borrow_mut() = Some(tick);
            });
        }
        let refresh_source = {
            let term_view = term_view.clone();
            let observed_version = observed_version.clone();
            let schedule_rebuild = schedule_rebuild.clone();
            glib::timeout_add_local(CROSS_BLOCK_SEARCH_REFRESH_INTERVAL, move || {
                let current = term_view.cross_block_search_version();
                if current != observed_version.get() {
                    observed_version.set(current);
                    schedule_rebuild(true);
                }
                glib::ControlFlow::Continue
            })
        };
        let refresh_source = Rc::new(RefCell::new(Some(refresh_source)));

        // Jump-to-hit: take the target record's best available surface AND
        // turn on its per-VTE search highlight at the matching hit. Plain
        // activation closes; Shift+Enter can keep a successful live jump open.
        let jump = {
            let term_view = term_view.clone();
            let hits = hits.clone();
            let filter_entry = filter_entry.clone();
            let regex_toggle = regex_toggle.clone();
            let case_toggle = case_toggle.clone();
            let whole_word_toggle = whole_word_toggle.clone();
            let status_label = status_label.clone();
            move |idx: usize| -> CrossBlockJumpOutcome {
                let pattern = filter_entry.text().to_string();
                let options = crate::block_view::CrossBlockSearchOptions {
                    case_sensitive: case_toggle.is_active(),
                    regex: regex_toggle.is_active(),
                    whole_word: whole_word_toggle.is_active(),
                };
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
                        if !pattern.is_empty() {
                            term_view.focus_match_in_block(
                                hit.block_id,
                                &pattern,
                                options,
                                hit.is_output,
                                hit.occurrence,
                            );
                        }
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
        let scrolled_for_key = scrolled.clone();
        let hits_for_key = hits.clone();
        let row_bookmark_buttons_for_key = row_bookmark_buttons.clone();
        let filter_entry_for_key = filter_entry.clone();
        let jump_for_key = jump.clone();
        let apply_for_key = apply_jump_outcome.clone();
        let case_toggle_for_key = case_toggle.clone();
        let regex_toggle_for_key = regex_toggle.clone();
        let whole_word_toggle_for_key = whole_word_toggle.clone();
        let scope_dropdown_for_key = scope_dropdown.clone();
        let reset_button_for_key = reset_button.clone();
        let refresh_button_for_key = refresh_button.clone();
        let term_view_for_key = term_view.clone();
        let status_label_for_key = status_label.clone();
        let observed_version_for_key = observed_version.clone();
        let schedule_rebuild_for_key = schedule_rebuild.clone();
        let bookmarked_toggle_for_key = bookmarked_toggle.clone();
        let toggle_key_latch_for_press = self.cross_block_search_toggle_latch.clone();
        let keybinding_map_for_key = self.keybinding_map.clone();
        let refresh_key_latch = Rc::new(RefCell::new(CrossBlockRefreshKeyLatch::default()));
        let refresh_key_latch_for_press = refresh_key_latch.clone();
        let bookmark_key_latch = Rc::new(RefCell::new(CrossBlockBookmarkKeyLatch::default()));
        let bookmark_key_latch_for_press = bookmark_key_latch.clone();
        key_controller.connect_key_pressed(move |_, keyval, keycode, state| {
            let is_toggle = crate::app::chord_from_gdk(keyval, state).is_some_and(|chord| {
                keybinding_map_for_key.borrow().lookup(&chord) == Some(Action::CrossBlockSearch)
            });
            // Normally the ancestor window controller gets here first. If a
            // platform routes directly to the overlay, query the same physical
            // latch on every press: a held opener stays consumed even after its
            // modifiers are released and its current chord becomes plain text.
            match toggle_key_latch_for_press
                .borrow_mut()
                .press(keycode, is_toggle, true)
            {
                CrossBlockSearchToggleRoute::Close => {
                    let dialog_to_close = cross_block_search_dialog_for_close(&dialog_ref);
                    if let Some(d) = dialog_to_close {
                        d.force_close();
                    }
                    return true.into();
                }
                CrossBlockSearchToggleRoute::Open => return true.into(),
                CrossBlockSearchToggleRoute::SuppressRepeat => return true.into(),
                CrossBlockSearchToggleRoute::Proceed => {}
            }
            if keyval == Key::Escape {
                let dialog_to_close = cross_block_search_dialog_for_close(&dialog_ref);
                if let Some(d) = dialog_to_close {
                    d.force_close();
                }
                return true.into();
            }
            if keyval == Key::F5 {
                let decision = refresh_key_latch_for_press
                    .borrow_mut()
                    .press(keyval, state);
                return match decision {
                    CrossBlockRefreshKeyDecision::Refresh => {
                        refresh_button_for_key.emit_clicked();
                        true.into()
                    }
                    CrossBlockRefreshKeyDecision::SuppressRepeat => true.into(),
                    CrossBlockRefreshKeyDecision::Propagate => false.into(),
                };
            }
            match bookmark_key_latch_for_press
                .borrow_mut()
                .press(keyval, keycode, state)
            {
                CrossBlockBookmarkKeyDecision::Toggle => {
                    if let Some(hit) = list_box_for_key
                        .selected_row()
                        .and_then(|row| hits_for_key.borrow().get(row.index() as usize).cloned())
                    {
                        match term_view_for_key.toggle_record_bookmark(hit.block_id) {
                            Some(active) => {
                                update_cross_block_bookmark_buttons(
                                    &row_bookmark_buttons_for_key.borrow(),
                                    hit.block_id,
                                    active,
                                );
                                observed_version_for_key
                                    .set(term_view_for_key.cross_block_search_version());
                                if bookmarked_toggle_for_key.is_active() {
                                    schedule_rebuild_for_key(true);
                                }
                                status_label_for_key.announce(
                                    cross_block_bookmark_confirmation(active),
                                    gtk4::AccessibleAnnouncementPriority::Medium,
                                );
                            }
                            None => {
                                update_cross_block_bookmark_buttons(
                                    &row_bookmark_buttons_for_key.borrow(),
                                    hit.block_id,
                                    term_view_for_key.is_record_bookmarked(hit.block_id),
                                );
                                let message = cross_block_bookmark_unavailable_status();
                                status_label_for_key.set_text(message);
                                status_label_for_key.announce(
                                    message,
                                    gtk4::AccessibleAnnouncementPriority::Medium,
                                );
                            }
                        }
                    }
                    filter_entry_for_key.grab_focus();
                    return true.into();
                }
                CrossBlockBookmarkKeyDecision::SuppressRepeat => return true.into(),
                CrossBlockBookmarkKeyDecision::Propagate => {}
            }
            if state.contains(ModifierType::CONTROL_MASK) {
                if matches!(keyval, Key::u | Key::U) {
                    if state.contains(ModifierType::SHIFT_MASK) {
                        reset_button_for_key.emit_clicked();
                    } else {
                        filter_entry_for_key.set_text("");
                        filter_entry_for_key.grab_focus();
                    }
                    return true.into();
                }
                let toggle = match keyval {
                    Key::i | Key::I => Some(&case_toggle_for_key),
                    Key::r | Key::R => Some(&regex_toggle_for_key),
                    Key::w | Key::W => Some(&whole_word_toggle_for_key),
                    _ => None,
                };
                if let Some(toggle) = toggle {
                    toggle.set_active(!toggle.is_active());
                    return true.into();
                }
                if matches!(keyval, Key::o | Key::O) {
                    let scope = crate::block_view::CrossBlockSearchScope::from_index(
                        scope_dropdown_for_key.selected(),
                    );
                    scope_dropdown_for_key.set_selected(scope.cycled().index());
                    return true.into();
                }
            }
            let focused = filter_entry_for_key.root().and_then(|root| root.focus());
            let confirmation_focused = cross_block_focus_confirms_result(
                focused.as_ref(),
                &filter_entry_for_key,
                &list_box_for_key,
            );
            match cross_block_enter_key_route(keyval, confirmation_focused) {
                CrossBlockEnterKeyRoute::Propagate => return false.into(),
                CrossBlockEnterKeyRoute::ConfirmResult => {
                    if let Some(row) = list_box_for_key.selected_row() {
                        let idx = row.index() as usize;
                        let outcome = jump_for_key(idx);
                        if cross_block_should_step(
                            outcome,
                            state.contains(ModifierType::SHIFT_MASK),
                        ) {
                            if let Some(next) = cross_block_selection_index(
                                Some(idx),
                                hits_for_key.borrow().len(),
                                CrossBlockSelectionMove::Next,
                            ) {
                                if let Some(next_row) = list_box_for_key.row_at_index(next as i32) {
                                    list_box_for_key.select_row(Some(&next_row));
                                    scroll_cross_block_row_into_view(&scrolled_for_key, &next_row);
                                }
                            }
                            filter_entry_for_key.grab_focus();
                        } else {
                            apply_for_key(outcome);
                        }
                    }
                    return true.into();
                }
                CrossBlockEnterKeyRoute::Other => {}
            }
            let movement = match keyval {
                Key::Home | Key::KP_Home => CrossBlockSelectionMove::First,
                Key::Up => CrossBlockSelectionMove::Previous,
                Key::Down => CrossBlockSelectionMove::Next,
                Key::Page_Up => CrossBlockSelectionMove::PagePrevious,
                Key::Page_Down => CrossBlockSelectionMove::PageNext,
                Key::End | Key::KP_End => CrossBlockSelectionMove::Last,
                _ => return false.into(),
            };
            let current = list_box_for_key
                .selected_row()
                .map(|row| row.index() as usize);
            if let Some(next) =
                cross_block_selection_index(current, hits_for_key.borrow().len(), movement)
            {
                if let Some(row) = list_box_for_key.row_at_index(next as i32) {
                    list_box_for_key.select_row(Some(&row));
                    scroll_cross_block_row_into_view(&scrolled_for_key, &row);
                }
            }
            true.into()
        });
        let refresh_key_latch_for_release = refresh_key_latch.clone();
        let bookmark_key_latch_for_release = bookmark_key_latch.clone();
        let toggle_key_latch_for_release = self.cross_block_search_toggle_latch.clone();
        key_controller.connect_key_released(move |_, keyval, keycode, _| {
            refresh_key_latch_for_release.borrow_mut().release(keyval);
            bookmark_key_latch_for_release.borrow_mut().release(keycode);
            toggle_key_latch_for_release.borrow_mut().release(keycode);
        });
        dialog.add_controller(key_controller);
        let refresh_focus = gtk4::EventControllerFocus::new();
        let refresh_key_latch_for_focus = refresh_key_latch.clone();
        let bookmark_key_latch_for_focus = bookmark_key_latch.clone();
        refresh_focus.connect_leave(move |_| {
            // Window-manager deactivation can drop the physical key-release
            // event. Do not strand this dialog in the repeat-suppressed state.
            refresh_key_latch_for_focus.borrow_mut().reset();
            bookmark_key_latch_for_focus.borrow_mut().reset();
        });
        dialog.add_controller(refresh_focus);

        let dialog_ref = self.cross_block_search_dialog.clone();
        let pending_rebuild_for_close = pending_rebuild.clone();
        let pending_manual_refresh_for_close = pending_manual_refresh.clone();
        let refresh_source_for_close = refresh_source.clone();
        let memory_for_close = self.cross_block_search_memory.clone();
        let filter_entry_for_close = filter_entry.clone();
        let case_toggle_for_close = case_toggle.clone();
        let regex_toggle_for_close = regex_toggle.clone();
        let whole_word_toggle_for_close = whole_word_toggle.clone();
        let scope_dropdown_for_close = scope_dropdown.clone();
        let failed_toggle_for_close = failed_toggle.clone();
        let slow_toggle_for_close = slow_toggle.clone();
        let background_toggle_for_close = background_toggle.clone();
        let bookmarked_toggle_for_close = bookmarked_toggle.clone();
        dialog.connect_closed(move |closed_dialog| {
            if let Some(source) = refresh_source_for_close.borrow_mut().take() {
                source.remove();
            }
            if let Some(source) = pending_rebuild_for_close.borrow_mut().take() {
                source.remove();
            }
            if let Some(tick) = pending_manual_refresh_for_close.borrow_mut().take() {
                tick.remove();
            }
            // Only the dialog that still owns the slot may persist intent.
            // A defensive stale callback must neither clear a replacement nor
            // overwrite its eventual memory with old controls.
            if clear_cross_block_search_dialog_claim(&dialog_ref, closed_dialog) {
                *memory_for_close.borrow_mut() = cross_block_search_memory(
                    filter_entry_for_close.text().as_str(),
                    crate::block_view::CrossBlockSearchOptions {
                        case_sensitive: case_toggle_for_close.is_active(),
                        regex: regex_toggle_for_close.is_active(),
                        whole_word: whole_word_toggle_for_close.is_active(),
                    },
                    crate::block_view::CrossBlockSearchScope::from_index(
                        scope_dropdown_for_close.selected(),
                    ),
                    failed_toggle_for_close.is_active(),
                    slow_toggle_for_close.is_active(),
                    background_toggle_for_close.is_active(),
                    bookmarked_toggle_for_close.is_active(),
                );
            }
        });

        *self.cross_block_search_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
        if cross_block_search_has_intent(
            &remembered.query,
            remembered.failed_only,
            remembered.slow_only,
            remembered.background_only,
            remembered.bookmarked_only,
        ) {
            schedule_rebuild(false);
        }
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
                .and_then(|page| tab_display_title(&ui_for_populate.notebook, page))
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
                group.set_title(preferences_group_title(&section).as_str());
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
        group.set_title(preferences_group_title("Appearance").as_str());

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
        terminal_group.set_title(preferences_group_title("Terminal & Blocks").as_str());
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
        privacy_group.set_title(preferences_group_title("Features & Privacy").as_str());
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
        ai_group.set_title(preferences_group_title("AI & Agent").as_str());
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
        remote_group.set_title(preferences_group_title("Remote Hosts").as_str());
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
                let host_display = jterm_core::review_input::safe_inline_display(&host.name, 1024);
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
                    .subtitle(jterm_core::review_input::safe_inline_display(
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
                    let display = jterm_core::review_input::safe_inline_display(&name, 1024);
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
                        let previous_hosts = {
                            let mut config = ui_for_response.config.borrow_mut();
                            let previous_hosts = config.remote_hosts.clone();
                            // The index can go stale if the file was reloaded
                            // behind the panel; fall back to matching the name.
                            match config.remote_hosts.get(index) {
                                Some(host) if host.name == name => {
                                    config.remote_hosts.remove(index);
                                }
                                _ => config.remote_hosts.retain(|h| h.name != name),
                            }
                            previous_hosts
                        };
                        ui_for_response.reconcile_file_tree_remote_hosts(&previous_hosts);
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
                let note = Label::new(Some(&jterm_core::review_input::safe_inline_display(
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
                    let previous_hosts = {
                        let mut config = ui.config.borrow_mut();
                        let previous_hosts = config.remote_hosts.clone();
                        match target {
                            // Replaced in place so the host keeps its position
                            // in the picker; remove-then-push would move it to
                            // the end on every edit.
                            Some(index) if index < config.remote_hosts.len() => {
                                config.remote_hosts[index] = new_host;
                            }
                            _ => config.remote_hosts.push(new_host),
                        }
                        previous_hosts
                    };
                    ui.reconcile_file_tree_remote_hosts(&previous_hosts);
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
            let remote_hosts: Vec<_> = ui
                .config
                .borrow()
                .remote_hosts
                .iter()
                .take(crate::config::MAX_REMOTE_HOSTS)
                .filter(|host| crate::config::validate_remote_host(host).is_ok())
                .cloned()
                .collect();
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
            for h in &remote_hosts {
                let name = jterm_core::review_input::safe_inline_display(&h.name, 256);
                let item = make_item(&format!("Connect: {name}"));
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

        let workflows = crate::workflows::load_all();
        if workflows.is_empty() {
            // `None` when no absolute user config directory resolves, which is
            // also when the loader skips that tier: the hint must never name a
            // directory the loader would not read.
            let user_dir = crate::workflows::user_workflow_dir();
            let target = user_dir
                .as_ref()
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "your user configuration directory".to_string());
            log::debug!("[workflows] no workflows in {target}");
            // Toast-like hint via a one-shot message dialog. Otherwise the
            // user gets no feedback at all and concludes the chord is dead.
            let dialog = adw::MessageDialog::builder()
                .heading("No workflows yet")
                .body(format!(
                    "Add `*.toml`, `*.yaml`, or `*.yml` workflow files to:\n\n{target}"
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

        let picker = Rc::new(RefCell::new(crate::workflows::WorkflowPicker::new(
            workflows,
            WORKFLOW_PALETTE_POLICY,
        )));
        let visible: Vec<_> = picker.borrow().filtered().into_iter().cloned().collect();
        render_workflow_palette_rows(&list_box, &visible);

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
        let picker_for_filter = picker.clone();
        filter_entry.connect_search_changed(move |entry| {
            let normalized = {
                let mut picker = picker_for_filter.borrow_mut();
                picker.set_query(entry.text().to_string());
                picker.query().to_string()
            };
            if entry.text().as_str() != normalized {
                // `set_text` emits this signal again; the normalized second
                // pass reaches the rendering branch and terminates.
                entry.set_text(&normalized);
                return;
            }
            let visible: Vec<_> = picker_for_filter
                .borrow()
                .filtered()
                .into_iter()
                .cloned()
                .collect();
            render_workflow_palette_rows(&list_box_for_filter, &visible);
        });

        // Pick is the only verb here: either write the command directly
        // (no args) or hand off to the args dialog. The row index resolves
        // against the same filtered snapshot the shared picker drew.
        let picker_for_pick = picker.clone();
        let ui_self = self.clone();
        let pane_for_pick = pane.clone();
        let pick = Rc::new(move |idx: usize| {
            let Some(wf) = picker_for_pick.borrow().workflow_at_filtered(idx).cloned() else {
                return;
            };
            if wf.args.is_empty() {
                // Through the template engine, like every other workflow: the
                // raw-template shortcut that used to live here is what made
                // forge's own literal-brace escape a no-op for zero-argument
                // workflows.
                ui_self.insert_workflow(&pane_for_pick, &wf);
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
        let picker_for_key = picker.clone();
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
            if matches!(keyval, Key::Down | Key::Up) {
                let current = list_box_for_key
                    .selected_row()
                    .map(|row| row.index() as usize)
                    .unwrap_or(0);
                let next = {
                    let mut picker = picker_for_key.borrow_mut();
                    picker.select(current);
                    if keyval == Key::Down {
                        picker.select_next();
                    } else {
                        picker.select_prev();
                    }
                    picker.selected()
                };
                if let Some(row) = list_box_for_key.row_at_index(next as i32) {
                    list_box_for_key.select_row(Some(&row));
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

    /// Modal arg-entry dialog for a workflow. One row per argument, seeded
    /// from its declared default; "Insert command" renders and writes the
    /// resolved command into the live PTY (without a trailing newline — the
    /// user reviews and hits Enter). Cancel/Escape exits without touching the
    /// terminal.
    ///
    /// The fill state is a [`crate::workflows::ArgsForm`], not a
    /// `Vec<(String, Entry)>`, because the distinction it carries is the whole
    /// point: an argument whose file declares **no default** starts *unset*,
    /// not pre-seeded with `""`. Every UI in this family used to seed every
    /// declared argument with the empty string, which made `render()`'s
    /// missing-value guard unreachable from all four apps — `kill -9 {pid}`
    /// with an untouched Pid field inserted `kill -9 ` at the prompt. forge
    /// could not even have had that guard: its `WorkflowArg::default` was a
    /// `String`, so "no default" and "empty default" were the same value.
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

        // Which rows are still outstanding, named before the user presses
        // Insert rather than as a failed render afterwards.
        let outstanding = Label::new(None);
        outstanding.set_xalign(0.0);
        outstanding.set_wrap(true);
        outstanding.add_css_class("dim-label");

        let run_btn = gtk4::Button::with_label("Insert command");
        run_btn.add_css_class("suggested-action");
        run_btn.set_halign(gtk4::Align::End);
        run_btn.set_margin_top(8);

        let args = wf.args.clone();
        let form = Rc::new(RefCell::new(crate::workflows::ArgsForm::new(wf)));

        // The hint is advisory, and the button stays live behind it:
        // `missing()` is a superset — an argument the template never
        // references does not block the render — so `render()` remains the one
        // authority on whether this workflow can be inserted.
        let refresh_outstanding = Rc::new({
            let form = form.clone();
            let outstanding = outstanding.clone();
            move || {
                let names = form.borrow().missing().join(", ");
                outstanding.set_visible(!names.is_empty());
                if !names.is_empty() {
                    outstanding.set_text(&format!("Still needs a value: {names}"));
                }
            }
        });

        // One row per argument, seeded from the form so an argument with no
        // declared default comes up visibly empty *and* unset.
        for (index, arg) in args.iter().enumerate() {
            let seed = form.borrow().value(index).to_string();
            let row = adw::EntryRow::builder()
                .title(&arg.name)
                .text(&seed)
                .build();
            if !arg.description.is_empty() {
                row.set_tooltip_text(Some(&arg.description));
            }
            let form_for_row = form.clone();
            let refresh_for_row = refresh_outstanding.clone();
            let programmatic_sync = Rc::new(Cell::new(false));
            let sync_for_row = programmatic_sync.clone();
            row.connect_changed(move |row| {
                record_workflow_arg_entry_change(
                    &mut form_for_row.borrow_mut(),
                    index,
                    row.text().as_str(),
                    sync_for_row.get(),
                );
                refresh_for_row();
            });
            let reset = gtk4::Button::with_label("Reset");
            reset.add_css_class("flat");
            reset.set_valign(gtk4::Align::Center);
            reset.set_tooltip_text(Some(if arg.default.is_some() {
                "Restore the workflow's declared default"
            } else {
                "Clear this value and mark it as required"
            }));
            let form_for_reset = form.clone();
            let row_for_reset = row.clone();
            let refresh_for_reset = refresh_outstanding.clone();
            let sync_for_reset = programmatic_sync.clone();
            reset.connect_clicked(move |_| {
                // Drop the mutable borrow before `set_text`: changing the row
                // emits `changed`, whose callback borrows the same form.
                let value = {
                    let mut form = form_for_reset.borrow_mut();
                    form.clear(index);
                    form.value(index).to_string()
                };
                let previous = sync_for_reset.replace(true);
                row_for_reset.set_text(&value);
                sync_for_reset.set(previous);
                refresh_for_reset();
            });
            row.add_suffix(&reset);
            body.append(&row);
        }

        body.append(&outstanding);
        body.append(&run_btn);
        refresh_outstanding();

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header_bar);
        toolbar_view.set_content(Some(&body));
        dialog.set_child(Some(&toolbar_view));

        let form_for_run = form.clone();
        let pane_for_run = pane.clone();
        let ui_for_run = self.clone();
        let dialog_for_run = dialog.clone();
        run_btn.connect_clicked(move |_| {
            // `ArgsForm::render` re-validates the workflow, bounds and checks
            // every value, and puts the finished text back across
            // `review_input::validate`. The old path called `substitute`,
            // which is the lenient seam and validates neither its bindings nor
            // its output.
            // Rendered before the match, so no borrow is live across
            // `force_close()` — tearing the dialog down disposes the rows, and
            // a row that emitted `changed` on the way out would re-enter the
            // form mutably.
            let rendered = form_for_run.borrow().render();
            match rendered {
                Ok(resolved) => {
                    dialog_for_run.force_close();
                    ui_for_run.insert_review_text(&pane_for_run, &resolved);
                }
                Err(error) => {
                    log::warn!("refusing unsafe or incomplete workflow render: {error}");
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
        clear_cross_block_search_dialog_claim, cross_block_bookmark_confirmation,
        cross_block_bookmark_copy, cross_block_bookmark_unavailable_status,
        cross_block_bookmarked_empty_status, cross_block_enter_key_route,
        cross_block_focus_confirms_result, cross_block_jump_outcome,
        cross_block_refresh_selection_index, cross_block_search_compact_layout,
        cross_block_search_dialog_for_close, cross_block_search_dialog_title,
        cross_block_search_has_intent, cross_block_search_idle_status,
        cross_block_search_is_bookmark_shortcut, cross_block_search_is_plain_refresh_key,
        cross_block_search_jump_unavailable_status, cross_block_search_memory,
        cross_block_search_pending_status, cross_block_search_query_error,
        cross_block_search_refresh_status, cross_block_search_status, cross_block_selection_index,
        cross_block_should_step, preferences_group_title, record_snapshot_dialog_title,
        record_snapshot_status_line, record_snapshot_unavailable_message,
        record_workflow_arg_entry_change, remote_picker_guard, CrossBlockBookmarkKeyDecision,
        CrossBlockBookmarkKeyLatch, CrossBlockEnterKeyRoute, CrossBlockJumpOutcome,
        CrossBlockRefreshFrameDecision, CrossBlockRefreshFrameGate, CrossBlockRefreshKeyDecision,
        CrossBlockRefreshKeyLatch, CrossBlockSelectionAnchor, CrossBlockSelectionMove,
        CROSS_BLOCK_SEARCH_LIMIT, CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES, WORKFLOW_PALETTE_POLICY,
    };
    use crate::block_view::{
        BookmarkedSearchEmptyReason, CrossBlockHit, CrossBlockSearchOptions, CrossBlockSearchScope,
        RecordNavigationResult, RecordSnapshotView,
    };
    use crate::workflows::{ArgsForm, Workflow, WorkflowArg};

    fn cross_block_hit(block_id: u64) -> CrossBlockHit {
        CrossBlockHit {
            block_id,
            is_output: true,
            line_no: 1,
            line_text: format!("hit {block_id}"),
            cmd_preview: "cargo test".to_string(),
            occurrence: 0,
        }
    }

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

    #[test]
    fn workflow_reset_widget_sync_does_not_turn_unset_into_supplied_empty() {
        let workflow = Workflow {
            name: "Run".to_string(),
            description: String::new(),
            command: "run {target}".to_string(),
            tags: Vec::new(),
            shell: None,
            args: vec![WorkflowArg {
                name: "target".to_string(),
                description: String::new(),
                default: None,
            }],
            source_path: None,
        };
        let mut form = ArgsForm::new(workflow);
        form.set(0, "server");
        form.clear(0);
        assert!(!form.is_set(0));

        record_workflow_arg_entry_change(&mut form, 0, "", true);
        assert!(
            !form.is_set(0),
            "the synchronous GTK changed signal must preserve Unset"
        );
        record_workflow_arg_entry_change(&mut form, 0, "", false);
        assert!(
            form.is_set(0),
            "a real user edit is still recorded as supplied"
        );
    }

    #[test]
    fn standalone_workflow_picker_keeps_forge_search_with_a_drawn_row_cap() {
        assert_eq!(WORKFLOW_PALETTE_POLICY.max_results(), 15);
        assert!(WORKFLOW_PALETTE_POLICY.search_command());

        let mut entries: Vec<_> = (0..20)
            .map(|index| crate::workflows::Workflow {
                name: format!("workflow-{index:02}"),
                description: String::new(),
                command: "echo ordinary".to_string(),
                tags: Vec::new(),
                shell: None,
                args: Vec::new(),
                source_path: None,
            })
            .collect();
        entries[19].command = "lsof -ti tcp:4321".to_string();

        let mut picker = crate::workflows::WorkflowPicker::new(entries, WORKFLOW_PALETTE_POLICY);
        assert_eq!(picker.filtered().len(), 15);
        picker.select_prev();
        assert_eq!(picker.selected(), 14, "navigation wraps within drawn rows");
        assert!(!picker.select(15), "an undrawn row cannot be selected");
        assert_eq!(picker.selected(), 14);

        // Forge intentionally retains command-template search. The newline
        // also observes the widget/programmatic query boundary used by the
        // actual SearchEntry callback.
        picker.set_query("lsof\n");
        assert_eq!(picker.query(), "lsof");
        assert_eq!(picker.selected(), 0, "query changes reset selection");
        assert_eq!(picker.filtered().len(), 1);
        assert_eq!(picker.filtered()[0].name, "workflow-19");
    }

    #[test]
    fn preferences_group_titles_escape_pango_markup() {
        assert_eq!(
            preferences_group_title("AI & Agent <safe>").as_str(),
            "AI &amp; Agent &lt;safe&gt;"
        );
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
        assert!(cross_block_should_step(CrossBlockJumpOutcome::Close, true));
        assert!(!cross_block_should_step(
            CrossBlockJumpOutcome::Close,
            false
        ));
        assert!(!cross_block_should_step(
            CrossBlockJumpOutcome::ShowSnapshot(42),
            true
        ));
        assert!(!cross_block_should_step(
            CrossBlockJumpOutcome::KeepOpen,
            true
        ));
    }

    #[test]
    fn cross_block_search_copy_stays_generic_and_consistent() {
        assert_eq!(cross_block_search_dialog_title(), "Search Blocks");
        assert_eq!(
            cross_block_search_idle_status(),
            "Type to search. F5 refreshes; Ctrl+Shift+B bookmarks; Shift+Enter jumps and advances."
        );
        assert_eq!(cross_block_search_pending_status(), "Searching blocks…");
        assert_eq!(cross_block_search_refresh_status(), "Refreshing blocks…");
        assert_eq!(cross_block_search_status(0, None), "No matches.");
        assert_eq!(
            cross_block_search_status(CROSS_BLOCK_SEARCH_LIMIT, None),
            "500 matches (capped) — refine your query."
        );
        assert_eq!(cross_block_search_status(37, None), "37 matches");
        assert_eq!(cross_block_search_status(1, Some(0)), "1 of 1 match");
        assert_eq!(
            cross_block_search_status(CROSS_BLOCK_SEARCH_LIMIT, Some(36)),
            "37 of 500 matches (capped) — refine your query."
        );
        assert_eq!(
            cross_block_search_jump_unavailable_status(),
            "This result is searchable, but its exact terminal location is not available yet."
        );
    }

    #[test]
    fn cross_block_only_confirmation_focus_routes_enter_to_the_selected_result() {
        use gtk4::gdk::Key;

        for key in [Key::Return, Key::KP_Enter] {
            assert_eq!(
                cross_block_enter_key_route(key, true),
                CrossBlockEnterKeyRoute::ConfirmResult,
                "query and result-list focus preserve picker confirmation"
            );
            assert_eq!(
                cross_block_enter_key_route(key, false),
                CrossBlockEnterKeyRoute::Propagate,
                "every other focused widget, including header controls, owns Enter"
            );
        }
        assert_eq!(
            cross_block_enter_key_route(Key::space, true),
            CrossBlockEnterKeyRoute::Other,
            "non-Enter keys keep their normal widget routing"
        );
    }

    #[test]
    #[ignore = "requires DISPLAY"]
    fn cross_block_enter_focus_classifier_allows_query_and_row_but_not_nested_or_header_controls() {
        use gtk4::prelude::*;

        gtk4::init().expect("GTK display");
        let query = gtk4::SearchEntry::new();
        let results = gtk4::ListBox::new();
        let row = gtk4::ListBoxRow::new();
        let bookmark = gtk4::ToggleButton::new();
        row.set_child(Some(&bookmark));
        results.append(&row);
        let close = gtk4::Button::from_icon_name("window-close-symbolic");
        let query_delegate = query.first_child().expect("SearchEntry text delegate");

        assert!(cross_block_focus_confirms_result(
            Some(query.upcast_ref()),
            &query,
            &results
        ));
        assert!(cross_block_focus_confirms_result(
            Some(&query_delegate),
            &query,
            &results
        ));
        assert!(cross_block_focus_confirms_result(
            Some(row.upcast_ref()),
            &query,
            &results
        ));
        assert!(!cross_block_focus_confirms_result(
            Some(bookmark.upcast_ref()),
            &query,
            &results
        ));
        assert!(!cross_block_focus_confirms_result(
            Some(close.upcast_ref()),
            &query,
            &results
        ));
        assert!(!cross_block_focus_confirms_result(None, &query, &results));
    }

    #[test]
    fn cross_block_search_manual_refresh_f5_modifier_matrix_is_strict() {
        use gtk4::gdk::{Key, ModifierType};

        let cases = [
            (ModifierType::empty(), true),
            // Lock state is not an action modifier, so Caps Lock must not
            // change whether an otherwise bare F5 refreshes.
            (ModifierType::LOCK_MASK, true),
            (ModifierType::CONTROL_MASK, false),
            (ModifierType::SHIFT_MASK, false),
            (ModifierType::ALT_MASK, false),
            (ModifierType::SUPER_MASK, false),
            (ModifierType::HYPER_MASK, false),
            (ModifierType::META_MASK, false),
            (ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK, false),
            (
                ModifierType::ALT_MASK | ModifierType::SUPER_MASK | ModifierType::LOCK_MASK,
                false,
            ),
        ];
        for (state, expected) in cases {
            assert_eq!(
                cross_block_search_is_plain_refresh_key(Key::F5, state),
                expected,
                "{state:?}"
            );
        }
        assert!(!cross_block_search_is_plain_refresh_key(
            Key::F6,
            ModifierType::empty()
        ));
    }

    #[test]
    fn cross_block_search_bookmark_shortcut_requires_the_exact_action_chord() {
        use gtk4::gdk::{Key, ModifierType};

        let chord = ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK;
        assert!(cross_block_search_is_bookmark_shortcut(Key::b, chord));
        assert!(cross_block_search_is_bookmark_shortcut(
            Key::B,
            chord | ModifierType::LOCK_MASK
        ));
        for state in [
            ModifierType::CONTROL_MASK,
            ModifierType::SHIFT_MASK,
            chord | ModifierType::ALT_MASK,
            chord | ModifierType::SUPER_MASK,
            chord | ModifierType::META_MASK,
            chord | ModifierType::HYPER_MASK,
        ] {
            assert!(!cross_block_search_is_bookmark_shortcut(Key::b, state));
        }
        assert!(!cross_block_search_is_bookmark_shortcut(Key::n, chord));
    }

    #[test]
    fn cross_block_search_bookmark_shortcut_latches_the_physical_key() {
        use gtk4::gdk::{Key, ModifierType};
        use CrossBlockBookmarkKeyDecision::{Propagate, SuppressRepeat, Toggle};

        let chord = ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK;
        let mut latch = CrossBlockBookmarkKeyLatch::default();
        assert_eq!(latch.press(Key::b, 56, chord), Toggle);
        assert_eq!(latch.press(Key::b, 56, chord), SuppressRepeat);
        assert_eq!(
            latch.press(Key::b, 56, ModifierType::empty()),
            SuppressRepeat,
            "releasing the modifiers while B is held must not type or toggle"
        );
        latch.release(55);
        assert_eq!(latch.press(Key::b, 56, chord), SuppressRepeat);
        latch.release(56);
        assert_eq!(latch.press(Key::b, 56, chord), Toggle);

        latch.reset();
        assert_eq!(latch.press(Key::b, 56, ModifierType::empty()), Propagate);
        assert_eq!(
            latch.press(Key::b, 56, ModifierType::empty()),
            Propagate,
            "plain query text keeps native auto-repeat"
        );

        latch.reset();
        assert_eq!(latch.press(Key::B, 56, ModifierType::SHIFT_MASK), Propagate);
        assert_eq!(
            latch.press(Key::B, 56, ModifierType::SHIFT_MASK),
            Propagate,
            "holding Shift+B must keep typing uppercase query text"
        );

        latch.reset();
        assert_eq!(
            latch.press(Key::b, 56, chord | ModifierType::ALT_MASK),
            Propagate
        );
        assert_eq!(
            latch.press(Key::b, 56, chord),
            Propagate,
            "a held invalid chord remains pass-through rather than becoming a late toggle"
        );
    }

    #[test]
    fn cross_block_search_bookmark_copy_exposes_both_actions() {
        assert_eq!(
            cross_block_bookmark_copy(false),
            ("☆", "Bookmark this block for this running session")
        );
        assert_eq!(
            cross_block_bookmark_copy(true),
            ("★", "Remove bookmark from this block")
        );
        assert_eq!(
            cross_block_bookmark_confirmation(false),
            "Removed bookmark."
        );
        assert_eq!(cross_block_bookmark_confirmation(true), "Bookmarked block.");
        assert_eq!(
            cross_block_bookmark_unavailable_status(),
            "That block is no longer retained."
        );
    }

    #[test]
    fn cross_block_search_bookmarked_empty_copy_names_the_failed_stage() {
        use BookmarkedSearchEmptyReason as Reason;

        assert_eq!(
            cross_block_bookmarked_empty_status(Reason::NoRetainedBookmarks),
            "No bookmarked blocks in retained history."
        );
        assert_eq!(
            cross_block_bookmarked_empty_status(Reason::MetadataMismatch),
            "No bookmarked blocks match all selected filters."
        );
        assert_eq!(
            cross_block_bookmarked_empty_status(Reason::NoRetainedTextInScope),
            "No bookmarked blocks with retained text in this scope."
        );
        assert_eq!(
            cross_block_bookmarked_empty_status(Reason::QueryNoMatches),
            "No matches in bookmarked blocks."
        );
    }

    #[test]
    fn cross_block_search_manual_refresh_latches_until_f5_release() {
        use gtk4::gdk::{Key, ModifierType};
        use CrossBlockRefreshKeyDecision::{Propagate, Refresh, SuppressRepeat};

        let mut latch = CrossBlockRefreshKeyLatch::default();
        assert_eq!(latch.press(Key::F5, ModifierType::empty()), Refresh);
        assert_eq!(
            latch.press(Key::F5, ModifierType::empty()),
            SuppressRepeat,
            "key auto-repeat must not rebuild again"
        );
        latch.release(Key::F4);
        assert_eq!(
            latch.press(Key::F5, ModifierType::empty()),
            SuppressRepeat,
            "only the F5 release clears its physical-key latch"
        );
        latch.release(Key::F5);
        assert_eq!(latch.press(Key::F5, ModifierType::empty()), Refresh);

        latch.reset();
        assert_eq!(
            latch.press(Key::F5, ModifierType::empty()),
            Refresh,
            "leaving the dialog focus domain clears a missed-release latch"
        );

        for modifier in [
            ModifierType::CONTROL_MASK,
            ModifierType::SHIFT_MASK,
            ModifierType::ALT_MASK,
            ModifierType::SUPER_MASK,
            ModifierType::HYPER_MASK,
            ModifierType::META_MASK,
        ] {
            let mut latch = CrossBlockRefreshKeyLatch::default();
            assert_eq!(latch.press(Key::F5, modifier), Propagate);
            assert_eq!(
                latch.press(Key::F5, ModifierType::empty()),
                SuppressRepeat,
                "releasing {modifier:?} while F5 is held must not refresh"
            );
            latch.release(Key::F5);
            assert_eq!(latch.press(Key::F5, ModifierType::empty()), Refresh);
        }
    }

    #[test]
    fn cross_block_search_manual_refresh_crosses_a_painted_frame_before_rebuild() {
        use CrossBlockRefreshFrameDecision::{PaintStatus, Rebuild};

        let mut gate = CrossBlockRefreshFrameGate::default();
        assert_eq!(
            gate.tick(),
            PaintStatus,
            "the first frame must leave Refreshing blocks visible"
        );
        assert_eq!(gate.tick(), Rebuild);
        assert_eq!(
            gate.tick(),
            Rebuild,
            "a completed gate must never regress to its paint stage"
        );
    }

    #[test]
    fn cross_block_search_closed_callback_only_releases_its_own_claim() {
        let slot = std::cell::RefCell::new(Some(2_u8));

        assert!(!clear_cross_block_search_dialog_claim(&slot, &1));
        assert_eq!(
            *slot.borrow(),
            Some(2),
            "a stale close cannot clear a replacement"
        );
        assert!(clear_cross_block_search_dialog_claim(&slot, &2));
        assert_eq!(*slot.borrow(), None);
    }

    #[test]
    #[ignore = "requires DISPLAY"]
    fn cross_block_search_close_borrows_the_gtk_dialog_without_releasing_its_slot() {
        gtk4::init().expect("GTK display");
        let old_dialog = libadwaita::Dialog::new();
        let replacement = libadwaita::Dialog::new();
        let slot = std::cell::RefCell::new(Some(old_dialog.clone()));

        let closing = cross_block_search_dialog_for_close(&slot).expect("claimed dialog");
        assert_eq!(closing, old_dialog);
        assert_eq!(slot.borrow().as_ref(), Some(&old_dialog));

        // A replacement cannot occur through production while the old claim
        // remains, but keep the callback identity-safe against future paths.
        *slot.borrow_mut() = Some(replacement.clone());
        assert!(!clear_cross_block_search_dialog_claim(&slot, &old_dialog));
        assert_eq!(slot.borrow().as_ref(), Some(&replacement));
        assert!(clear_cross_block_search_dialog_claim(&slot, &replacement));
        assert!(slot.borrow().is_none());
    }

    #[test]
    #[ignore = "requires DISPLAY"]
    fn cross_block_search_compact_rows_are_inside_horizontal_scroller() {
        use gtk4::prelude::*;

        gtk4::init().expect("GTK display");
        let matching_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        let filter_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        let scroller = cross_block_search_compact_layout(&matching_row, &filter_row);

        assert_eq!(
            scroller.policy(),
            (gtk4::PolicyType::Automatic, gtk4::PolicyType::Never),
            "theme/font growth must scroll horizontally instead of clipping controls"
        );
        let viewport = scroller
            .child()
            .and_downcast::<gtk4::Viewport>()
            .expect("GTK must provide a viewport for the non-scrollable rows");
        let controls = viewport
            .child()
            .and_downcast::<gtk4::Box>()
            .expect("viewport must directly own the compact rows");
        assert_eq!(controls.orientation(), gtk4::Orientation::Vertical);
        let first = controls.first_child().expect("matching row");
        assert_eq!(first, matching_row.clone().upcast::<gtk4::Widget>());
        let second = first.next_sibling().expect("filter row");
        assert_eq!(second, filter_row.clone().upcast::<gtk4::Widget>());
        assert!(second.next_sibling().is_none(), "exactly two compact rows");
    }

    #[test]
    fn cross_block_selection_wraps_and_pages_within_bounds() {
        use CrossBlockSelectionMove as Move;

        assert_eq!(cross_block_selection_index(None, 0, Move::Next), None);
        assert_eq!(cross_block_selection_index(None, 37, Move::Next), Some(0));
        assert_eq!(
            cross_block_selection_index(None, 37, Move::Previous),
            Some(36)
        );
        assert_eq!(
            cross_block_selection_index(Some(36), 37, Move::Next),
            Some(0)
        );
        assert_eq!(
            cross_block_selection_index(Some(0), 37, Move::Previous),
            Some(36)
        );
        assert_eq!(
            cross_block_selection_index(Some(21), 37, Move::First),
            Some(0)
        );
        assert_eq!(
            cross_block_selection_index(Some(2), 37, Move::Last),
            Some(36)
        );
        assert_eq!(
            cross_block_selection_index(Some(23), 37, Move::PagePrevious),
            Some(13)
        );
        assert_eq!(
            cross_block_selection_index(Some(31), 37, Move::PageNext),
            Some(36)
        );
    }

    #[test]
    fn cross_block_refresh_preserves_identity_then_nearest_rank() {
        let selected = cross_block_hit(2);
        let anchor = CrossBlockSelectionAnchor {
            hit: selected.clone(),
            index: 1,
        };
        assert_eq!(
            cross_block_refresh_selection_index(
                &[cross_block_hit(4), cross_block_hit(3), selected],
                Some(&anchor),
            ),
            2,
            "the same stable hit wins even when its rank moves"
        );
        assert_eq!(
            cross_block_refresh_selection_index(
                &[cross_block_hit(4), cross_block_hit(3)],
                Some(&anchor),
            ),
            1,
            "an evicted hit keeps the closest surviving rank"
        );
        assert_eq!(
            cross_block_refresh_selection_index(&[cross_block_hit(4)], Some(&anchor)),
            0
        );
        assert_eq!(
            cross_block_refresh_selection_index(&[cross_block_hit(4)], None),
            0,
            "a new intent deliberately starts at the top"
        );
        assert_eq!(cross_block_refresh_selection_index(&[], Some(&anchor)), 0);
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

    #[test]
    fn cross_block_search_metadata_filters_are_intent_without_text() {
        assert!(!cross_block_search_has_intent(
            "", false, false, false, false
        ));
        assert!(cross_block_search_has_intent(
            "needle", false, false, false, false
        ));
        assert!(cross_block_search_has_intent("", true, false, false, false));
        assert!(cross_block_search_has_intent("", false, true, false, false));
        assert!(cross_block_search_has_intent("", false, false, true, false));
        assert!(cross_block_search_has_intent("", false, false, false, true));
        assert!(cross_block_search_has_intent("", true, true, true, true));
    }

    #[test]
    fn cross_block_search_memory_is_bounded_and_keeps_matching_intent() {
        let options = CrossBlockSearchOptions {
            case_sensitive: true,
            regex: true,
            whole_word: true,
        };
        let remembered = cross_block_search_memory(
            "needle",
            options,
            CrossBlockSearchScope::Output,
            true,
            true,
            true,
            true,
        );
        assert_eq!(remembered.query, "needle");
        assert_eq!(remembered.options, options);
        assert_eq!(remembered.scope, CrossBlockSearchScope::Output);
        assert!(remembered.failed_only);
        assert!(remembered.slow_only);
        assert!(remembered.background_only);
        assert!(remembered.bookmarked_only);

        let oversized = cross_block_search_memory(
            &"x".repeat(CROSS_BLOCK_SEARCH_QUERY_LIMIT_BYTES + 1),
            options,
            CrossBlockSearchScope::Command,
            true,
            false,
            true,
            false,
        );
        assert!(oversized.query.is_empty());
        assert_eq!(oversized.options, options);
        assert_eq!(oversized.scope, CrossBlockSearchScope::Command);
        assert!(oversized.failed_only);
        assert!(!oversized.slow_only);
        assert!(oversized.background_only);
        assert!(!oversized.bookmarked_only);
        assert!(!super::CrossBlockSearchMemory::default().background_only);
        assert!(!super::CrossBlockSearchMemory::default().bookmarked_only);
    }
}
