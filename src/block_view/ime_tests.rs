// Included only inside block_view::tests. Real XTest events exercise VTE's private
// GtkIMContext. The established keyboard regression covers commit/cancel.
// The Send regression uses the actual pointer path and checks native IM safety.

fn native_ime_keys(action: &str) {
    native_ime_keys_with_focus(action, false);
}

fn native_ime_keys_with_focus(action: &str, require_focus: bool) {
    let mut command = std::process::Command::new("python3");
    command
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/qa-native-ime-keyboard.py"
        ))
        .arg(action);
    if require_focus {
        command.arg("--require-focus");
    }
    let output = command
        .output()
        .expect("start fixed-action native IME helper");
    assert!(
        output.status.success(),
        "native IME helper: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    chat_display_settle();
}

// Accumulate through short writer-thread scheduling gaps, then reject extras.
fn native_ime_expect(pty: &crate::pty::OwnedPty, expected: &[u8]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut actual = Vec::new();
    while actual.len() < expected.len() && std::time::Instant::now() < deadline {
        actual.extend(pty.drain_test_slave(std::time::Duration::from_millis(20)));
    }
    assert_eq!(actual, expected, "native IME PTY payload");
    assert!(
        pty.drain_test_slave(std::time::Duration::from_millis(50))
            .is_empty(),
        "duplicate native IME bytes"
    );
}

#[test]
#[ignore = "requires isolated X11 DISPLAY and GTK_IM_MODULE=simple; run through test-gtk-display.sh"]
fn native_ime_commit_cancel_and_independent_ptys() {
    assert_eq!(std::env::var("GTK_IM_MODULE").as_deref(), Ok("simple"));
    use gtk4::prelude::*;
    use std::time::Duration;
    gtk4::init().expect("GTK display");
    let first = chat_display_view(crate::config::TerminalMode::Chat);
    let second = chat_display_view(crate::config::TerminalMode::Chat);
    let panes = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    panes.append(&first.root);
    panes.append(&second.root);
    let window = gtk4::Window::builder()
        .title("forge-native-ime-qa")
        .default_width(1000)
        .default_height(600)
        .child(&panes)
        .build();
    window.present();
    chat_display_settle();
    for view in [&first, &second] {
        view.pty.write_test_slave(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    }
    chat_display_settle();
    assert_eq!(first.bstate.get(), BlockState::AwaitingCommand);
    assert_eq!(second.bstate.get(), BlockState::AwaitingCommand);
    assert_ne!(first.pty.master_fd_raw(), second.pty.master_fd_raw());
    for view in [&first, &second] {
        view.pty.drain_test_slave(Duration::from_millis(30));
    }
    // Commit and cancel in each pane, while its sibling is a byte recorder.
    for (owner, other, ascii, expected_ascii) in [
        (&first, &second, "ascii-a", b"a".as_slice()),
        (&second, &first, "ascii-b", b"b".as_slice()),
    ] {
        assert!(owner.vte().grab_focus());
        chat_display_settle();
        assert!(owner.vte().has_focus());
        native_ime_keys("compose");
        assert!(
            owner
                .pty
                .drain_test_slave(Duration::from_millis(30))
                .is_empty(),
            "preedit must not reach PTY"
        );
        assert!(other
            .pty
            .drain_test_slave(Duration::from_millis(30))
            .is_empty());
        native_ime_keys("commit");
        // Return commits Unicode without submitting.
        native_ime_expect(&owner.pty, "中".as_bytes());
        native_ime_keys("commit");
        // The next Return reaches native input exactly once.
        native_ime_expect(&owner.pty, b"\r");
        native_ime_keys("compose");
        assert!(owner
            .pty
            .drain_test_slave(Duration::from_millis(30))
            .is_empty());
        native_ime_keys("cancel");
        assert!(
            owner
                .pty
                .drain_test_slave(Duration::from_millis(30))
                .is_empty(),
            "Escape cancels preedit without sending ESC"
        );
        native_ime_keys(ascii);
        // ASCII editing resumes after cancellation.
        native_ime_expect(&owner.pty, expected_ascii);
        assert!(
            other
                .pty
                .drain_test_slave(Duration::from_millis(50))
                .is_empty(),
            "all native input stays on the owner's PTY"
        );
    }
    window.close();
    first.pty.kill();
    second.pty.kill();
}

// A real pointer click must never bypass native preedit with a raw shell CR.
#[test]
#[ignore = "requires isolated X11 DISPLAY, GTK_IM_MODULE=simple and xdotool"]
fn native_ime_send_button_preserves_composition() {
    assert_eq!(std::env::var("GTK_IM_MODULE").as_deref(), Ok("simple"));
    use gtk4::prelude::*;
    use std::time::Duration;
    gtk4::init().expect("GTK display");
    let first = chat_display_view(crate::config::TerminalMode::Chat);
    let second = chat_display_view(crate::config::TerminalMode::Chat);
    let panes = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    panes.append(&first.root);
    panes.append(&second.root);
    let window = gtk4::Window::builder()
        .title("forge-native-ime-qa")
        .default_width(1000)
        .default_height(600)
        .child(&panes)
        .build();
    window.present();
    chat_display_settle();
    for view in [&first, &second] {
        view.pty.write_test_slave(b"\x1b]133;A\x07$ \x1b]133;B\x07");
    }
    chat_display_settle();
    assert_eq!(first.bstate.get(), BlockState::AwaitingCommand);
    assert_eq!(second.bstate.get(), BlockState::AwaitingCommand);
    assert_ne!(first.pty.master_fd_raw(), second.pty.master_fd_raw());
    for view in [&first, &second] {
        view.pty.drain_test_slave(Duration::from_millis(30));
    }

    for (owner, other) in [(&first, &second), (&second, &first)] {
        let send = chat_display_widget(owner.root.upcast_ref(), "chat-submit")
            .downcast::<gtk4::Button>()
            .unwrap();
        assert!(send.is_mapped() && send.is_sensitive());
        let click = || {
            let bounds = send.compute_bounds(&window).expect("mapped Send bounds");
            assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
            let x = (bounds.x() + bounds.width() / 2.0).round() as i32;
            let y = (bounds.y() + bounds.height() / 2.0).round() as i32;
            let found = std::process::Command::new("xdotool")
                .args(["search", "--onlyvisible", "--name", "^forge-native-ime-qa$"])
                .output()
                .expect("locate isolated QA window");
            assert!(found.status.success());
            let ids = String::from_utf8(found.stdout).unwrap();
            let ids: Vec<_> = ids.lines().collect();
            assert_eq!(
                ids.len(),
                1,
                "only the dedicated QA window may receive input"
            );
            let status = std::process::Command::new("xdotool")
                .args([
                    "mousemove",
                    "--window",
                    ids[0],
                    &x.to_string(),
                    &y.to_string(),
                    "click",
                    "1",
                ])
                .status()
                .expect("native Send pointer click");
            assert!(status.success());
            chat_display_settle();
        };
        assert!(owner.vte().grab_focus());
        chat_display_settle();
        // Positive control: use the production gesture, capture and admission
        // callbacks rather than directly emitting clicked or commit.
        click();
        native_ime_expect(&owner.pty, b"\r");
        assert!(owner.vte().has_focus(), "idle Send retains the input owner");
        native_ime_keys("compose");
        assert!(owner
            .pty
            .drain_test_slave(Duration::from_millis(50))
            .is_empty());
        click();
        assert!(
            owner
                .pty
                .drain_test_slave(Duration::from_millis(100))
                .is_empty(),
            "Send cannot submit or alter an active composition"
        );
        assert!(
            owner.vte().has_focus(),
            "blocked Send keeps native candidate ownership"
        );
        native_ime_keys("commit");
        native_ime_expect(&owner.pty, "中".as_bytes());
        // Only the next deliberate Send after native commit may submit.
        click();
        native_ime_expect(&owner.pty, b"\r");
        native_ime_keys("compose");
        assert!(owner
            .pty
            .drain_test_slave(Duration::from_millis(100))
            .is_empty());
        native_ime_keys("cancel");
        assert!(owner
            .pty
            .drain_test_slave(Duration::from_millis(100))
            .is_empty());
        native_ime_keys("ascii-a");
        native_ime_expect(&owner.pty, b"a");
        click();
        native_ime_expect(&owner.pty, b"\r");
        assert!(other
            .pty
            .drain_test_slave(Duration::from_millis(100))
            .is_empty());
    }
    // Hidden-but-live contexts must not be forgotten prematurely. Remapping
    // and native cancellation must recover without any application IM reset.
    assert!(first.vte().grab_focus());
    native_ime_keys("compose");
    assert!(super::ime_gate::blocked());
    first.root.set_visible(false);
    chat_display_settle();
    first.root.set_visible(true);
    chat_display_settle();
    assert!(first.vte().grab_focus());
    native_ime_keys("cancel");
    assert!(!first
        .pty
        .drain_test_slave(Duration::from_millis(100))
        .contains(&b'\r'));
    assert!(
        !super::ime_gate::blocked(),
        "native cancel after remap clears the gate"
    );
    native_ime_keys("compose");
    assert!(super::ime_gate::blocked());
    window.close();
    chat_display_settle();
    assert!(
        !super::ime_gate::blocked(),
        "unrealized contexts cannot strand Send"
    );
    first.pty.kill();
    second.pty.kill();
}

// Mirror the first and returning tab focus ordering without AppModel or a shell.
// This uses real X11 scripted keys and GTK simple-IM, not Fcitx or Wayland.
#[test]
#[ignore = "requires isolated X11 DISPLAY and GTK_IM_MODULE=simple; run through test-gtk-display.sh"]
fn native_ime_initial_and_returning_stack_tab_keep_same_owner() {
    use gtk4::prelude::*;
    use std::time::Duration;

    assert_eq!(std::env::var("GTK_IM_MODULE").as_deref(), Ok("simple"));
    gtk4::init().expect("GTK display");
    for mode in [
        crate::config::TerminalMode::Block,
        crate::config::TerminalMode::Chat,
        crate::config::TerminalMode::Unified,
    ] {
        let view = chat_display_view(mode.clone());
        view.block_onboarding.history_resolved(false);
        let other = gtk4::Entry::new();
        let stack = gtk4::Stack::new();
        stack.set_transition_type(gtk4::StackTransitionType::None);
        stack.add_named(&view.root, Some("terminal"));
        stack.add_named(&other, Some("other"));
        let window = gtk4::Window::builder()
            .title("forge-native-ime-qa")
            .default_width(800)
            .default_height(600)
            .child(&stack)
            .build();
        assert!(!view.vte().is_mapped());
        stack.set_visible_child_name("terminal");
        view.grab_focus();
        window.present();
        for returning in [false, true] {
            if returning {
                stack.set_visible_child_name("other");
                chat_display_settle();
                assert!(other.is_mapped());
                assert!(other.grab_focus());
                chat_display_settle();
                assert!(!view.vte().is_mapped());
                stack.set_visible_child_name("terminal");
                view.grab_focus();
            }
            chat_display_settle();
            assert!(
                view.vte().is_mapped() && view.vte().has_focus(),
                "{mode:?}, returning={returning}: terminal must own focus after layout"
            );
            view.pty.drain_test_slave(Duration::from_millis(30));
            native_ime_keys("compose");
            assert!(
                view.pty
                    .drain_test_slave(Duration::from_millis(30))
                    .is_empty(),
                "{mode:?}, returning={returning}: preedit must not reach the PTY"
            );
            native_ime_keys("commit");
            native_ime_expect(&view.pty, "中".as_bytes());
            assert!(
                other.text().is_empty(),
                "hidden sibling must not receive input"
            );
        }
        window.close();
        view.pty.kill();
        chat_display_settle();
    }
}

// The Stack fixture tests mode behavior. Use the production focus queues and
// a Notebook here, without asking the key injector to activate the X11 window.
#[test]
#[ignore = "requires isolated X11 DISPLAY and GTK_IM_MODULE=simple; run through test-gtk-display.sh"]
fn native_ime_notebook_startup_and_return_use_production_focus_queues() {
    use gtk4::prelude::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    assert_eq!(std::env::var("GTK_IM_MODULE").as_deref(), Ok("simple"));
    gtk4::init().expect("GTK display");
    let first = chat_display_view(crate::config::TerminalMode::Block);
    let second = chat_display_view(crate::config::TerminalMode::Block);
    for view in [&first, &second] {
        view.block_onboarding.history_resolved(false);
    }
    assert_ne!(first.pty.master_fd_raw(), second.pty.master_fd_raw());

    let notebook = gtk4::Notebook::new();
    let first_page = notebook.append_page(&first.root, None::<&gtk4::Widget>);
    let second_page = notebook.append_page(&second.root, None::<&gtk4::Widget>);
    notebook.set_current_page(Some(first_page));
    let chrome = gtk4::Entry::new();
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&chrome);
    root.append(&notebook);
    let window = gtk4::Window::builder()
        .title("forge-native-ime-qa")
        .default_width(800)
        .default_height(600)
        .child(&root)
        .build();
    let generation = Rc::new(Cell::new(1u64));
    let search = gtk4::SearchBar::new();
    let baseline_controllers = window.observe_controllers().n_items();
    crate::app::queue_startup_terminal_focus(
        &notebook,
        &window,
        generation.clone(),
        1,
        first.vte().clone(),
        first_page,
        search.clone(),
    );
    assert_eq!(
        window.observe_controllers().n_items(),
        baseline_controllers + 2
    );

    let first_terminal = first.vte().clone();
    let second_terminal = second.vte().clone();
    let switch_generation = generation.clone();
    notebook.connect_switch_page(move |notebook, _, page| {
        let request = switch_generation.get().wrapping_add(1);
        switch_generation.set(request);
        let target = if page == first_page {
            first_terminal.clone()
        } else {
            assert_eq!(page, second_page);
            second_terminal.clone()
        };
        crate::app::queue_tab_terminal_focus(
            notebook,
            switch_generation.clone(),
            request,
            target,
            page,
            search.clone(),
            || panic!("search is closed throughout this fixture"),
        );
    });

    // Expire idle-only focus before the first map. Production reconciliation
    // must retain the request until a real frame clock exists.
    let context = gtk4::glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(1);
    while context.iteration(false) {
        assert!(Instant::now() < deadline, "pre-map idles did not settle");
    }
    assert!(!first.vte().is_mapped());
    assert!(!first.vte().has_focus());
    assert_eq!(generation.get(), 1);
    window.present();

    for (stage, page, request, owner, other) in [
        ("initial", first_page, 1, &first, &second),
        ("switch-away", second_page, 2, &second, &first),
        ("return", first_page, 3, &first, &second),
    ] {
        if notebook.current_page() != Some(page) {
            notebook.set_current_page(Some(page));
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            while context.iteration(false) && Instant::now() < deadline {}
            if window.is_active()
                && notebook.current_page() == Some(page)
                && owner.vte().is_mapped()
                && owner.vte().has_focus()
                && !other.vte().is_mapped()
                && window.observe_controllers().n_items() == baseline_controllers
            {
                break;
            }
            assert!(Instant::now() < deadline, "{stage}: focus did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(generation.get(), request, "{stage}: exact Notebook request");
        assert_eq!(chrome.text().as_str(), "", "{stage}: no input in chrome");
        for view in [owner, other] {
            // Existing asynchronous protocol traffic is outside this key
            // sequence; from compose onward any non-commit byte is a failure.
            let _ = view.pty.drain_test_slave(Duration::from_millis(30));
        }
        native_ime_keys_with_focus("compose", true);
        for view in [owner, other] {
            assert!(
                view.pty
                    .drain_test_slave(Duration::from_millis(30))
                    .is_empty(),
                "{stage}: preedit must not reach either PTY"
            );
        }
        native_ime_keys_with_focus("commit", true);
        native_ime_expect(&owner.pty, "中".as_bytes());
        assert!(
            other
                .pty
                .drain_test_slave(Duration::from_millis(50))
                .is_empty(),
            "{stage}: inactive PTY must not receive the commit"
        );
        assert_eq!(chrome.text().as_str(), "", "{stage}: no input in chrome");
        println!("Native Notebook simple IM: {stage} selected PTY only; no X focus repair");
    }
    window.close();
    first.pty.kill();
    second.pty.kill();
    chat_display_settle();
}
