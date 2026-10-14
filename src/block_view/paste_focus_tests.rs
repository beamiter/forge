// Real GTK focus and the same request/completion boundary as read_text_async.
// No clipboard-provider timing or synthetic IME events are needed here.
fn paste_focus_fixture(
    mode: crate::config::TerminalMode,
) -> (
    super::TermView,
    super::TermView,
    gtk4::SearchEntry,
    gtk4::Window,
) {
    use gtk4::prelude::*;
    let first = chat_display_view(mode);
    let second = chat_display_view(crate::config::TerminalMode::Block);
    let find = gtk4::SearchEntry::new();
    let panes = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    panes.append(&first.root);
    panes.append(&second.root);
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.append(&find);
    root.append(&panes);
    let window = gtk4::Window::builder()
        .default_width(1200)
        .default_height(800)
        .child(&root)
        .build();
    window.present();
    replay_native_focus_bytes(
        &first,
        b"\x1b]133;A\x07$ \x1b]133;B\x07history\r\n\x1b]133;C;cmdline_url=history\x07output\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07\x1b[?2004h",
    );
    replay_native_focus_bytes(&second, b"\x1b]133;A\x07$ \x1b]133;B\x07");
    chat_display_settle();
    first.pty.drain_test_slave(PTY_REPLY_WAIT);
    second.pty.drain_test_slave(PTY_REPLY_WAIT);
    (first, second, find, window)
}

fn assert_paste_focus_payload(first: &super::TermView, second: &super::TermView, expected: &[u8]) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut received = Vec::new();
    while received.len() < expected.len() && std::time::Instant::now() < deadline {
        received.extend(first.pty.drain_test_slave(PTY_REPLY_WAIT));
    }
    assert_eq!(received, expected);
    assert!(first.pty.drain_test_slave(PTY_REPLY_WAIT).is_empty());
    assert!(second.pty.drain_test_slave(PTY_REPLY_WAIT).is_empty());
}

#[test]
#[ignore = "requires DISPLAY; run explicitly under Xvfb"]
fn delayed_clipboard_paste_preserves_newer_pane_search_and_empty_focus() {
    use gtk4::prelude::*;
    gtk4::init().unwrap();
    for mode in [
        crate::config::TerminalMode::Block,
        crate::config::TerminalMode::Chat,
    ] {
        let (first, second, find, window) = paste_focus_fixture(mode);
        for owner in [
            second.active_vte.clone().upcast::<gtk4::Widget>(),
            find.upcast(),
        ] {
            for clear_new_owner in [false, true] {
                assert!(first.active_vte.grab_focus());
                let request = first.clipboard_paste_request();
                assert!(request.focus.is_some());
                assert!(owner.grab_focus());
                let chosen = GtkWindowExt::focus(&window).unwrap();
                if clear_new_owner {
                    GtkWindowExt::set_focus(&window, None::<&gtk4::Widget>);
                }
                assert!(request.complete(Some("one\r\ntwo")).is_ok());
                drain_native_focus_idle();
                assert_eq!(
                    GtkWindowExt::focus(&window),
                    (!clear_new_owner).then_some(chosen)
                );
                assert_paste_focus_payload(&first, &second, b"\x1b[200~one\ntwo\x1b[201~");
            }
        }
        // A clipboard action while unrelated search owns focus cannot claim
        // that search owner as its initial pane authority.
        let unrelated = gtk4::SearchEntry::new();
        let root = window.child().unwrap().downcast::<gtk4::Box>().unwrap();
        root.prepend(&unrelated);
        chat_display_settle();
        assert!(unrelated.grab_focus());
        let chosen = GtkWindowExt::focus(&window);
        let request = first.clipboard_paste_request();
        assert!(request.focus.is_none());
        assert!(request.complete(Some("safe")).is_ok());
        drain_native_focus_idle();
        assert_eq!(GtkWindowExt::focus(&window), chosen);
        assert_paste_focus_payload(&first, &second, b"\x1b[200~safe\x1b[201~");

        // Accepted-input callbacks run inside the production sink. They can
        // choose newer chrome focus before complete() schedules its idle.
        let callback_find = unrelated.clone();
        first.connect_human_input(move |_| {
            assert!(callback_find.grab_focus());
        });
        assert!(first.active_vte.grab_focus());
        let request = first.clipboard_paste_request();
        assert!(request.complete(Some("callback")).is_ok());
        let chosen = GtkWindowExt::focus(&window);
        assert_ne!(chosen.as_ref(), Some(first.active_vte.upcast_ref()));
        drain_native_focus_idle();
        assert_eq!(GtkWindowExt::focus(&window), chosen);
        assert_paste_focus_payload(&first, &second, b"\x1b[200~callback\x1b[201~");
        window.close();
    }
}

#[test]
#[ignore = "requires DISPLAY; run explicitly under Xvfb"]
fn delayed_clipboard_paste_restores_original_history_only_after_admission() {
    use gtk4::prelude::*;
    gtk4::init().unwrap();
    for mode in [
        crate::config::TerminalMode::Block,
        crate::config::TerminalMode::Chat,
    ] {
        let (first, second, _, window) = paste_focus_fixture(mode);
        let history = first.finished_blocks.borrow()[0].command_vte.clone();
        let oversized = "x".repeat(crate::pty::MAX_PTY_INPUT_MESSAGE_BYTES + 1);
        for text in [
            None,
            Some(""),
            Some("\x1b[200~\x1b[201~"),
            Some(oversized.as_str()),
        ] {
            assert!(history.grab_focus());
            let owner = GtkWindowExt::focus(&window);
            let request = first.clipboard_paste_request();
            assert!(request.focus.is_some());
            let shadow = first.typed_cmd.borrow().clone();
            let generation = first.accepted_input_generation.get();
            let result = request.complete(text);
            assert_eq!(result.is_err(), text == Some(oversized.as_str()));
            drain_native_focus_idle();
            assert_eq!(GtkWindowExt::focus(&window), owner);
            assert_eq!(*first.typed_cmd.borrow(), shadow);
            assert_eq!(first.accepted_input_generation.get(), generation);
            assert_paste_focus_payload(&first, &second, b"");
        }
        // Returning to the original history after a newer live owner does not
        // revive a retired request, even though the final snapshot is identical.
        assert!(history.grab_focus());
        let request = first.clipboard_paste_request();
        assert!(first.active_vte.grab_focus());
        assert!(history.grab_focus());
        assert!(request.complete(Some("older")).is_ok());
        drain_native_focus_idle();
        assert!(history.has_focus());
        assert_paste_focus_payload(&first, &second, b"\x1b[200~older\x1b[201~");

        assert!(history.grab_focus());
        let request = first.clipboard_paste_request();
        assert!(request.focus.is_some());
        assert!(
            history.has_focus(),
            "request must not synchronously move focus"
        );
        assert!(request.complete(Some("one\r\ntwo")).is_ok());
        drain_native_focus_idle();
        assert!(first.active_vte.has_focus());
        assert_paste_focus_payload(&first, &second, b"\x1b[200~one\ntwo\x1b[201~");

        // Re-encode against the delivery-time mode, not the captured request.
        let request = first.clipboard_paste_request();
        replay_native_focus_bytes(&first, b"\x1b[?2004l");
        assert!(request.complete(Some("one\r\ntwo")).is_ok());
        drain_native_focus_idle();
        assert_paste_focus_payload(&first, &second, b"one");
        window.close();
    }
}

#[test]
#[ignore = "requires DISPLAY; run explicitly under Xvfb"]
fn delayed_clipboard_paste_closed_or_hidden_target_cannot_reclaim_focus() {
    use gtk4::prelude::*;
    gtk4::init().unwrap();
    for mode in [
        crate::config::TerminalMode::Block,
        crate::config::TerminalMode::Chat,
    ] {
        for close in [false, true] {
            for newer_owner in [false, true] {
                let (first, second, _, window) = paste_focus_fixture(mode.clone());
                assert!(first.active_vte.grab_focus());
                let request = first.clipboard_paste_request();
                if newer_owner {
                    assert!(second.active_vte.grab_focus());
                } else {
                    GtkWindowExt::set_focus(&window, None::<&gtk4::Widget>);
                }
                let owner = GtkWindowExt::focus(&window);
                if close {
                    first.kill();
                }
                first.root.set_visible(false);
                if !newer_owner {
                    assert!(
                        !request.focus.as_ref().unwrap().superseded.get(),
                        "fixture must test visibility/close without a newer focus owner"
                    );
                }
                assert_eq!(request.complete(Some("one\r\ntwo")).is_err(), close);
                drain_native_focus_idle();
                assert_eq!(GtkWindowExt::focus(&window), owner);
                assert_paste_focus_payload(
                    &first,
                    &second,
                    if close {
                        b""
                    } else {
                        b"\x1b[200~one\ntwo\x1b[201~"
                    },
                );
                window.close();
            }
        }
    }
}
