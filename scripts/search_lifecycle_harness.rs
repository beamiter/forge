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
}
pub mod glib {
    use super::*;
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
    #[derive(Clone, Default)]
    pub struct Terminal {
        pub installed: Rc<RefCell<Option<String>>>,
        pub steps: Rc<Cell<usize>>,
    }
    impl Terminal {
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
    }
    impl TermView {
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
    }
}
#[derive(Clone, Default)]
struct Entry(Rc<RefCell<String>>);
impl Entry {
    fn text(&self) -> String {
        self.0.borrow().clone()
    }
    fn grab_focus(&self) {}
    fn set_text(&self, s: &str) {
        *self.0.borrow_mut() = s.into();
    }
}
#[derive(Clone, Default)]
struct SearchBar(Rc<Cell<bool>>);
impl SearchBar {
    fn is_search_mode(&self) -> bool {
        self.0.get()
    }
    fn set_search_mode(&self, b: bool) {
        self.0.set(b)
    }
}
#[derive(Clone, Default)]
struct Label(Rc<RefCell<String>>);
impl Label {
    fn set_text(&self, s: &str) {
        *self.0.borrow_mut() = s.into()
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
        search_debounce_source: Rc<RefCell<Option<glib::SourceId>>>,
        current: Rc<Cell<usize>>,
        terminals: Vec<vte4::Terminal>,
        views: Vec<Option<Rc<TermView>>>,
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
        fn focus_current_terminal(&self) {}
        fn invalidate_tab_focus_requests(&self) {}
    }
    mod search {
        // @search-lifecycle:source
    }
    fn setup() -> UiState {
        UiState {
            search_bar: SearchBar(Rc::new(Cell::new(true))),
            search_entry: Entry::default(),
            search_status: Label::default(),
            search_generation: Rc::new(Cell::new(0)),
            search_debounce_source: Rc::new(RefCell::new(None)),
            current: Rc::new(Cell::new(0)),
            terminals: vec![vte4::Terminal::default(), vte4::Terminal::default()],
            views: vec![None, None],
        }
    }
    fn switch_page(ui: &UiState, target: usize) {
        let ui_for_switch = ui;
        let widget = &gtk4::Widget(target);
        // GTK switch-page is RUN_LAST: the signal argument is the destination,
        // while current_page still refers to the source during this callback.
        // @search-lifecycle:switch
        ui.current.set(target);
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
        });
        ui.views[1] = Some(view.clone());
        ui.search_entry.set_text("needle");
        switch_page(&ui, 1);
        assert_eq!(view.queries.borrow().as_slice(), ["needle"]);
        assert_eq!(*ui.search_status.0.borrow(), "1 of 2");
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
}
