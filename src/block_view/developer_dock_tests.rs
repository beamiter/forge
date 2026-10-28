//! Real GTK allocation / slave TIOCGWINSZ checks for the new Block seats.
use super::*;

#[test]
#[ignore = "requires DISPLAY; run explicitly under Xvfb"]
fn block_dock_keeps_one_native_surface_and_physical_grid() {
    gtk4::init().unwrap();
    crate::ui::reference_shell::install_css();
    let config = crate::config::Config::safe_defaults();
    let view = TermView::new_with_spawner(
        &config,
        &crate::config::TerminalMode::Block,
        &["sh".into()],
        None,
        None,
        &[],
        |_, _, _, _| crate::pty::OwnedPty::for_tests(crate::pty::PtyForeground::Shell),
    )
    .unwrap();
    let window = gtk4::Window::builder()
        .default_width(758)
        .default_height(662)
        .child(&view.root)
        .build();
    window.add_css_class("forge-window");
    window.add_css_class("reference-shell");
    window.present();
    let native = view.active_vte.clone();
    let anchor = view.active.borrow().widget().clone();
    view.ftcs_seen.set(true);

    let settle = || {
        let clock = window.frame_clock().expect("native frame clock");
        let frames = Rc::new(Cell::new(0));
        let observed = frames.clone();
        let handler = clock.connect_after_paint(move |_| observed.set(observed.get() + 1));
        let context = glib::MainContext::default();
        let started = std::time::Instant::now();
        while frames.get() < 4 && started.elapsed() < std::time::Duration::from_secs(3) {
            window.queue_draw();
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        clock.disconnect(handler);
        assert!(frames.get() >= 4, "real after-paint barrier");
        view.render_backend.sync_geometry_to_pty();
    };

    let mut same_window_grid = None;
    for (state, input) in [
        (BlockState::AwaitingCommand, ""),
        (BlockState::AwaitingCommand, "one\ntwo\nthree\nfour"),
        (BlockState::CollectingOutput, ""),
        (BlockState::AwaitingCommand, ""),
    ] {
        *view.typed_cmd.borrow_mut() = input.into();
        view.bstate.set(state);
        if state == BlockState::CollectingOutput {
            view.active
                .borrow()
                .developer_header
                .as_ref()
                .unwrap()
                .begin("native allocation fixture");
            view.active_vte
                .feed(b"actual native VTE fixture output\r\n");
        }
        view.render_backend.sync_geometry_to_pty();
        settle();
        let grid = view.pty.test_slave_winsize();
        if let Some(expected) = same_window_grid {
            assert_eq!(grid, expected, "{state:?}/{input:?}: no seat-only SIGWINCH");
        } else {
            same_window_grid = Some(grid);
        }
        assert_eq!(grid, pty_grid_size(&native, &view.block_scroll));
        assert_eq!(view.active_vte, native);
        assert!(!view.root.has_css_class("term-chat-root"));
        assert_eq!(anchor.parent().as_ref(), Some(view.block_list.upcast_ref()));
        let clip = view.active.borrow().live_clip();
        assert_eq!(
            clip.parent().unwrap().has_css_class("chat-composer"),
            state == BlockState::AwaitingCommand
        );
        let bounds = clip.compute_bounds(&view.root).unwrap();
        assert!(
            bounds.y() >= 0.0 && bounds.y() + bounds.height() <= view.root.height() as f32 + 1.0
        );
        assert_eq!(clip.overflow(), gtk4::Overflow::Hidden);
    }
    let before_resize = view.pty.test_slave_winsize();
    window.set_default_size(758, 812);
    settle();
    let after_resize = view.pty.test_slave_winsize();
    assert_eq!(before_resize.0, after_resize.0);
    assert!(after_resize.1 > before_resize.1);
    window.set_default_size(758, 662);
    settle();
    assert_eq!(view.pty.test_slave_winsize(), before_resize);
    window.close();
}
