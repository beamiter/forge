//! Exact production pane-focus and zoom-restore functions at deterministic
//! GTK tree boundaries. This does not claim a native GTK reparenting test.
#![allow(dead_code)]
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
#[derive(Clone, Debug)]
struct Widget {
    id: usize,
    name: Rc<RefCell<String>>,
}
impl PartialEq for Widget {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Widget {
    fn new(id: usize) -> Self {
        Self {
            id,
            name: Rc::new(RefCell::new(format!("tab-{id}"))),
        }
    }
    fn widget_name(&self) -> String {
        self.name.borrow().clone()
    }
    fn set_widget_name(&self, s: &str) {
        *self.name.borrow_mut() = s.into()
    }
}
#[derive(Clone)]
struct Terminal(bool);
impl Terminal {
    fn has_focus(&self) -> bool {
        self.0
    }
}
#[derive(Clone)]
struct PaneLeaf {
    root: Widget,
    terminal: Terminal,
    focused: Rc<Cell<Option<usize>>>,
}
impl PaneLeaf {
    fn terminal(&self) -> &Terminal {
        &self.terminal
    }
    fn root_widget(&self) -> Widget {
        self.root.clone()
    }
    fn grab_focus(&self) {
        self.focused.set(Some(self.root.id))
    }
}
#[derive(Clone)]
struct PaneNode {
    leaves: Vec<PaneLeaf>,
    active: Option<usize>,
}
thread_local! {static NODE:RefCell<Option<PaneNode>>=const{RefCell::new(None)};}
impl PaneNode {
    fn from_widget(_: &Widget) -> Option<Self> {
        NODE.with(|n| n.borrow().clone())
    }
    fn leaves(&self) -> Vec<PaneLeaf> {
        self.leaves.clone()
    }
    fn active_leaf(&self) -> Option<PaneLeaf> {
        self.active.and_then(|i| self.leaves.get(i).cloned())
    }
}
#[derive(Default)]
struct Notebook {
    pages: RefCell<Vec<Widget>>,
    current: Cell<Option<u32>>,
}
impl Notebook {
    fn current_page(&self) -> Option<u32> {
        self.current.get()
    }
    fn nth_page(&self, n: Option<u32>) -> Option<Widget> {
        self.pages.borrow().get(n? as usize).cloned()
    }
    fn page_num(&self, w: &Widget) -> Option<u32> {
        self.pages
            .borrow()
            .iter()
            .position(|p| p == w)
            .map(|i| i as u32)
    }
    fn remove_page(&self, n: Option<u32>) {
        self.pages.borrow_mut().remove(n.unwrap() as usize);
    }
    fn insert_page(&self, w: &Widget, _: Option<&Widget>, n: Option<u32>) -> u32 {
        let mut pages = self.pages.borrow_mut();
        let n = n
            .map(|n| n as usize)
            .unwrap_or(pages.len())
            .min(pages.len());
        pages.insert(n, w.clone());
        n as u32
    }
    fn set_tab_reorderable(&self, _: &Widget, _: bool) {}
    fn set_current_page(&self, n: Option<u32>) {
        self.current.set(n)
    }
}
struct UiState {
    notebook: Notebook,
}
impl UiState {
    fn refresh_pane_headers(&self) {}
    // @pane-lifecycle:cycle
}
fn reattach_terminal_to_tree(_: &Widget, _: &Widget) {}
// @pane-lifecycle:swap
// @pane-lifecycle:restore
// @pane-lifecycle:restore_preserving
// @pane-lifecycle:restore_with_selection
// @pane-lifecycle:selection
// @pane-lifecycle:apply_selection
fn focus_fixture(
    active: Option<usize>,
    live: Option<usize>,
    count: usize,
) -> (UiState, Rc<Cell<Option<usize>>>) {
    let focused = Rc::new(Cell::new(None));
    let leaves = (0..count)
        .map(|i| PaneLeaf {
            root: Widget::new(i),
            terminal: Terminal(live == Some(i)),
            focused: focused.clone(),
        })
        .collect();
    NODE.with(|n| *n.borrow_mut() = Some(PaneNode { leaves, active }));
    (
        UiState {
            notebook: Notebook {
                pages: RefCell::new(vec![Widget::new(99)]),
                current: Cell::new(Some(0)),
            },
        },
        focused,
    )
}
#[test]
fn cycle_after_finished_block_focus_uses_owning_pane() {
    let (ui, focused) = focus_fixture(Some(2), None, 3);
    ui.cycle_pane_focus(1);
    assert_eq!(focused.get(), Some(0));
}
#[test]
fn reverse_cycle_after_finished_block_focus_uses_owning_pane() {
    let (ui, focused) = focus_fixture(Some(1), None, 3);
    ui.cycle_pane_focus(-1);
    assert_eq!(focused.get(), Some(0));
}
#[test]
fn cycle_from_search_or_scrollbar_uses_last_active_leaf() {
    let (ui, focused) = focus_fixture(Some(2), None, 4);
    ui.cycle_pane_focus(1);
    assert_eq!(focused.get(), Some(3));
}
#[test]
fn live_focus_wraps_normally_in_both_directions() {
    for (active, direction, expected) in [(2, 1, 0), (0, -1, 2), (1, 1, 2)] {
        let (ui, focused) = focus_fixture(Some(active), Some(active), 3);
        ui.cycle_pane_focus(direction);
        assert_eq!(focused.get(), Some(expected));
    }
}
#[test]
fn empty_single_and_unresolved_panes_do_not_move_focus() {
    for (active, count) in [(None, 0), (Some(0), 1), (None, 3)] {
        let (ui, focused) = focus_fixture(active, None, count);
        ui.cycle_pane_focus(1);
        assert_eq!(focused.get(), None);
    }
}
#[test]
fn zoom_restore_preserves_current_tab_order_after_reorder() {
    let original = Widget::new(10);
    let zoomed = Widget::new(11);
    let swap = ZoomPageSwap {
        original_page: original.clone(),
        zoomed_page: zoomed.clone(),
        page_index: 0,
        tab_label: None,
    };
    let notebook = Notebook {
        pages: RefCell::new(vec![Widget::new(20), Widget::new(30), zoomed]),
        current: Cell::new(Some(2)),
    };
    assert_eq!(restore_zoomed_leaf(&notebook, &swap), Some(2));
    assert_eq!(
        notebook
            .pages
            .borrow()
            .iter()
            .map(|w| w.id)
            .collect::<Vec<_>>(),
        [20, 30, 10]
    );
}
#[test]
fn zoom_restore_preserves_position_after_an_earlier_tab_closes() {
    let original = Widget::new(10);
    let zoomed = Widget::new(11);
    let swap = ZoomPageSwap {
        original_page: original.clone(),
        zoomed_page: zoomed.clone(),
        page_index: 2,
        tab_label: None,
    };
    let notebook = Notebook {
        pages: RefCell::new(vec![zoomed, Widget::new(30)]),
        current: Cell::new(Some(0)),
    };
    assert_eq!(restore_zoomed_leaf(&notebook, &swap), Some(0));
    assert_eq!(
        notebook
            .pages
            .borrow()
            .iter()
            .map(|w| w.id)
            .collect::<Vec<_>>(),
        [10, 30]
    );
}
#[test]
fn stale_zoom_target_does_not_mutate_notebook() {
    let swap = ZoomPageSwap {
        original_page: Widget::new(10),
        zoomed_page: Widget::new(11),
        page_index: 0,
        tab_label: None,
    };
    let notebook = Notebook {
        pages: RefCell::new(vec![Widget::new(30)]),
        current: Cell::new(Some(0)),
    };
    assert_eq!(restore_zoomed_leaf(&notebook, &swap), None);
    assert_eq!(notebook.pages.borrow()[0].id, 30);
}

#[test]
fn background_zoom_restore_keeps_foreground_widget_identity() {
    let original = Widget::new(10);
    let zoomed = Widget::new(11);
    let foreground = Widget::new(20);
    let swap = ZoomPageSwap {
        original_page: original.clone(),
        zoomed_page: zoomed.clone(),
        page_index: 99,
        tab_label: None,
    };
    let notebook = Notebook {
        pages: RefCell::new(vec![zoomed, foreground.clone()]),
        current: Cell::new(Some(1)),
    };
    assert_eq!(
        restore_zoomed_leaf_preserving_selection(&notebook, &swap),
        Some(0)
    );
    assert_eq!(
        notebook.nth_page(notebook.current_page()),
        Some(foreground.clone())
    );
    assert_eq!(notebook.nth_page(Some(0)), Some(original));
    // A late duplicate close completion cannot resurrect or select the old swap.
    assert_eq!(
        restore_zoomed_leaf_preserving_selection(&notebook, &swap),
        None
    );
    assert_eq!(notebook.nth_page(notebook.current_page()), Some(foreground));
}

#[test]
fn active_zoom_restore_selects_its_reinstated_tree() {
    let original = Widget::new(10);
    let zoomed = Widget::new(11);
    let swap = ZoomPageSwap {
        original_page: original.clone(),
        zoomed_page: zoomed.clone(),
        page_index: 99,
        tab_label: None,
    };
    let notebook = Notebook {
        pages: RefCell::new(vec![Widget::new(20), zoomed]),
        current: Cell::new(Some(1)),
    };
    assert_eq!(
        restore_zoomed_leaf_preserving_selection(&notebook, &swap),
        Some(1)
    );
    assert_eq!(notebook.nth_page(notebook.current_page()), Some(original));
}
