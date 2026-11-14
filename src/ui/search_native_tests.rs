//! Production search controller with native GTK signals, GLib timers and VTE.
//! Only pane lookup is a fixed classic-terminal fixture. This does not launch
//! the complete application, spawn a shell, or inject operating-system keys.
use crate::block_view::TermView;
use gtk4::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use vte4::TerminalExt;

// Compile the actual controller, not a second implementation of its policy.
#[allow(
    clippy::duplicate_mod,
    reason = "exercise the unchanged production controller against a native classic-pane fixture"
)]
#[path = "search.rs"]
mod controller;

#[derive(Clone)]
struct UiState {
    search_bar: gtk4::SearchBar,
    search_entry: gtk4::SearchEntry,
    search_status: gtk4::Label,
    search_debounce_source: Rc<RefCell<Option<glib::SourceId>>>,
    search_generation: Rc<Cell<u64>>,
    search_owner: Rc<RefCell<Option<controller::SearchOwner>>>,
    terminal: vte4::Terminal,
}

impl UiState {
    fn current_terminal(&self) -> Option<vte4::Terminal> {
        Some(self.terminal.clone())
    }
    fn current_term_view(&self) -> Option<Rc<TermView>> {
        None
    }
    fn terminal_in_page(&self, _: &gtk4::Widget) -> Option<vte4::Terminal> {
        self.current_terminal()
    }
    fn term_view_in_page(&self, _: &gtk4::Widget) -> Option<Rc<TermView>> {
        None
    }
    fn focus_current_terminal(&self) {
        use gtk4::prelude::WidgetExt;
        self.terminal.grab_focus();
    }
}

fn pump(duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        for _ in 0..128 {
            if !glib::MainContext::default().iteration(false) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn selected(ui: &UiState) -> String {
    ui.terminal
        .text_selected(vte4::Format::Text)
        .unwrap_or_default()
        .to_string()
}

#[test]
#[ignore = "requires DISPLAY; real SearchEntry signals, GLib timers and VTE selection"]
fn native_entry_navigation_and_close_reopen_retire_stale_queries() {
    use gtk4::prelude::*;
    libadwaita::init().expect("private GTK display");
    let ui = Rc::new(UiState {
        search_bar: gtk4::SearchBar::new(),
        search_entry: gtk4::SearchEntry::new(),
        search_status: gtk4::Label::new(None),
        search_debounce_source: Default::default(),
        search_generation: Default::default(),
        search_owner: Default::default(),
        terminal: vte4::Terminal::new(),
    });
    // Make the old delayed signal's race deterministic, independent of how
    // quickly the CI machine processes input. Production uses its default.
    ui.search_entry.set_search_delay(1000);
    ui.search_bar.set_child(Some(&ui.search_entry));
    ui.search_bar.connect_entry(&ui.search_entry);
    ui.search_bar.set_search_mode(true);
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    column.append(&ui.search_bar);
    column.append(&ui.terminal);
    ui.terminal.set_vexpand(true);
    let window = gtk4::Window::builder()
        .default_width(720)
        .default_height(400)
        .child(&column)
        .build();
    window.present();
    pump(Duration::from_millis(250));
    assert!(ui.search_entry.is_mapped() && ui.terminal.is_mapped());
    // Distinct rows isolate controller lifecycle from VTE 0.76's same-row
    // search cursor behavior (which can skip additional matches on that row).
    ui.terminal.feed(b"old1\r\nold2\r\nnew1\r\nnew2\r\n");
    pump(Duration::from_millis(100));

    // Negative control: the previous native wiring really leaves navigation
    // on the old expression before SearchEntry emits its delayed signal.
    ui.search_entry.set_text("/old[12]/");
    ui.search_apply();
    assert_eq!(selected(&ui), "old1");
    let legacy_ui = ui.clone();
    let legacy = ui
        .search_entry
        .connect_search_changed(move |_| legacy_ui.schedule_search_apply());
    ui.search_entry.set_text("/new[12]/");
    ui.search_next();
    assert!(
        selected(&ui).starts_with("old"),
        "negative control must expose stale navigation: {:?}",
        selected(&ui)
    );
    ui.search_entry.disconnect(legacy);

    let connection = ui.connect_search_entry_changes();
    for backwards in [false, true] {
        ui.search_entry.set_text("/old[12]/");
        ui.search_apply();
        ui.search_entry.set_text("/new[12]/");
        assert!(ui.search_debounce_source.borrow().is_some());
        if backwards {
            ui.search_prev();
        } else {
            ui.search_next();
        }
        let first = selected(&ui);
        assert!(first == "new1" || first == "new2", "{first:?}");
        assert!(ui.search_debounce_source.borrow().is_none());
        ui.search_next();
        pump(Duration::from_millis(30));
        let second = selected(&ui);
        assert!(second == "new1" || second == "new2", "{second:?}");
        assert_ne!(second, first, "next must advance the native match");
        ui.search_prev();
        pump(Duration::from_millis(30));
        assert_eq!(
            selected(&ui),
            first,
            "previous must return to the same match"
        );
        pump(Duration::from_millis(1200));
        assert_eq!(
            selected(&ui),
            first,
            "late signals must not reset navigation"
        );
    }

    ui.search_entry.set_text("old");
    assert!(ui.search_debounce_source.borrow().is_some());
    ui.toggle_search();
    assert!(!ui.search_bar.is_search_mode());
    assert!(ui.search_debounce_source.borrow().is_none());
    assert!(ui.terminal.search_get_regex().is_none());
    pump(Duration::from_millis(1200));
    assert!(ui.terminal.search_get_regex().is_none());
    // GTK clears the connected entry on close. Reopening must remain idle,
    // then accept a fresh query; it must not resurrect the cancelled one.
    assert!(ui.search_entry.text().is_empty());
    ui.toggle_search();
    pump(Duration::from_millis(350));
    assert!(ui.search_bar.is_search_mode());
    assert!(ui.terminal.search_get_regex().is_none());
    ui.search_entry.set_text("old");
    pump(Duration::from_millis(350));
    assert!(ui.terminal.search_get_regex().is_some());
    assert_eq!(
        selected(&ui),
        "old",
        "entry={:?}, status={:?}, buffer={:?}, geometry={}x{}",
        ui.search_entry.text(),
        ui.search_status.text(),
        ui.terminal.text_format(vte4::Format::Text),
        ui.terminal.column_count(),
        ui.terminal.row_count()
    );
    ui.search_entry.set_text("");
    assert!(ui.terminal.search_get_regex().is_none());
    assert_eq!(ui.search_status.text().as_str(), "");
    ui.toggle_search();
    ui.search_entry.disconnect(connection);
    window.close();
}
