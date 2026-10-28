//! Real GTK allocation / slave TIOCGWINSZ checks for the new Block seats.
use super::*;

#[test]
#[ignore = "requires DISPLAY; run explicitly under Xvfb"]
fn resize_reanchor_does_not_absorb_shell_side_prefill() {
    gtk4::init().unwrap();
    crate::ui::reference_shell::install_css();
    let view = TermView::new_with_spawner(
        &crate::config::Config::safe_defaults(),
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
    let settle = || {
        let clock = window.frame_clock().unwrap();
        let frames = Rc::new(Cell::new(0));
        let observed = frames.clone();
        let signal = clock.connect_after_paint(move |_| observed.set(observed.get() + 1));
        let context = glib::MainContext::default();
        let started = std::time::Instant::now();
        while frames.get() < 8 && started.elapsed() < std::time::Duration::from_secs(3) {
            window.queue_draw();
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        clock.disconnect(signal);
        assert!(frames.get() >= 8);
    };
    settle();
    view.pty.write_test_slave(b"\x1b]133;A\x07> \x1b]133;B\x07");
    settle();
    assert!(view.prompt_anchor_ready.get());
    let anchor = view.prompt_end_pos.get();
    let anchor_rows = view.prompt_anchor_rows.get();
    let resize_epoch = view.prompt_anchor_resize_generation.get();
    let input_generation = view.accepted_input_generation.get();
    // Child-origin text has no local typed shadow. A matching resize epoch is
    // not permission to move the trusted boundary beyond this native prefill.
    view.pty.write_test_slave(b"shell_prefill_keep");
    settle();
    assert!(view.typed_cmd.borrow().is_empty());
    assert_eq!(view.accepted_input_generation.get(), input_generation);
    window.set_default_size(658, 812);
    settle();
    assert!(
        view.pty_resize_generation.get() > resize_epoch,
        "real native geometry must publish a new PTY resize epoch"
    );
    assert_ne!(view.active_vte.row_count(), anchor_rows);
    assert_eq!(view.prompt_end_pos.get(), anchor);
    assert_eq!(view.prompt_anchor_rows.get(), anchor_rows);
    assert_eq!(view.prompt_anchor_resize_generation.get(), resize_epoch);
    assert_eq!(view.command_prompt_status(), CommandPromptStatus::HasInput);
    assert_eq!(
        view.active_vte
            .text_format(vte4::Format::Text)
            .unwrap()
            .matches("shell_prefill_keep")
            .count(),
        1
    );
    assert!(view.block_data.borrow().is_empty());
    window.close();
}

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

#[test]
#[ignore = "requires DISPLAY; run explicitly under Xvfb"]
fn real_more_fold_restores_native_height_and_document_span() {
    gtk4::init().unwrap();
    crate::ui::reference_shell::install_css();
    let view = TermView::new_with_spawner(
        &crate::config::Config::safe_defaults(),
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
    let settle = || {
        let clock = window.frame_clock().unwrap();
        let frames = Rc::new(Cell::new(0));
        let observed = frames.clone();
        let signal = clock.connect_after_paint(move |_| observed.set(observed.get() + 1));
        let context = glib::MainContext::default();
        let started = std::time::Instant::now();
        while frames.get() < 8 && started.elapsed() < std::time::Duration::from_secs(3) {
            window.queue_draw();
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        clock.disconnect(signal);
        assert!(frames.get() >= 8, "native allocation/paint barrier");
    };
    settle();
    view.pty.write_test_slave(b"\x1b]133;A\x07> \x1b]133;B\x07");
    settle();
    view.pty.write_test_slave(b"fixture_output\r\n\x1b]133;C;cmdline_url=fixture_output\x07first\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\n\x1b]133;D;0\x07\x1b]133;A\x07> \x1b]133;B\x07");
    settle();
    assert_eq!(view.finished_blocks.borrow().len(), 1);
    let card = view.finished_blocks.borrow()[0].clone();
    let original_output = card.output_vte.text_format(vte4::Format::Text).unwrap();
    let source_output = view.block_data.borrow()[0].output.clone();
    view.scroll_debouncer.user_scrolled_up.set(true);
    let menu_button = |name: &str| {
        let mut pending = vec![window.clone().upcast::<gtk4::Widget>()];
        while let Some(widget) = pending.pop() {
            if let Some(button) = widget.downcast_ref::<gtk4::Button>() {
                if button.label().as_deref() == Some(name) && button.is_mapped() {
                    return button.clone();
                }
            }
            let mut next = widget.first_child();
            while let Some(child) = next {
                next = child.next_sibling();
                pending.push(child);
            }
        }
        panic!("actual mapped More action missing: {name}");
    };
    card.more_btn.emit_clicked();
    settle();
    let expanded_height = card.widget().height();
    menu_button("Hide Output").emit_clicked();
    settle();
    assert!(card.is_collapsed());
    assert!(!card.output_vte.is_mapped());
    assert!(card.widget().height() < expanded_height);
    assert_eq!(
        view.block_data.borrow()[0].estimated_height,
        card.widget().height()
    );
    assert_eq!(
        view.block_document_index.borrow().total(),
        i64::from(card.widget().height())
    );
    card.more_btn.emit_clicked();
    settle();
    menu_button("Show Output").emit_clicked();
    settle();
    assert!(!card.is_collapsed());
    assert!(card.output_vte.is_mapped());
    assert_eq!(card.widget().height(), expanded_height);
    assert_eq!(
        view.block_data.borrow()[0].estimated_height,
        expanded_height
    );
    assert_eq!(
        view.block_document_index.borrow().total(),
        i64::from(expanded_height)
    );
    assert_eq!(
        card.output_vte.text_format(vte4::Format::Text).unwrap(),
        original_output
    );
    assert_eq!(view.block_data.borrow()[0].output, source_output);
    assert!(view.scroll_debouncer.user_scrolled_up.get());

    // Exercise a menu in real scrolling history, with the reader anchored in
    // an earlier card and the action's header still visible below it. Static
    // restoration at document y=0 cannot establish this reading-position gate.
    // Enough real history remains after a fold even at the compact12px
    // default; the strict anchor must not sit in a clamped live-edge range.
    for _ in 0..5 {
        view.pty.write_test_slave(b"fixture_output\r\n\x1b]133;C;cmdline_url=fixture_output\x07first\r\nsecond\r\nthird\r\nfourth\r\nfifth\r\nsixth\r\n\x1b]133;D;0\x07\x1b]133;A\x07> \x1b]133;B\x07");
        settle();
    }
    assert_eq!(view.finished_blocks.borrow().len(), 6);
    view.scroll_debouncer
        .scroll_wheel(&view.block_scroll, 87.25, true);
    view.update_viewport();
    view.update_block_visibility();
    settle();
    let reading_anchor = || {
        let position = view.block_scroll.vadjustment().value();
        let index = view.block_document_index.borrow();
        let item = index.card_at_document_y(position.floor() as i64).unwrap();
        let records = view.block_data.borrow();
        let prefix: i64 = records
            .iter()
            .take(item)
            .map(|record| i64::from(block_document_height(record.estimated_height)))
            .sum();
        (records[item].id, position - prefix as f64)
    };
    let before = reading_anchor();
    assert_eq!(before.0, card.id);
    assert_eq!(before.1, 87.25, "retain fractional reading coordinates");
    let lower = view.finished_blocks.borrow()[1].clone();
    assert!(
        view.block_scroll.vadjustment().upper() - view.block_scroll.vadjustment().page_size()
            > before.1 + f64::from(lower.widget().height()),
        "folding the lower card must leave the reading anchor away from a clamp"
    );
    let header = lower.more_btn.compute_bounds(&view.block_scroll).unwrap();
    assert!(
        header.y() > 0.0
            && header.y() + header.height() < view.block_scroll.vadjustment().page_size() as f32,
        "the real menu header must be visible below the reading anchor"
    );
    let reading = view.scroll_debouncer.reference_state();
    for (action, collapsed) in [("Hide Output", true), ("Show Output", false)] {
        lower.more_btn.emit_clicked();
        settle();
        assert_eq!(
            reading_anchor(),
            before,
            "opening {action} changed the reader"
        );
        menu_button(action).emit_clicked();
        settle();
        assert_eq!(lower.is_collapsed(), collapsed);
        assert_eq!(
            reading_anchor(),
            before,
            "{action} moved the reading anchor"
        );
        assert_eq!(
            view.scroll_debouncer.reference_state(),
            reading,
            "{action} changed follow intent or its motion target"
        );
        let data = view.block_data.borrow();
        assert_eq!(data[1].estimated_height, lower.widget().height());
        assert_eq!(
            view.block_document_index.borrow().total(),
            data.iter()
                .map(|record| i64::from(block_document_height(record.estimated_height)))
                .sum::<i64>()
        );
    }
    window.close();
}
