//! Native Unified navigation through the production reader, VTE and controls.
use super::*;
use crate::config::TerminalMode;
use crate::keybindings::Action;
use gtk4 as gtk;
use std::path::Path;
use std::time::{Duration, Instant};

fn settle() {
    let context = gtk::glib::MainContext::default();
    let until = Instant::now() + Duration::from_millis(100);
    while Instant::now() < until {
        for _ in 0..128 {
            if !context.iteration(false) {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_until(mut ready: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(Instant::now() < deadline, "{message}");
        settle();
    }
    settle();
    assert!(ready(), "{message}: the settled state changed again");
}

fn completed_commands(view: &TermView) -> Vec<(u64, String)> {
    view.render_backend
        .records()
        .iter()
        .filter(|record| !record.is_background())
        .map(|record| (record.id(), record.command().to_owned()))
        .collect()
}

fn start_command(view: &TermView, command: &str, output: &str) {
    assert_eq!(view.bstate.get(), BlockState::AwaitingCommand);
    let encoded: String = command.bytes().map(|byte| format!("%{byte:02X}")).collect();
    view.pty.write_test_slave(
        format!("{command}\r\n\x1b]133;C;cmdline_url={encoded}\x07{output}").as_bytes(),
    );
    wait_until(
        || view.bstate.get() == BlockState::CollectingOutput,
        "the real OSC 133 command start must reach the Unified reader",
    );
}

fn finish_command(view: &TermView, expected_commands: usize) {
    view.pty
        .write_test_slave(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    wait_until(
        || {
            view.bstate.get() == BlockState::AwaitingCommand
                && completed_commands(view).len() == expected_commands
        },
        "the next real prompt must finalize exactly one command",
    );
}

fn assert_no_input(view: &TermView) {
    assert!(
        view.pty
            .drain_test_slave(Duration::from_millis(30))
            .is_empty(),
        "navigation and observation must not write input to the shell PTY"
    );
}

fn grid_matches_pty(view: &TermView) -> bool {
    let (columns, rows) = view.pty.test_slave_winsize();
    columns > 0
        && rows > 0
        && i64::from(columns) == view.active_vte.column_count()
        && i64::from(rows) == view.active_vte.row_count()
}

fn selected_text(view: &TermView) -> Option<String> {
    view.active_vte
        .text_selected(vte4::Format::Text)
        .map(|text| text.to_string())
}

fn owns_focus(window: &gtk::Window, widget: &impl IsA<gtk::Widget>) -> bool {
    let widget = widget.as_ref();
    gtk::prelude::GtkWindowExt::focus(window)
        .is_some_and(|focus| focus == *widget || focus.is_ancestor(widget))
}

fn screenshot(window: &gtk::Window, name: &str) {
    let Some(directory) = std::env::var_os("FORGE_UNIFIED_SCREENSHOT_DIR") else {
        return;
    };
    let directory = Path::new(&directory);
    std::fs::create_dir_all(directory).expect("create Unified screenshot directory");
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let node = snapshot
        .to_node()
        .expect("the mapped Unified window paints");
    window
        .renderer()
        .expect("mapped native renderer")
        .render_texture(&node, None)
        .save_to_png(directory.join(name))
        .expect("save native Unified screenshot");
}

#[test]
#[ignore = "requires an isolated GTK/VTE display; run explicitly under Xvfb"]
fn unified_navigation_preserves_reading_identity_selection_and_tui() {
    gtk::init().expect("GTK display");
    let view = find::palette_activation_test_view(TerminalMode::Unified);
    // The normal app places Find outside the terminal subtree. Its focus
    // must survive command completion and native incremental search alike.
    let query = gtk::SearchEntry::new();
    let search_bar = gtk::SearchBar::new();
    search_bar.set_child(Some(&query));
    search_bar.connect_entry(&query);
    let contents = gtk::Box::new(gtk::Orientation::Vertical, 0);
    contents.append(&search_bar);
    contents.append(&view.widget());
    let window = gtk::Window::builder()
        .title("Forge · Unified navigation")
        .default_width(900)
        .default_height(560)
        .child(&contents)
        .build();
    // Exercise the production pane-local control binding. The application
    // dispatcher additionally owns dialogs; this fixture only activates the
    // four presentation actions that complete inside this exact TermView.
    view.connect_unified_navigation({
        let weak = Rc::downgrade(&view);
        move |action| {
            let view = weak.upgrade().expect("the fixture owns its pane");
            match action {
                Action::PreviousCommand | Action::NextCommand => assert_eq!(
                    view.navigate_command(action == Action::NextCommand),
                    Ok(find::RecordNavigationResult::Navigated),
                    "a proven native button target must be reachable"
                ),
                Action::ReturnToLive => assert!(view.return_to_live()),
                Action::CopyCurrentCommand => view
                    .copy_current_command()
                    .expect("the explicitly chosen command is copyable"),
                _ => panic!("unexpected fixture action {action:?}"),
            }
        }
    });
    window.present();
    find::palette_activation_test_feed(&view, b"\x1b]133;A\x07$ \x1b]133;B\x07");
    wait_until(
        || view.bstate.get() == BlockState::AwaitingCommand && grid_matches_pty(&view),
        "the initial prompt must map a grid matching the child PTY",
    );
    let bar = view
        .unified_navigation
        .as_ref()
        .expect("Unified constructs its navigation bar")
        .clone();
    assert!(bar.root.is_mapped());
    assert!(
        !view.jump_fab.is_visible(),
        "the bar owns the live affordance"
    );
    assert!(view.finished_blocks.borrow().is_empty());
    assert!(!bar.previous.is_sensitive());

    // Each boundary is rendered before the next command starts. This proves
    // real zone heads through the production OSC 8 probe instead of seeding
    // authority or manufacturing a metadata-to-row correspondence.
    start_command(
        &view,
        "printf build-log",
        &"build output retained\r\n".repeat(80),
    );
    finish_command(&view, 1);
    for (index, command) in ["printf alpha", "printf beta", "printf gamma"]
        .into_iter()
        .enumerate()
    {
        start_command(&view, command, "completed successfully\r\n");
        finish_command(&view, index + 2);
    }
    let commands = completed_commands(&view);
    assert_eq!(commands.len(), 4);
    let previous = commands[2].0;
    let latest = commands[3].0;
    let adjustment = view.active_vte.vadjustment().expect("native adjustment");
    assert!(adjustment.upper() - adjustment.page_size() > adjustment.lower() + 10.0);
    wait_until(
        || {
            view.render_backend.record_scroll_value(previous).is_some()
                && view.render_backend.record_scroll_value(latest).is_some()
        },
        "visible completed command heads must acquire native location proof",
    );
    assert_eq!(
        view.render_backend.record_scroll_value(previous),
        view.render_backend.record_scroll_value(latest),
        "the two short tail commands must share a bottom-clamped scroll target"
    );
    assert!(bar.previous.is_sensitive());
    assert_no_input(&view);

    bar.previous.emit_clicked();
    settle();
    assert_eq!(view.navigated_record_id.get(), Some(latest));
    let shared_value = adjustment.value();
    bar.previous.emit_clicked();
    settle();
    assert_eq!(view.navigated_record_id.get(), Some(previous));
    assert_eq!(adjustment.value(), shared_value);
    bar.next.emit_clicked();
    settle();
    assert_eq!(view.navigated_record_id.get(), Some(latest));
    assert_eq!(adjustment.value(), shared_value);

    // A GtkAdjustment range notification is independent of value-changed.
    // Exercise that real signal synchronously, then restore the native range
    // before another VTE frame. It must update Reading without mistaking a
    // range-only change for the user abandoning the chosen command.
    let native_upper = adjustment.upper();
    adjustment.set_upper(native_upper + 8.0);
    assert_eq!(adjustment.value(), shared_value);
    assert!(view.user_scrolled_up.get());
    assert_eq!(view.navigated_record_id.get(), Some(latest));
    adjustment.set_upper(native_upper);
    assert!(!view.user_scrolled_up.get());
    assert_eq!(view.navigated_record_id.get(), Some(latest));
    settle();

    // Copy addresses the displayed record choice. A native selection remains
    // independently available to the ordinary selected-text copy action.
    view.active_vte.select_all();
    let selection = selected_text(&view).expect("native selection");
    assert!(!selection.is_empty());
    bar.copy.emit_clicked();
    settle();
    assert_eq!(selected_text(&view).as_deref(), Some(selection.as_str()));
    assert_eq!(
        gtk::glib::MainContext::default()
            .block_on(view.active_vte.clipboard().read_text_future())
            .expect("local clipboard provider")
            .as_deref(),
        Some(commands[3].1.as_str())
    );
    assert_no_input(&view);
    view.active_vte.unselect_all();
    view.selection_feed_hold.flush_now();

    // Native value movement abandons the explicit command cursor. Further
    // real output increases the adjustment range while the reader stays put.
    adjustment.set_value(shared_value - 10.0);
    wait_until(
        || view.user_scrolled_up.get(),
        "native history reading state",
    );
    assert_eq!(view.navigated_record_id.get(), None);
    let parked = adjustment.value();
    let old_upper = adjustment.upper();
    start_command(
        &view,
        "printf while-reading",
        &"new output below the reader\r\n".repeat(16),
    );
    assert!(adjustment.upper() > old_upper);
    assert!((adjustment.value() - parked).abs() < 0.01);
    assert!(view.user_scrolled_up.get());
    finish_command(&view, 5);
    assert_eq!(view.unread_count.get(), 1);
    assert!((adjustment.value() - parked).abs() < 0.01);
    assert!(bar.live.is_sensitive());
    screenshot(&window, "forge-unified-wide.png");

    window.set_default_size(360, 560);
    wait_until(
        || window.width() <= 380 && grid_matches_pty(&view),
        "a 360 px Unified pane must remain usable and resize its PTY",
    );
    let strip = bar
        .root
        .compute_bounds(&window)
        .expect("navigation allocation");
    for button in [
        &bar.previous,
        &bar.next,
        &bar.copy,
        &bar.find,
        &bar.browse,
        &bar.live,
    ] {
        let bounds = button
            .compute_bounds(&window)
            .expect("visible button allocation");
        assert!(
            bounds.x() >= strip.x() - 1.0
                && bounds.x() + bounds.width() <= strip.x() + strip.width() + 1.0,
            "a navigation control must not overflow the narrow pane"
        );
    }
    screenshot(&window, "forge-unified-narrow.png");
    window.set_default_size(900, 560);
    wait_until(
        || window.width() >= 850 && grid_matches_pty(&view),
        "the restored wide pane must synchronize its PTY again",
    );
    bar.live.emit_clicked();
    settle();
    assert!(!view.user_scrolled_up.get());
    assert_eq!(view.unread_count.get(), 0);
    assert_eq!(view.navigated_record_id.get(), None);
    assert!(
        (adjustment.value()
            - (adjustment.upper() - adjustment.page_size()).max(adjustment.lower()))
        .abs()
            < 0.01
    );
    assert!(owns_focus(&window, &view.active_vte));
    assert_no_input(&view);

    // A completion invalidates the old native pass without pretending the
    // still-open Find entry became an inactive query. Its next action must
    // request a rebuild, and that rebuild keeps the entry's keyboard focus.
    start_command(
        &view,
        "printf find-case",
        "navigation-needle\r\nnavigation-needle again\r\n",
    );
    let find_predecessor = completed_commands(&view).last().unwrap().0;
    assert!(matches!(
        view.navigate_to_record_id(find_predecessor, false),
        find::RecordNavigationResult::Navigated | find::RecordNavigationResult::SnapshotView { .. }
    ));
    assert_eq!(view.navigated_record_id.get(), Some(find_predecessor));
    search_bar.set_search_mode(true);
    query.set_text("navigation-needle");
    query.grab_focus();
    settle();
    assert!(matches!(
        view.find_in_blocks("navigation-needle", false),
        find::FindSearchResult::Matches(_)
    ));
    assert_eq!(
        view.navigated_record_id.get(),
        None,
        "Find is a new reading intent even when its hit shares the same viewport"
    );
    assert!(owns_focus(&window, &query));
    view.selection_feed_hold.flush_now();
    finish_command(&view, 6);
    assert!(owns_focus(&window, &query));
    assert_eq!(view.find_next(), find::FindNavigationResult::Invalidated);
    assert!(matches!(
        view.find_in_blocks("navigation-needle", false),
        find::FindSearchResult::Matches(_)
    ));
    assert!(owns_focus(&window, &query));
    view.clear_find();

    // An exact zero-hit search is still an active query. The next real
    // command can make it match without any edit to the open Find entry.
    query.set_text("arriving-needle");
    assert_eq!(
        view.find_in_blocks("arriving-needle", false),
        find::FindSearchResult::NoMatches
    );
    assert!(owns_focus(&window, &query));
    start_command(&view, "printf delayed-find", "arriving-needle\r\n");
    finish_command(&view, 7);
    assert!(owns_focus(&window, &query));
    assert_eq!(view.find_next(), find::FindNavigationResult::Invalidated);
    assert!(matches!(
        view.find_in_blocks("arriving-needle", false),
        find::FindSearchResult::Matches(_)
    ));
    assert!(owns_focus(&window, &query));
    view.clear_find();
    search_bar.set_search_mode(false);
    view.return_to_live();
    wait_until(
        || grid_matches_pty(&view),
        "Find close geometry must settle",
    );
    let normal_grid = view.pty.test_slave_winsize();
    assert_no_input(&view);

    // Removing the space-occupying bar hands its rows to the alternate screen;
    // the child must see the exact grid that VTE actually renders on both sides.
    find::palette_activation_test_feed(
        &view,
        b"\x1b[?1049h\x1b[H\x1b[2JFullscreen application\r\n",
    );
    wait_until(
        || {
            view.bstate.get() == BlockState::AltScreen
                && !bar.root.is_visible()
                && grid_matches_pty(&view)
                && view.pty.test_slave_winsize().1 > normal_grid.1
        },
        "alternate screen takeover must hide chrome and publish the expanded native grid",
    );
    assert!(view.navigate_command(false).is_err());
    assert!(!view.return_to_live());
    assert!(view.copy_current_command().is_err());
    find::palette_activation_test_feed(&view, b"\x1b[?1049l");
    wait_until(
        || {
            view.bstate.get() == BlockState::AwaitingCommand
                && bar.root.is_mapped()
                && grid_matches_pty(&view)
                && view.pty.test_slave_winsize() == normal_grid
        },
        "alternate screen exit must restore the bar and matching normal grid",
    );
    assert_no_input(&view);

    // RIS removes row authority while retained completed snapshots survive.
    // Navigation must offer the snapshot, never invent a scroll coordinate.
    let retained = completed_commands(&view).last().unwrap().0;
    find::palette_activation_test_feed(&view, b"\x1bc");
    settle();
    assert!(view.render_backend.record_scroll_value(retained).is_none());
    let after_reset = adjustment.value();
    assert_eq!(
        view.navigate_to_record_id(retained, false),
        find::RecordNavigationResult::SnapshotView {
            record_id: retained
        }
    );
    assert_eq!(adjustment.value(), after_reset);
    assert_no_input(&view);

    // Exercise real retention rather than deleting metadata from the fixture.
    let mut config = Config::safe_defaults();
    config.max_visible_blocks = 1;
    view.reload_config(&config);
    find::palette_activation_test_feed(&view, b"\x1b]133;A\x07$ \x1b]133;B\x07");
    wait_until(
        || view.bstate.get() == BlockState::AwaitingCommand,
        "post-reset prompt",
    );
    start_command(
        &view,
        "printf retained-tail",
        "only the newest record remains\r\n",
    );
    finish_command(&view, 1);
    assert_ne!(completed_commands(&view)[0].0, retained);
    assert!(!view.can_jump_to_record(retained, false));
    assert_eq!(
        view.navigate_to_record_id(retained, false),
        find::RecordNavigationResult::NoMatchingRecord
    );
    assert!(view.copy_current_command().is_err());
    assert_no_input(&view);
    window.close();
    settle();
}
