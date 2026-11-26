//! Debounced, bounded terminal-history search UI and result feedback.
use adw::prelude::*;
use gtk4::glib;
use libadwaita as adw;
use std::rc::{Rc, Weak};
use std::time::Duration;
#[cfg(any(not(all(forge_shared_terminal, target_os = "linux")), test))]
use vte4::TerminalExt;

use crate::block_view::{FindNavigationResult, FindProgress, FindSearchResult, TermView};

use super::*;

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
const SEARCH_QUERY_BYTE_LIMIT: usize = 8 * 1024;

/// The pane that owns the installed query, independently of later focus.
/// Neither backend may be kept alive solely by an open search bar.
pub(crate) struct SearchOwner {
    #[cfg(any(test, forge_legacy_renderer))]
    terminal: Option<glib::WeakRef<vte4::Terminal>>,
    view: Option<Weak<TermView>>,
    /// A pane-local Find button captures its owner before the entry gains
    /// focus. Debounced edits and steps keep that owner until a page switch.
    pinned: bool,
    #[cfg(all(forge_shared_terminal, target_os = "linux"))]
    shared: Option<Weak<crate::ui::shared_terminal::SharedPane>>,
}

impl SearchOwner {
    fn new(
        #[cfg(any(test, forge_legacy_renderer))] terminal: Option<&vte4::Terminal>,
        view: Option<&Rc<TermView>>,
        #[cfg(all(forge_shared_terminal, target_os = "linux"))] shared: Option<
            &Rc<crate::ui::shared_terminal::SharedPane>,
        >,
    ) -> Self {
        Self {
            #[cfg(any(test, forge_legacy_renderer))]
            terminal: terminal.map(|terminal| terminal.downgrade()),
            view: view.map(Rc::downgrade),
            pinned: false,
            #[cfg(all(forge_shared_terminal, target_os = "linux"))]
            shared: shared.map(Rc::downgrade),
        }
    }

    fn matches(
        &self,
        #[cfg(any(test, forge_legacy_renderer))] terminal: Option<&vte4::Terminal>,
        view: Option<&Rc<TermView>>,
        #[cfg(all(forge_shared_terminal, target_os = "linux"))] shared: Option<
            &Rc<crate::ui::shared_terminal::SharedPane>,
        >,
    ) -> bool {
        #[cfg(any(test, forge_legacy_renderer))]
        let same_terminal = match (&self.terminal, terminal) {
            (Some(previous), Some(current)) => previous.upgrade().as_ref() == Some(current),
            (None, None) => true,
            _ => false,
        };
        let same_view = match (&self.view, view) {
            (Some(previous), Some(current)) => previous
                .upgrade()
                .is_some_and(|previous| Rc::ptr_eq(&previous, current)),
            (None, None) => true,
            _ => false,
        };
        #[cfg(all(forge_shared_terminal, target_os = "linux"))]
        let same_shared = match (&self.shared, shared) {
            (Some(previous), Some(current)) => previous
                .upgrade()
                .is_some_and(|previous| Rc::ptr_eq(&previous, current)),
            (None, None) => true,
            _ => false,
        };
        #[cfg(not(all(forge_shared_terminal, target_os = "linux")))]
        let same_shared = true;
        #[cfg(any(test, forge_legacy_renderer))]
        if !same_terminal {
            return false;
        }
        same_view && same_shared
    }

    fn clear(&self) {
        #[cfg(all(forge_shared_terminal, target_os = "linux"))]
        if let Some(view) = self.shared.as_ref().and_then(Weak::upgrade) {
            if let Some(find) = view.find_controller() {
                find.clear();
            }
        }
        #[cfg(any(not(all(forge_shared_terminal, target_os = "linux")), test))]
        if let Some(terminal) = self.terminal.as_ref().and_then(glib::WeakRef::upgrade) {
            terminal.search_set_regex(None::<&vte4::Regex>, 0);
        }
        if let Some(view) = self.view.as_ref().and_then(Weak::upgrade) {
            view.clear_find();
        }
    }
}

fn query_exceeds_byte_limit(query: &str) -> bool {
    query.len() > SEARCH_QUERY_BYTE_LIMIT
}

fn find_status_text(result: FindSearchResult) -> String {
    match result {
        FindSearchResult::NoMatches => "No matches".to_string(),
        FindSearchResult::InvalidRegex => "Invalid regex".to_string(),
        FindSearchResult::ScanLimit => "Search incomplete (scan limit)".to_string(),
        FindSearchResult::Matches(progress) => progress_status_text(progress),
    }
}

fn progress_status_text(progress: FindProgress) -> String {
    if progress.scan_limited {
        format!("{} of {}+ (scan limit)", progress.current, progress.total)
    } else if progress.capped {
        format!("{} of {}+", progress.current, progress.total)
    } else {
        format!("{} of {}", progress.current, progress.total)
    }
}

#[cfg(any(not(all(forge_shared_terminal, target_os = "linux")), test))]
fn terminal_status_text(found: bool) -> &'static str {
    if found {
        // VTE exposes navigation success but not a total count. This is an
        // honest lower bound for classic/non-Block terminals.
        "1 of 1+"
    } else {
        "No matches"
    }
}

#[cfg(all(forge_shared_terminal, target_os = "linux"))]
fn shared_status_text(status: jterm_gtk_view::TerminalFindStatus) -> String {
    match status {
        jterm_gtk_view::TerminalFindStatus::Idle => String::new(),
        jterm_gtk_view::TerminalFindStatus::Results {
            current,
            total,
            truncated,
        } => {
            if total == 0 && truncated {
                "Search incomplete (scan limit)".into()
            } else if total == 0 {
                "No matches".into()
            } else {
                format!("{current} of {total}{}", if truncated { "+" } else { "" })
            }
        }
        jterm_gtk_view::TerminalFindStatus::Error(error) => {
            if error.starts_with("Invalid search pattern:") {
                "Invalid regex".into()
            } else {
                error
            }
        }
    }
}

impl UiState {
    /// Register edits before GtkSearchEntry's delayed search-changed signal.
    /// The controller already owns the single debounce timer.
    pub(crate) fn connect_search_entry_changes(self: &Rc<Self>) -> glib::SignalHandlerId {
        let ui = self.clone();
        self.search_entry
            .connect_changed(move |_| ui.schedule_search_apply())
    }

    pub(super) fn show_search_for(&self, view: &Rc<TermView>) {
        self.cancel_pending_search();
        self.clear_search_owner();
        let mut owner = SearchOwner::new(
            #[cfg(any(test, forge_legacy_renderer))]
            view.legacy_terminal(),
            Some(view),
            #[cfg(all(forge_shared_terminal, target_os = "linux"))]
            None,
        );
        owner.pinned = true;
        *self.search_owner.borrow_mut() = Some(owner);
        self.search_bar.set_search_mode(true);
        self.search_entry.grab_focus();
        self.search_apply();
    }

    pub(super) fn search_view(&self) -> Option<Rc<TermView>> {
        let owner = self.search_owner.borrow();
        if let Some(owner) = owner.as_ref().filter(|owner| owner.pinned) {
            // A closed/hidden captured pane must not fall back to its sibling.
            return owner
                .view
                .as_ref()
                .and_then(Weak::upgrade)
                .filter(|view| view.widget().is_mapped());
        }
        self.current_term_view()
    }

    #[cfg(any(test, forge_legacy_renderer))]
    fn native_search_target(&self) -> Option<vte4::Terminal> {
        if self
            .search_owner
            .borrow()
            .as_ref()
            .is_some_and(|owner| owner.pinned)
        {
            return self
                .search_view()
                .and_then(|view| view.legacy_terminal().cloned());
        }
        self.current_terminal()
    }

    #[cfg(all(forge_shared_terminal, target_os = "linux"))]
    fn shared_search_target(&self) -> Option<Rc<crate::ui::shared_terminal::SharedPane>> {
        if let Some(owner) = self
            .search_owner
            .borrow()
            .as_ref()
            .filter(|owner| owner.pinned)
        {
            return owner
                .shared
                .as_ref()
                .and_then(Weak::upgrade)
                .filter(|pane| pane.root().is_mapped());
        }
        match self.current_pane_leaf()? {
            crate::ui::PaneLeaf::Shared(pane) => Some(pane),
            _ => None,
        }
    }

    #[cfg(all(forge_shared_terminal, target_os = "linux"))]
    fn shared_search_in_page(
        &self,
        page: &gtk4::Widget,
    ) -> Option<Rc<crate::ui::shared_terminal::SharedPane>> {
        match crate::ui::PaneNode::from_widget(page)?.active_leaf()? {
            crate::ui::PaneLeaf::Shared(pane) => Some(pane),
            _ => None,
        }
    }

    pub(crate) fn toggle_search(&self) {
        let visible = self.search_bar.is_search_mode();
        let pinned = visible
            && self
                .search_owner
                .borrow()
                .as_ref()
                .is_some_and(|owner| owner.pinned);
        // Capture ownership before hiding the bar: unmapping its focused
        // entry can let GTK choose another pane. A user who already focused
        // something outside the bar keeps that new focus when it closes.
        let restore_pinned = pinned
            && self
                .search_bar
                .root()
                .and_then(|root| root.focus())
                .is_some_and(|focus| {
                    focus == *self.search_bar.upcast_ref::<gtk4::Widget>()
                        || focus.is_ancestor(&self.search_bar)
                });
        let pinned_target = restore_pinned.then(|| self.search_view()).flatten();
        #[cfg(all(forge_shared_terminal, target_os = "linux"))]
        let pinned_shared_target = restore_pinned
            .then(|| self.shared_search_target())
            .flatten();
        #[cfg(all(forge_shared_terminal, target_os = "linux"))]
        if !visible {
            if let Some(pane) = self.shared_search_target() {
                let mut owner = SearchOwner::new(
                    #[cfg(any(test, forge_legacy_renderer))]
                    None,
                    None,
                    Some(&pane),
                );
                owner.pinned = true;
                *self.search_owner.borrow_mut() = Some(owner);
            }
        }
        self.search_bar.set_search_mode(!visible);
        if !visible {
            self.search_entry.grab_focus();
            if !self.search_entry.text().is_empty() {
                self.schedule_search_apply();
            }
        } else {
            self.cancel_pending_search();
            // Focus may have moved to another split since this query ran.
            self.clear_search_owner();
            self.search_status.set_text("");
            #[cfg(all(forge_shared_terminal, target_os = "linux"))]
            if let Some(pane) = pinned_shared_target {
                pane.focus_widget().grab_focus();
                return;
            }
            if let Some(view) = pinned_target {
                view.grab_focus();
            } else if !pinned {
                self.focus_current_terminal();
            }
        }
    }

    fn clear_search_owner(&self) {
        let previous = self.search_owner.borrow_mut().take();
        if let Some(previous) = previous {
            previous.clear();
        }
    }

    fn search_owner_is_current(&self) -> bool {
        let view = self.search_view();
        #[cfg(any(test, forge_legacy_renderer))]
        let terminal = self.native_search_target();
        self.search_owner.borrow().as_ref().is_some_and(|owner| {
            owner.matches(
                #[cfg(any(test, forge_legacy_renderer))]
                terminal.as_ref(),
                view.as_ref(),
                #[cfg(all(forge_shared_terminal, target_os = "linux"))]
                self.shared_search_target().as_ref(),
            )
        })
    }

    fn cancel_pending_search(&self) {
        self.search_generation
            .set(self.search_generation.get().wrapping_add(1));
        if let Some(source) = self.search_debounce_source.borrow_mut().take() {
            source.remove();
        }
    }

    pub(crate) fn schedule_search_apply(&self) {
        self.cancel_pending_search();
        // GtkSearchEntry can deliver its delayed signal after the bar closes.
        if !self.search_bar.is_search_mode() {
            return;
        }
        if self.search_entry.text().is_empty() {
            self.apply_search_now();
            return;
        }

        self.search_status.set_text("Searching…");
        let generation = self.search_generation.get();
        let current_generation = self.search_generation.clone();
        let source_slot = self.search_debounce_source.clone();
        let ui = self.clone();
        let source = glib::timeout_add_local_once(SEARCH_DEBOUNCE, move || {
            if current_generation.get() == generation {
                source_slot.borrow_mut().take();
                ui.apply_search_now();
            }
        });
        *self.search_debounce_source.borrow_mut() = Some(source);
    }

    pub(crate) fn search_apply(&self) {
        self.cancel_pending_search();
        self.apply_search_now();
    }

    /// `switch-page` runs before GtkNotebook commits `current_page`. Bind the
    /// query to its explicit destination instead of resolving the old page.
    pub(crate) fn search_apply_in_page(&self, page: &gtk4::Widget) {
        self.cancel_pending_search();
        if let Some(owner) = self.search_owner.borrow_mut().as_mut() {
            owner.pinned = false;
        }
        self.apply_search_to(
            #[cfg(any(test, forge_legacy_renderer))]
            self.terminal_in_page(page).as_ref(),
            self.term_view_in_page(page).as_ref(),
            #[cfg(all(forge_shared_terminal, target_os = "linux"))]
            self.shared_search_in_page(page).as_ref(),
        );
    }

    fn apply_search_now(&self) {
        let view = self.search_view();
        #[cfg(any(test, forge_legacy_renderer))]
        let terminal = self.native_search_target();
        self.apply_search_to(
            #[cfg(any(test, forge_legacy_renderer))]
            terminal.as_ref(),
            view.as_ref(),
            #[cfg(all(forge_shared_terminal, target_os = "linux"))]
            self.shared_search_target().as_ref(),
        );
    }

    fn apply_search_to(
        &self,
        #[cfg(any(test, forge_legacy_renderer))] terminal: Option<&vte4::Terminal>,
        term_view: Option<&Rc<TermView>>,
        #[cfg(all(forge_shared_terminal, target_os = "linux"))] shared: Option<
            &Rc<crate::ui::shared_terminal::SharedPane>,
        >,
    ) {
        if !self.search_bar.is_search_mode() {
            return;
        }
        let pinned = self
            .search_owner
            .borrow()
            .as_ref()
            .is_some_and(|owner| owner.pinned);
        self.clear_search_owner();
        let mut owner = SearchOwner::new(
            #[cfg(any(test, forge_legacy_renderer))]
            terminal,
            term_view,
            #[cfg(all(forge_shared_terminal, target_os = "linux"))]
            shared,
        );
        owner.pinned = pinned;
        *self.search_owner.borrow_mut() = Some(owner);
        #[cfg(all(forge_shared_terminal, target_os = "linux"))]
        let missing_shared = shared.is_none();
        #[cfg(not(all(forge_shared_terminal, target_os = "linux")))]
        let missing_shared = true;
        if pinned && term_view.is_none() && missing_shared {
            self.search_status
                .set_text("The search pane is no longer available");
            return;
        }
        // A new query retires the live fallback before either backend can
        // return early. Block errors/limits must never leave Enter navigating
        // a native regex installed by an older query.
        #[cfg(any(not(all(forge_shared_terminal, target_os = "linux")), test))]
        if let Some(term) = terminal {
            term.search_set_regex(None::<&vte4::Regex>, 0);
        }
        let text = self.search_entry.text();
        if text.is_empty() {
            // `search_changed` also fires when the user deletes the query.  Clear
            // both search backends here; otherwise the previous highlights stay
            // painted until the search bar itself is closed.
            if let Some(term_view) = term_view {
                term_view.clear_find();
            }
            self.search_status.set_text("");
            return;
        }
        if query_exceeds_byte_limit(&text) {
            if let Some(term_view) = term_view {
                term_view.clear_find();
            }
            self.search_status.set_text(&format!(
                "Query too long ({} KiB limit)",
                SEARCH_QUERY_BYTE_LIMIT / 1024
            ));
            return;
        }

        // Detect regex pattern: /pattern/ syntax
        let text_str = text.as_str();
        let (query, use_regex) =
            if text_str.starts_with('/') && text_str.ends_with('/') && text_str.len() > 2 {
                (text_str[1..text_str.len() - 1].to_string(), true)
            } else {
                (text_str.to_string(), false)
            };

        #[cfg(all(forge_shared_terminal, target_os = "linux"))]
        if let Some(pane) = shared {
            if let Some(find) = pane.find_controller() {
                let status = self.search_status.downgrade();
                let generation = self.search_generation.clone();
                let installed = generation.get();
                let source = Rc::downgrade(pane);
                find.set_status_handler(move |result| {
                    if generation.get() == installed
                        && source.upgrade().is_some_and(|pane| pane.root().is_mapped())
                    {
                        if let Some(status) = status.upgrade() {
                            status.set_text(&shared_status_text(result));
                        }
                    }
                });
                find.set_query(&query, use_regex, false);
                self.search_status
                    .set_text(&shared_status_text(find.status()));
            } else {
                self.search_status
                    .set_text("The search pane is no longer available");
            }
            return;
        }

        // Block mode: highlight every in-text match and focus the first one
        // (Warp's FindWithinBlock). Next/Prev step through them.
        if let Some(term_view) = term_view {
            let result = term_view.find_in_blocks(&query, use_regex);
            match result {
                FindSearchResult::Matches(_)
                | FindSearchResult::InvalidRegex
                | FindSearchResult::ScanLimit => {
                    self.search_status.set_text(&find_status_text(result));
                    return;
                }
                FindSearchResult::NoMatches => {}
            }
        }

        // Fall back to the live VTE for prompts/classic terminal panes. VTE can
        // report whether navigation found a hit, but it does not expose a count.
        #[cfg(any(not(all(forge_shared_terminal, target_os = "linux")), test))]
        {
            if let Some(term) = terminal {
                let pattern = if use_regex {
                    query
                } else {
                    regex::escape(&query)
                };
                match vte4::Regex::for_search(
                    &pattern,
                    pcre2_sys::PCRE2_CASELESS | pcre2_sys::PCRE2_MULTILINE,
                ) {
                    Ok(regex) => {
                        term.search_set_regex(Some(&regex), 0);
                        term.search_set_wrap_around(true);
                        let found = term.search_find_next();
                        self.search_status.set_text(terminal_status_text(found));
                    }
                    Err(_) => {
                        self.search_status
                            .set_text(&find_status_text(FindSearchResult::InvalidRegex));
                    }
                }
            } else {
                self.search_status
                    .set_text(&find_status_text(FindSearchResult::NoMatches));
            }
        }
        #[cfg(all(forge_shared_terminal, target_os = "linux", not(test)))]
        self.search_status
            .set_text(&find_status_text(FindSearchResult::NoMatches));
    }

    pub(crate) fn search_next(&self) {
        if !self.search_bar.is_search_mode() {
            return;
        }
        if self.search_debounce_source.borrow().is_some() || !self.search_owner_is_current() {
            self.search_apply();
            return;
        }
        #[cfg(all(forge_shared_terminal, target_os = "linux"))]
        if let Some(pane) = self.shared_search_target() {
            if let Some(find) = pane.find_controller() {
                find.next();
            }
            return;
        }
        if let Some(term_view) = self.search_view() {
            match term_view.find_next() {
                FindNavigationResult::Progress(progress) => {
                    self.search_status.set_text(&progress_status_text(progress));
                    return;
                }
                FindNavigationResult::Invalidated => {
                    // The card set moved under the search: a pane resize, an
                    // Expand, a filter, or a block that was removed. Rebuild
                    // the pass from the query the entry still holds instead of
                    // reporting "No matches" for text that is still on screen.
                    self.search_apply();
                    return;
                }
                FindNavigationResult::Inactive => {}
            }
        }
        // Without an installed native query, false means no search ran. Keep
        // the empty-query, validation-error or scan-limit feedback intact.
        #[cfg(any(not(all(forge_shared_terminal, target_os = "linux")), test))]
        if let Some(term) = self
            .native_search_target()
            .filter(|term| term.search_get_regex().is_some())
        {
            self.search_status
                .set_text(terminal_status_text(term.search_find_next()));
        }
    }

    pub(crate) fn search_prev(&self) {
        if !self.search_bar.is_search_mode() {
            return;
        }
        if self.search_debounce_source.borrow().is_some() || !self.search_owner_is_current() {
            self.search_apply();
            return;
        }
        #[cfg(all(forge_shared_terminal, target_os = "linux"))]
        if let Some(pane) = self.shared_search_target() {
            if let Some(find) = pane.find_controller() {
                find.previous();
            }
            return;
        }
        if let Some(term_view) = self.search_view() {
            match term_view.find_prev() {
                FindNavigationResult::Progress(progress) => {
                    self.search_status.set_text(&progress_status_text(progress));
                    return;
                }
                FindNavigationResult::Invalidated => {
                    // See `search_next`: rebuild rather than claim no matches.
                    self.search_apply();
                    return;
                }
                FindNavigationResult::Inactive => {}
            }
        }
        // Without an installed native query, false means no search ran. Keep
        // the empty-query, validation-error or scan-limit feedback intact.
        #[cfg(any(not(all(forge_shared_terminal, target_os = "linux")), test))]
        if let Some(term) = self
            .native_search_target()
            .filter(|term| term.search_get_regex().is_some())
        {
            self.search_status
                .set_text(terminal_status_text(term.search_find_previous()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{find_status_text, query_exceeds_byte_limit, terminal_status_text};
    use crate::block_view::{FindProgress, FindSearchResult};

    #[cfg(all(forge_shared_terminal, target_os = "linux"))]
    #[test]
    fn shared_find_never_reports_a_limited_empty_pass_as_complete() {
        use jterm_gtk_view::TerminalFindStatus;
        assert_eq!(
            super::shared_status_text(TerminalFindStatus::Results {
                current: 0,
                total: 0,
                truncated: true,
            }),
            "Search incomplete (scan limit)",
        );
        assert_eq!(
            super::shared_status_text(TerminalFindStatus::Results {
                current: 0,
                total: 0,
                truncated: false,
            }),
            "No matches",
        );
    }

    #[test]
    fn search_status_distinguishes_empty_invalid_exact_and_capped_results() {
        assert_eq!(find_status_text(FindSearchResult::NoMatches), "No matches");
        assert_eq!(
            find_status_text(FindSearchResult::InvalidRegex),
            "Invalid regex"
        );
        assert_eq!(
            find_status_text(FindSearchResult::Matches(FindProgress {
                current: 3,
                total: 8,
                capped: false,
                scan_limited: false,
            })),
            "3 of 8"
        );
        assert_eq!(
            find_status_text(FindSearchResult::Matches(FindProgress {
                current: 7,
                total: 10_000,
                capped: true,
                scan_limited: false,
            })),
            "7 of 10000+"
        );
        assert_eq!(
            find_status_text(FindSearchResult::ScanLimit),
            "Search incomplete (scan limit)"
        );
        assert_eq!(
            find_status_text(FindSearchResult::Matches(FindProgress {
                current: 4,
                total: 27,
                capped: true,
                scan_limited: true,
            })),
            "4 of 27+ (scan limit)"
        );
    }

    #[test]
    fn classic_terminal_status_is_an_honest_lower_bound() {
        assert_eq!(terminal_status_text(true), "1 of 1+");
        assert_eq!(terminal_status_text(false), "No matches");
    }

    #[test]
    fn query_byte_limit_accepts_exact_and_rejects_one_over() {
        assert!(!query_exceeds_byte_limit(&"x".repeat(8 * 1024)));
        assert!(query_exceeds_byte_limit(&"x".repeat(8 * 1024 + 1)));
    }
}
