//! Exact tab-range and clear-selection methods with deterministic strip widgets.
//! The fixture tests selected identities/CSS effects, not GTK input delivery.
#![allow(dead_code)]
use std::{
    cell::RefCell,
    collections::HashSet,
    rc::{Rc, Weak},
};
mod gtk4 {
    use super::*;
    #[derive(Clone)]
    pub struct Widget {
        pub id: usize,
        pub name: String,
        pub strip: Weak<RefCell<Vec<Widget>>>,
        pub classes: Rc<RefCell<HashSet<String>>>,
    }
    impl Widget {
        pub fn widget_name(&self) -> String {
            self.name.clone()
        }
        pub fn next_sibling(&self) -> Option<Self> {
            let strip = self.strip.upgrade()?;
            let strip = strip.borrow();
            let i = strip.iter().position(|w| w.id == self.id)?;
            strip.get(i + 1).cloned()
        }
        pub fn downcast<T: From<Widget>>(self) -> Result<T, Self> {
            Ok(T::from(self))
        }
    }
    #[derive(Clone)]
    pub struct ToggleButton(pub Widget);
    impl From<Widget> for ToggleButton {
        fn from(w: Widget) -> Self {
            Self(w)
        }
    }
    impl ToggleButton {
        pub fn widget_name(&self) -> String {
            self.0.widget_name()
        }
        pub fn add_css_class(&self, s: &str) {
            self.0.classes.borrow_mut().insert(s.into());
        }
        pub fn remove_css_class(&self, s: &str) {
            self.0.classes.borrow_mut().remove(s);
        }
    }
}
use gtk4::ToggleButton;
struct Strip(Rc<RefCell<Vec<gtk4::Widget>>>);
impl Strip {
    fn first_child(&self) -> Option<gtk4::Widget> {
        self.0.borrow().first().cloned()
    }
}
struct UiState {
    tab_strip: Strip,
    selected_tabs: RefCell<Vec<String>>,
}
impl UiState {
    // @tab-selection:clear
    // @tab-selection:select
}
fn setup(names: &[&str]) -> UiState {
    let items = Rc::new(RefCell::new(Vec::new()));
    for (i, name) in names.iter().enumerate() {
        items.borrow_mut().push(gtk4::Widget {
            id: i,
            name: (*name).into(),
            strip: Rc::downgrade(&items),
            classes: Rc::new(RefCell::new(HashSet::new())),
        })
    }
    UiState {
        tab_strip: Strip(items),
        selected_tabs: RefCell::new(Vec::new()),
    }
}
fn assert_selection(ui: &UiState, expected: &[&str]) {
    assert_eq!(*ui.selected_tabs.borrow(), expected);
    for w in ui.tab_strip.0.borrow().iter() {
        assert_eq!(
            w.classes.borrow().contains("tab-selected"),
            expected.contains(&w.name.as_str()),
            "CSS mismatch: {}",
            w.name
        )
    }
}
#[test]
fn every_distinct_range_is_symmetric_and_inclusive() {
    let names = ["a", "b", "c", "d", "e"];
    for from in 0..names.len() {
        for to in 0..names.len() {
            let ui = setup(&names);
            ui.select_tab_range(names[from], names[to]);
            assert_selection(&ui, &names[from.min(to)..=from.max(to)]);
        }
    }
}
#[test]
fn reverse_range_never_selects_the_unrelated_right_tail() {
    let ui = setup(&["a", "b", "c", "d", "e"]);
    ui.select_tab_range("d", "b");
    assert_selection(&ui, &["b", "c", "d"]);
}
#[test]
fn same_endpoint_selects_exactly_one_tab() {
    let ui = setup(&["a", "b", "c"]);
    ui.select_tab_range("b", "b");
    assert_selection(&ui, &["b"]);
}
#[test]
fn later_range_replaces_prior_selection_and_css() {
    let ui = setup(&["a", "b", "c", "d"]);
    ui.select_tab_range("a", "d");
    ui.select_tab_range("c", "b");
    assert_selection(&ui, &["b", "c"]);
}
#[test]
fn absent_endpoint_leaves_existing_selection_unchanged() {
    for (from, to) in [("missing", "b"), ("b", "missing"), ("missing", "absent")] {
        let ui = setup(&["a", "b", "c"]);
        ui.select_tab_range("a", "a");
        ui.select_tab_range(from, to);
        assert_selection(&ui, &["a"]);
    }
}
#[test]
fn ambiguous_endpoint_leaves_existing_selection_unchanged() {
    for (from, to) in [("b", "c"), ("c", "b"), ("b", "b")] {
        let ui = setup(&["a", "b", "b", "c"]);
        ui.select_tab_range("a", "a");
        ui.select_tab_range(from, to);
        assert_selection(&ui, &["a"]);
    }
}
#[test]
fn empty_strip_is_a_noop() {
    let ui = setup(&[]);
    ui.select_tab_range("a", "b");
    assert_selection(&ui, &[]);
}
