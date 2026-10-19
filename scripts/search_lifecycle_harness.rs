//! Executes the checkout's complete find-bar controller with deterministic
//! GTK/VTE boundaries. This tests routing, cancellation and installed-query
//! ownership; it is not a native GTK rendering or regex-engine test.
#![allow(dead_code, unused_imports)]
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
mod gtk4 {
    pub use crate::glib;
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Widget(pub usize);
    impl Widget {
        pub fn is_ancestor(&self, bar: &super::SearchBar) -> bool {
            // The entry is the only descendant in this fixture's widget tree.
            self == &bar.entry_widget
        }
    }
}
// The production closure body remains unchanged; only GLib's boxing boundary
// is replaced so no native signal, widget, or display is required.
macro_rules! closure_local {
    ($callback:expr) => {
        $callback
    };
}
pub(crate) use closure_local;
pub mod glib {
    use super::*;
    pub(crate) use crate::closure_local;
    pub struct WeakRef<T>(pub Rc<dyn Fn() -> Option<T>>);
    impl<T> WeakRef<T> {
        pub fn upgrade(&self) -> Option<T> {
            (self.0)()
        }
    }
    type Callback = Box<dyn FnOnce()>;
    thread_local! { static SOURCES: RefCell<Vec<Option<Callback>>> = const { RefCell::new(Vec::new()) }; }
    pub struct SourceId(usize);
    impl SourceId {
        pub fn remove(self) {
            SOURCES.with(|s| {
                s.borrow_mut()[self.0].take();
            });
        }
    }
    pub fn timeout_add_local_once(_: Duration, f: impl FnOnce() + 'static) -> SourceId {
        SOURCES.with(|s| {
            let mut s = s.borrow_mut();
            let i = s.len();
            s.push(Some(Box::new(f)));
            SourceId(i)
        })
    }
    pub fn run_pending() {
        loop {
            let next = SOURCES.with(|s| {
                let mut s = s.borrow_mut();
                s.iter_mut().find_map(Option::take)
            });
            match next {
                Some(f) => f(),
                None => break,
            }
        }
    }
}
mod pcre2_sys {
    pub const PCRE2_CASELESS: u32 = 1;
    pub const PCRE2_MULTILINE: u32 = 2;
}
mod regex {
    pub fn escape(s: &str) -> String {
        s.to_owned()
    }
}
mod vte4 {
    use super::*;
    pub struct Regex(String);
    impl Regex {
        pub fn for_search(s: &str, _: u32) -> Result<Self, ()> {
            if s == "[" {
                Err(())
            } else {
                Ok(Self(s.to_owned()))
            }
        }
    }
    #[derive(Default)]
    pub struct TerminalState {
        pub installed: Rc<RefCell<Option<String>>>,
        pub steps: Rc<Cell<usize>>,
    }
    #[derive(Clone, Default)]
    pub struct Terminal(Rc<TerminalState>);
    impl std::ops::Deref for Terminal {
        type Target = TerminalState;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
    impl PartialEq for Terminal {
        fn eq(&self, other: &Self) -> bool {
            Rc::ptr_eq(&self.0, &other.0)
        }
    }
    impl Terminal {
        pub fn downgrade(&self) -> glib::WeakRef<Self> {
            let weak = Rc::downgrade(&self.0);
            glib::WeakRef(Rc::new(move || weak.upgrade().map(Self)))
        }
        pub fn search_set_regex(&self, re: Option<&Regex>, _: u32) {
            *self.installed.borrow_mut() = re.map(|r| r.0.clone());
        }
        pub fn search_set_wrap_around(&self, _: bool) {}
        pub fn search_find_next(&self) -> bool {
            if self.installed.borrow().is_some() {
                self.steps.set(self.steps.get() + 1);
                true
            } else {
                false
            }
        }
        pub fn search_find_previous(&self) -> bool {
            self.search_find_next()
        }
    }
}
mod block_view {
    use super::*;
    #[derive(Clone, Copy, Debug)]
    pub struct FindProgress {
        pub current: usize,
        pub total: usize,
        pub capped: bool,
        pub scan_limited: bool,
    }
    #[derive(Clone, Copy, Debug)]
    pub enum FindSearchResult {
        NoMatches,
        InvalidRegex,
        ScanLimit,
        Matches(FindProgress),
    }
    #[derive(Clone, Copy, Debug)]
    pub enum FindNavigationResult {
        Inactive,
        Invalidated,
        Progress(FindProgress),
    }
    #[derive(Clone)]
    pub struct TermView {
        pub result: FindSearchResult,
        pub queries: Rc<RefCell<Vec<String>>>,
        pub cleared: Rc<Cell<usize>>,
        pub revealed: Rc<Cell<usize>>,
        pub terminal: vte4::Terminal,
        pub root: MappedWidget,
        pub focused: Rc<Cell<usize>>,
    }
    #[derive(Clone)]
    pub struct MappedWidget(pub Rc<Cell<bool>>);
    impl MappedWidget {
        pub fn is_mapped(&self) -> bool {
            self.0.get()
        }
    }
    impl Default for TermView {
        fn default() -> Self {
            Self {
                result: FindSearchResult::NoMatches,
                queries: Default::default(),
                cleared: Default::default(),
                revealed: Default::default(),
                terminal: Default::default(),
                root: MappedWidget(Rc::new(Cell::new(true))),
                focused: Default::default(),
            }
        }
    }
    impl TermView {
        pub fn vte(&self) -> &vte4::Terminal {
            &self.terminal
        }
        pub fn widget(&self) -> &MappedWidget {
            &self.root
        }
        pub fn grab_focus(&self) {
            self.focused.set(self.focused.get() + 1);
        }
        pub fn clear_find(&self) {
            self.cleared.set(self.cleared.get() + 1)
        }
        pub fn find_in_blocks(&self, s: &str, _: bool) -> FindSearchResult {
            self.queries.borrow_mut().push(s.to_owned());
            self.result
        }
        pub fn find_next(&self) -> FindNavigationResult {
            FindNavigationResult::Inactive
        }
        pub fn find_prev(&self) -> FindNavigationResult {
            FindNavigationResult::Inactive
        }
        pub fn reveal_live_input(&self) {
            self.revealed.set(self.revealed.get() + 1);
        }
    }
}
#[derive(Clone, Default)]
struct Entry {
    text: Rc<RefCell<String>>,
    focus_count: Rc<Cell<usize>>,
    focus_events: Rc<RefCell<Vec<&'static str>>>,
    focus_target: Option<(FocusRoot, gtk4::Widget)>,
}
impl Entry {
    fn text(&self) -> String {
        self.text.borrow().clone()
    }
    fn grab_focus(&self) {
        self.focus_count.set(self.focus_count.get() + 1);
        self.focus_events.borrow_mut().push("entry-focus");
        if let Some((root, widget)) = &self.focus_target {
            *root.0.borrow_mut() = Some(widget.clone());
        }
    }
    fn set_text(&self, s: &str) {
        *self.text.borrow_mut() = s.into();
    }
}
#[derive(Clone, Default)]
struct FocusRoot(Rc<RefCell<Option<gtk4::Widget>>>);
impl FocusRoot {
    fn focus(&self) -> Option<gtk4::Widget> {
        self.0.borrow().clone()
    }
}
#[derive(Clone)]
struct SearchBar {
    mode: Rc<Cell<bool>>,
    root: Option<FocusRoot>,
    widget: gtk4::Widget,
    entry_widget: gtk4::Widget,
}
impl Default for SearchBar {
    fn default() -> Self {
        Self {
            mode: Default::default(),
            root: Some(FocusRoot::default()),
            widget: gtk4::Widget(usize::MAX),
            entry_widget: gtk4::Widget(usize::MAX - 1),
        }
    }
}
impl AsRef<gtk4::Widget> for SearchBar {
    fn as_ref(&self) -> &gtk4::Widget {
        &self.widget
    }
}
impl SearchBar {
    fn is_search_mode(&self) -> bool {
        self.mode.get()
    }
    fn set_search_mode(&self, b: bool) {
        self.mode.set(b)
    }
    fn root(&self) -> Option<FocusRoot> {
        self.root.clone()
    }
    fn upcast_ref<T>(&self) -> &T
    where
        Self: AsRef<T>,
    {
        self.as_ref()
    }
}
#[derive(Clone, Default)]
struct Label(Rc<RefCell<String>>);
impl Label {
    fn set_text(&self, s: &str) {
        *self.0.borrow_mut() = s.into()
    }
}
type SearchEntry = Entry;
// @search-lifecycle:focus-helper
// @search-lifecycle:after-helper

type SwitchCallback = Rc<dyn Fn(Notebook, gtk4::Widget, u32)>;
#[derive(Clone, Default)]
struct Notebook {
    current: Rc<Cell<Option<u32>>>,
    pages: Rc<RefCell<Vec<gtk4::Widget>>>,
    after: Rc<Cell<bool>>,
    callback: Rc<RefCell<Option<SwitchCallback>>>,
}
impl Notebook {
    fn connect_closure(
        &self,
        signal: &str,
        after: bool,
        callback: impl Fn(Notebook, gtk4::Widget, u32) + 'static,
    ) {
        assert_eq!(signal, "switch-page");
        self.after.set(after);
        *self.callback.borrow_mut() = Some(Rc::new(callback));
    }
    fn current_page(&self) -> Option<u32> {
        self.current.get()
    }
    fn nth_page(&self, page: Option<u32>) -> Option<gtk4::Widget> {
        self.pages.borrow().get(page? as usize).cloned()
    }
    fn emit(&self, page: gtk4::Widget, page_num: u32) {
        let callback = self.callback.borrow().clone().unwrap();
        callback(self.clone(), page, page_num);
    }
}

mod ui {
    use super::*;
    use block_view::TermView;
    #[derive(Clone)]
    pub struct UiState {
        search_bar: SearchBar,
        search_entry: Entry,
        search_status: Label,
        search_generation: Rc<Cell<u64>>,
        search_owner: Rc<RefCell<Option<search::SearchOwner>>>,
        search_debounce_source: Rc<RefCell<Option<glib::SourceId>>>,
        current: Rc<Cell<usize>>,
        terminals: Vec<vte4::Terminal>,
        views: Vec<Option<Rc<TermView>>>,
        tab_focus_generation: Rc<Cell<u64>>,
        terminal_focus_requests: Rc<RefCell<Vec<(vte4::Terminal, u32)>>>,
        current_terminal_focuses: Rc<Cell<usize>>,
    }
    impl UiState {
        fn current_terminal(&self) -> Option<vte4::Terminal> {
            self.terminals.get(self.current.get()).cloned()
        }
        fn current_term_view(&self) -> Option<Rc<TermView>> {
            self.views.get(self.current.get()).cloned().flatten()
        }
        fn terminal_in_page(&self, w: &gtk4::Widget) -> Option<vte4::Terminal> {
            self.terminals.get(w.0).cloned()
        }
        fn term_view_in_page(&self, w: &gtk4::Widget) -> Option<Rc<TermView>> {
            self.views.get(w.0).cloned().flatten()
        }
        fn focus_current_terminal(&self) {
            self.current_terminal_focuses
                .set(self.current_terminal_focuses.get() + 1);
        }
        fn invalidate_tab_focus_requests(&self) -> u64 {
            let generation = self.tab_focus_generation.get().wrapping_add(1);
            self.tab_focus_generation.set(generation);
            generation
        }
        fn request_tab_terminal_focus(&self, terminal: vte4::Terminal, page_num: u32) {
            self.invalidate_tab_focus_requests();
            self.terminal_focus_requests
                .borrow_mut()
                .push((terminal, page_num));
        }
    }
    mod search {
        // @search-lifecycle:source
    }
    fn setup() -> UiState {
        let search_bar = SearchBar::default();
        search_bar.set_search_mode(true);
        let search_entry = Entry {
            focus_target: Some((
                search_bar.root.clone().unwrap(),
                search_bar.entry_widget.clone(),
            )),
            ..Entry::default()
        };
        UiState {
            search_bar,
            search_entry,
            search_status: Label::default(),
            search_generation: Rc::new(Cell::new(0)),
            search_owner: Rc::new(RefCell::new(None)),
            search_debounce_source: Rc::new(RefCell::new(None)),
            current: Rc::new(Cell::new(0)),
            terminals: vec![vte4::Terminal::default(), vte4::Terminal::default()],
            views: vec![None, None],
            tab_focus_generation: Rc::new(Cell::new(0)),
            terminal_focus_requests: Rc::new(RefCell::new(Vec::new())),
            current_terminal_focuses: Rc::new(Cell::new(0)),
        }
    }
    fn switch_page(ui: &UiState, target: usize) {
        let ui_for_switch = ui;
        let widget = &gtk4::Widget(target);
        let page_num = target as u32;
        // Deliberately leave current_page stale until after the production call:
        // even before GTK commits selection, routing must honor its explicit
        // destination. Native tests separately cover after-handler ordering.
        // @search-lifecycle:switch
        ui.current.set(target);
    }

    fn setup_notebook(ui: &UiState) -> Notebook {
        let notebook = Notebook::default();
        *notebook.pages.borrow_mut() = vec![gtk4::Widget(0), gtk4::Widget(1)];
        notebook.current.set(Some(ui.current.get() as u32));
        let search_bar = ui.search_bar.clone();
        let search_entry = ui.search_entry.clone();
        // @search-lifecycle:after-registration
        notebook
    }

    #[test]
    fn pane_find_pins_edits_and_steps_until_an_explicit_page_switch() {
        let mut ui = setup();
        let owner = Rc::new(TermView {
            terminal: ui.terminals[0].clone(),
            ..TermView::default()
        });
        let sibling = Rc::new(TermView {
            terminal: ui.terminals[1].clone(),
            ..TermView::default()
        });
        ui.views = vec![Some(owner.clone()), Some(sibling.clone())];
        ui.current.set(1);
        ui.search_entry.set_text("captured");
        ui.show_search_for(&owner);
        assert_eq!(*ui.terminals[0].installed.borrow(), Some("captured".into()));
        assert!(ui.terminals[1].installed.borrow().is_none());
        ui.search_entry.set_text("edited");
        ui.schedule_search_apply();
        glib::run_pending();
        let steps = ui.terminals[0].steps.get();
        ui.search_next();
        ui.search_prev();
        assert_eq!(ui.terminals[0].steps.get(), steps + 2);
        assert_eq!(owner.queries.borrow().as_slice(), ["captured", "edited"]);
        assert!(sibling.queries.borrow().is_empty());
        assert!(ui.terminals[1].installed.borrow().is_none());
        switch_page(&ui, 1);
        assert!(ui.terminals[0].installed.borrow().is_none());
        assert_eq!(*ui.terminals[1].installed.borrow(), Some("edited".into()));
        assert_eq!(sibling.queries.borrow().as_slice(), ["edited"]);
    }

    #[test]
    fn hidden_or_destroyed_pinned_owner_never_retargets_its_sibling() {
        for hidden in [true, false] {
            let mut ui = setup();
            let owner = Rc::new(TermView {
                terminal: ui.terminals[0].clone(),
                ..TermView::default()
            });
            let weak = Rc::downgrade(&owner);
            ui.views[0] = Some(owner.clone());
            ui.search_entry.set_text("old");
            ui.show_search_for(&owner);
            ui.current.set(1);
            if hidden {
                owner.root.0.set(false);
            } else {
                ui.views[0] = None;
            }
            drop(owner);
            assert_eq!(weak.upgrade().is_some(), hidden);
            ui.search_entry.set_text("must-not-rebind");
            ui.schedule_search_apply();
            glib::run_pending();
            ui.search_next();
            ui.search_prev();
            assert_eq!(
                ui.search_status.0.borrow().as_str(),
                "The search pane is no longer available"
            );
            assert!(ui
                .terminals
                .iter()
                .all(|term| term.installed.borrow().is_none()));
            assert_eq!(ui.terminals[1].steps.get(), 0);
            ui.toggle_search();
            assert_eq!(ui.current_terminal_focuses.get(), 0);
        }
    }

    #[test]
    fn closing_pinned_search_restores_only_focus_still_owned_by_the_bar() {
        // Direct bar focus, its entry, a newer sibling, no focus, and unrooting
        // exercise both ownership predicates without native GTK side effects.
        for focus in 0..5 {
            let mut ui = setup();
            let owner = Rc::new(TermView {
                terminal: ui.terminals[0].clone(),
                ..TermView::default()
            });
            ui.views[0] = Some(owner.clone());
            ui.search_entry.set_text("owner");
            ui.show_search_for(&owner);
            ui.current.set(1);
            if focus == 4 {
                ui.search_bar.root = None;
            } else {
                *ui.search_bar.root.as_ref().unwrap().0.borrow_mut() = match focus {
                    0 => Some(ui.search_bar.widget.clone()),
                    1 => Some(ui.search_bar.entry_widget.clone()),
                    2 => Some(gtk4::Widget(42)),
                    _ => None,
                };
            }
            ui.toggle_search();
            assert_eq!(owner.focused.get(), usize::from(focus < 2));
            assert_eq!(ui.current_terminal_focuses.get(), 0);
            assert!(ui.search_owner.borrow().is_none());
            assert!(ui
                .terminals
                .iter()
                .all(|term| term.installed.borrow().is_none()));
        }
    }

    #[test]
    fn after_switch_reconciles_mapping_synchronously_without_later_focus_theft() {
        let ui = setup();
        let notebook = setup_notebook(&ui);
        assert!(notebook.after.get(), "focus handler ran before GTK mapping");
        ui.search_entry.set_text("needle");
        switch_page(&ui, 1);
        // The deterministic native boundary models the RUN_LAST mapping step.
        // Both focus decisions on either side are extracted production code.
        ui.search_entry
            .focus_events
            .borrow_mut()
            .push("mapped-terminal-focus");
        notebook.current.set(Some(1));
        notebook.emit(gtk4::Widget(1), 1);
        assert_eq!(
            ui.search_entry.focus_events.borrow().as_slice(),
            ["entry-focus", "mapped-terminal-focus", "entry-focus"]
        );
        assert_eq!(*ui.terminals[1].installed.borrow(), Some("needle".into()));
        ui.search_entry
            .focus_events
            .borrow_mut()
            .push("newer-user-focus");
        glib::run_pending();
        assert_eq!(ui.search_entry.focus_count.get(), 2);
        assert_eq!(
            ui.search_entry.focus_events.borrow().last(),
            Some(&"newer-user-focus")
        );
    }

    #[test]
    fn after_switch_ignores_closed_search_stale_pages_and_replaced_widgets() {
        let ui = setup();
        let notebook = setup_notebook(&ui);
        notebook.current.set(Some(1));
        ui.search_bar.set_search_mode(false);
        notebook.emit(gtk4::Widget(1), 1);
        ui.search_bar.set_search_mode(true);
        notebook.emit(gtk4::Widget(0), 0);
        notebook.emit(gtk4::Widget(99), 1);
        notebook.current.set(None);
        notebook.emit(gtk4::Widget(1), 1);
        notebook.current.set(Some(2));
        notebook.emit(gtk4::Widget(2), 2);
        assert_eq!(ui.search_entry.focus_count.get(), 0);
        notebook.current.set(Some(1));
        notebook.emit(gtk4::Widget(1), 1);
        assert_eq!(ui.search_entry.focus_count.get(), 1);
        assert!(ui.terminal_focus_requests.borrow().is_empty());
    }

    #[test]
    fn search_focus_invalidates_then_applies_before_focusing_entry() {
        let ui = setup();
        let events = &ui.search_entry.focus_events;
        focus_switched_page(
            &ui.search_bar,
            &ui.search_entry,
            || events.borrow_mut().push("invalidate"),
            || events.borrow_mut().push("apply"),
            || panic!("search must not resolve a terminal-focus target"),
            |_| panic!("search must not request terminal focus"),
        );
        assert_eq!(
            events.borrow().as_slice(),
            ["invalidate", "apply", "entry-focus"]
        );
    }

    #[test]
    fn closed_search_switch_focuses_explicit_destination_and_reveals_live_input() {
        let mut ui = setup();
        ui.search_bar.set_search_mode(false);
        ui.search_entry.set_text("must-not-apply");
        let view = Rc::new(TermView {
            result: block_view::FindSearchResult::NoMatches,
            queries: Rc::new(RefCell::new(Vec::new())),
            cleared: Rc::new(Cell::new(0)),
            revealed: Rc::new(Cell::new(0)),
            ..TermView::default()
        });
        ui.views[1] = Some(view.clone());
        switch_page(&ui, 1);
        let requests = ui.terminal_focus_requests.borrow();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].0 == ui.terminals[1]);
        assert_eq!(requests[0].1, 1);
        assert_eq!(view.revealed.get(), 1);
        assert_eq!(ui.tab_focus_generation.get(), 1);
        assert_eq!(ui.search_entry.focus_count.get(), 0);
        assert!(ui.search_owner.borrow().is_none());
        assert!(ui.terminals.iter().all(|t| t.installed.borrow().is_none()));
    }

    #[test]
    fn closed_search_switch_without_terminal_invalidates_previous_focus_request() {
        let ui = setup();
        ui.search_bar.set_search_mode(false);
        ui.tab_focus_generation.set(7);
        switch_page(&ui, 2);
        assert_eq!(ui.tab_focus_generation.get(), 8);
        assert_eq!(ui.search_entry.focus_count.get(), 0);
        assert!(ui.terminal_focus_requests.borrow().is_empty());
        assert!(ui.search_owner.borrow().is_none());
    }

    #[test]
    fn repeated_switches_leave_only_the_latest_search_owner() {
        let ui = setup();
        ui.search_entry.set_text("latest");
        ui.schedule_search_apply();
        switch_page(&ui, 1);
        switch_page(&ui, 0);
        assert_eq!(*ui.terminals[0].installed.borrow(), Some("latest".into()));
        assert!(ui.terminals[1].installed.borrow().is_none());
        assert_eq!(ui.search_entry.focus_count.get(), 2);
        assert_eq!(ui.tab_focus_generation.get(), 2);
        assert!(ui.terminal_focus_requests.borrow().is_empty());
        let steps = ui.terminals[0].steps.get();
        glib::run_pending();
        assert_eq!(ui.terminals[0].steps.get(), steps);
        assert_eq!(ui.search_entry.focus_count.get(), 2);
    }

    #[test]
    fn closing_search_after_switch_does_not_refocus_entry_later() {
        let ui = setup();
        ui.search_entry.set_text("needle");
        ui.schedule_search_apply();
        switch_page(&ui, 1);
        ui.toggle_search();
        let focuses = ui.search_entry.focus_count.get();
        glib::run_pending();
        assert_eq!(ui.search_entry.focus_count.get(), focuses);
        assert!(ui.terminals.iter().all(|t| t.installed.borrow().is_none()));
        assert!(ui.search_owner.borrow().is_none());
    }

    #[test]
    fn invalid_regex_removes_previous_native_search() {
        let ui = setup();
        ui.search_entry.set_text("needle");
        ui.search_apply();
        assert_eq!(*ui.terminals[0].installed.borrow(), Some("needle".into()));
        ui.search_entry.set_text("/[/");
        ui.search_apply();
        assert_eq!(
            *ui.terminals[0].installed.borrow(),
            None,
            "invalid query retained old regex"
        );
        let count = ui.terminals[0].steps.get();
        ui.search_next();
        assert_eq!(
            ui.terminals[0].steps.get(),
            count,
            "Enter navigated an obsolete query"
        );
    }
    #[test]
    fn block_early_returns_retire_previous_live_query() {
        for result in [
            block_view::FindSearchResult::InvalidRegex,
            block_view::FindSearchResult::ScanLimit,
            block_view::FindSearchResult::Matches(block_view::FindProgress {
                current: 1,
                total: 2,
                capped: false,
                scan_limited: false,
            }),
        ] {
            let mut ui = setup();
            ui.search_entry.set_text("old-live-query");
            ui.search_apply();
            assert!(ui.terminals[0].installed.borrow().is_some());
            ui.views[0] = Some(Rc::new(TermView {
                result,
                queries: Rc::new(RefCell::new(Vec::new())),
                cleared: Rc::new(Cell::new(0)),
                revealed: Rc::new(Cell::new(0)),
                ..TermView::default()
            }));
            ui.search_entry.set_text("new-block-query");
            ui.search_apply();
            assert!(ui.terminals[0].installed.borrow().is_none(), "{result:?}");
            let steps = ui.terminals[0].steps.get();
            // The test boundary deliberately makes Block navigation inactive;
            // even then neither direction may fall back to the obsolete regex.
            ui.search_next();
            ui.search_prev();
            assert_eq!(ui.terminals[0].steps.get(), steps, "{result:?}");
        }
    }

    #[test]
    fn switch_page_applies_to_explicit_destination_before_notebook_commits() {
        let ui = setup();
        ui.search_entry.set_text("needle");
        ui.search_apply();
        switch_page(&ui, 1);
        assert_eq!(
            *ui.terminals[1].installed.borrow(),
            Some("needle".into()),
            "destination never received the query"
        );
        assert_eq!(ui.tab_focus_generation.get(), 1);
        assert_eq!(ui.search_entry.focus_count.get(), 1);
        assert!(ui.terminal_focus_requests.borrow().is_empty());
    }
    #[test]
    fn switch_page_cancels_pending_query_debounce() {
        let ui = setup();
        ui.search_entry.set_text("new");
        ui.schedule_search_apply();
        switch_page(&ui, 1);
        assert!(ui.search_debounce_source.borrow().is_none());
        let steps = ui.terminals[1].steps.get();
        glib::run_pending();
        assert_eq!(ui.terminals[1].steps.get(), steps);
        assert_eq!(*ui.terminals[1].installed.borrow(), Some("new".into()));
    }
    #[test]
    fn empty_query_on_switch_clears_destination() {
        let ui = setup();
        ui.current.set(1);
        ui.search_entry.set_text("old");
        ui.search_apply();
        ui.current.set(0);
        ui.search_entry.set_text("");
        switch_page(&ui, 1);
        assert!(ui.terminals[1].installed.borrow().is_none());
    }
    #[test]
    fn block_search_on_switch_uses_destination_controller() {
        let mut ui = setup();
        let view = Rc::new(TermView {
            result: block_view::FindSearchResult::Matches(block_view::FindProgress {
                current: 1,
                total: 2,
                capped: false,
                scan_limited: false,
            }),
            queries: Rc::new(RefCell::new(Vec::new())),
            cleared: Rc::new(Cell::new(0)),
            revealed: Rc::new(Cell::new(0)),
            ..TermView::default()
        });
        ui.views[1] = Some(view.clone());
        ui.search_entry.set_text("needle");
        switch_page(&ui, 1);
        assert_eq!(view.queries.borrow().as_slice(), ["needle"]);
        assert_eq!(*ui.search_status.0.borrow(), "1 of 2");
        assert!(ui.terminals[0].installed.borrow().is_none());
        assert_eq!(ui.search_entry.focus_count.get(), 1);
        assert_eq!(view.revealed.get(), 0, "search revealed live input");
    }
    #[test]
    fn same_query_moves_between_split_owners_before_navigation() {
        let ui = setup();
        ui.search_entry.set_text("same-query");
        ui.search_apply();
        ui.current.set(1);
        ui.search_next();
        assert!(ui.terminals[0].installed.borrow().is_none());
        assert_eq!(
            *ui.terminals[1].installed.borrow(),
            Some("same-query".into())
        );
        assert_eq!(ui.terminals[1].steps.get(), 1);
        ui.search_next();
        assert_eq!(ui.terminals[1].steps.get(), 2);
        ui.current.set(0);
        ui.search_prev();
        assert!(ui.terminals[1].installed.borrow().is_none());
        assert_eq!(
            *ui.terminals[0].installed.borrow(),
            Some("same-query".into())
        );
        assert_eq!(ui.terminals[0].steps.get(), 2);
    }

    #[test]
    fn close_clears_the_installed_owner_after_split_focus_changes() {
        let mut ui = setup();
        let view = Rc::new(TermView {
            result: block_view::FindSearchResult::NoMatches,
            queries: Rc::new(RefCell::new(Vec::new())),
            cleared: Rc::new(Cell::new(0)),
            revealed: Rc::new(Cell::new(0)),
            ..TermView::default()
        });
        ui.views[0] = Some(view.clone());
        ui.search_entry.set_text("owner-query");
        ui.search_apply();
        let cleared = view.cleared.get();
        ui.current.set(1);
        *ui.terminals[1].installed.borrow_mut() = Some("unrelated-query".into());
        ui.toggle_search();
        assert!(ui.terminals[0].installed.borrow().is_none());
        assert_eq!(view.cleared.get(), cleared + 1);
        assert_eq!(
            *ui.terminals[1].installed.borrow(),
            Some("unrelated-query".into())
        );
        assert!(ui.search_owner.borrow().is_none());
    }

    #[test]
    fn destroyed_search_owner_is_not_retained_or_confused_with_replacement() {
        let mut ui = setup();
        let view = Rc::new(TermView {
            result: block_view::FindSearchResult::NoMatches,
            queries: Rc::new(RefCell::new(Vec::new())),
            cleared: Rc::new(Cell::new(0)),
            revealed: Rc::new(Cell::new(0)),
            ..TermView::default()
        });
        let weak_view = Rc::downgrade(&view);
        let weak_terminal = ui.terminals[0].downgrade();
        ui.views[0] = Some(view);
        ui.search_entry.set_text("retired-query");
        ui.search_apply();
        ui.views[0] = None;
        ui.terminals[0] = vte4::Terminal::default();
        assert!(weak_view.upgrade().is_none());
        assert!(weak_terminal.upgrade().is_none());
        ui.search_next();
        assert_eq!(
            *ui.terminals[0].installed.borrow(),
            Some("retired-query".into())
        );
        assert_eq!(ui.terminals[0].steps.get(), 1);
        ui.toggle_search();
        assert!(ui.terminals[0].installed.borrow().is_none());
    }

    #[test]
    fn newest_debounce_wins() {
        let ui = setup();
        ui.search_entry.set_text("old");
        ui.schedule_search_apply();
        ui.search_entry.set_text("new");
        ui.schedule_search_apply();
        glib::run_pending();
        assert_eq!(*ui.terminals[0].installed.borrow(), Some("new".into()));
        assert!(ui.search_debounce_source.borrow().is_none());
    }
    #[test]
    fn close_removes_scheduled_source_and_native_query() {
        let ui = setup();
        ui.search_entry.set_text("old");
        ui.search_apply();
        ui.schedule_search_apply();
        ui.toggle_search();
        glib::run_pending();
        assert!(ui.terminals[0].installed.borrow().is_none());
        assert!(ui.search_debounce_source.borrow().is_none());
        assert!(ui.search_status.0.borrow().is_empty());
    }
    #[test]
    fn empty_and_oversized_queries_clear_existing_native_search() {
        for query in [String::new(), "x".repeat(8193)] {
            let ui = setup();
            ui.search_entry.set_text("old");
            ui.search_apply();
            ui.search_entry.set_text(&query);
            ui.search_apply();
            assert!(ui.terminals[0].installed.borrow().is_none());
        }
    }

    #[test]
    fn delayed_entry_signal_after_close_cannot_reinstall_search() {
        let ui = setup();
        ui.search_entry.set_text("needle");
        ui.search_apply();
        let steps = ui.terminals[0].steps.get();
        ui.toggle_search();
        ui.schedule_search_apply();
        assert!(ui.search_debounce_source.borrow().is_none());
        assert!(ui.search_status.0.borrow().is_empty());
        glib::run_pending();
        assert!(ui.terminals[0].installed.borrow().is_none());
        assert!(ui.search_owner.borrow().is_none());
        assert_eq!(ui.terminals[0].steps.get(), steps);
    }

    #[test]
    fn hidden_direct_apply_and_navigation_leave_both_backends_idle() {
        let mut ui = setup();
        let view = Rc::new(TermView {
            result: block_view::FindSearchResult::NoMatches,
            queries: Rc::new(RefCell::new(Vec::new())),
            cleared: Rc::new(Cell::new(0)),
            revealed: Rc::new(Cell::new(0)),
            ..TermView::default()
        });
        ui.views[0] = Some(view.clone());
        ui.search_entry.set_text("needle");
        ui.search_apply();
        ui.toggle_search();
        let queries = view.queries.borrow().len();
        let revealed = view.revealed.get();
        let steps = ui.terminals[0].steps.get();
        ui.search_apply();
        ui.search_apply_in_page(&gtk4::Widget(0));
        ui.search_next();
        ui.search_prev();
        assert!(ui.search_debounce_source.borrow().is_none());
        assert!(ui.search_owner.borrow().is_none());
        assert!(ui.search_status.0.borrow().is_empty());
        assert!(ui.terminals[0].installed.borrow().is_none());
        assert_eq!(view.queries.borrow().len(), queries);
        assert_eq!(view.revealed.get(), revealed);
        assert_eq!(ui.terminals[0].steps.get(), steps);
    }

    #[test]
    fn reopen_after_delayed_closed_signal_replays_retained_query_once() {
        let ui = setup();
        ui.search_entry.set_text("old");
        ui.search_apply();
        ui.search_entry.set_text("new");
        ui.schedule_search_apply();
        ui.toggle_search();
        ui.schedule_search_apply();
        glib::run_pending();
        assert!(ui.terminals[0].installed.borrow().is_none());
        let steps = ui.terminals[0].steps.get();
        ui.toggle_search();
        assert_eq!(*ui.search_status.0.borrow(), "Searching…");
        glib::run_pending();
        assert_eq!(*ui.terminals[0].installed.borrow(), Some("new".into()));
        assert_eq!(ui.terminals[0].steps.get(), steps + 1);
        assert!(ui.search_debounce_source.borrow().is_none());
    }
}
