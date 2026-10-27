//! Real record navigation must expose output hits without unfolding command-only targets.
use super::*;
use gtk4 as gtk;
use std::time::{Duration, Instant};

fn settle() {
    let context = gtk::glib::MainContext::default();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(250) {
        while context.iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn searchable_output(view: &TermView) -> String {
    let cards = view.finished_blocks.borrow();
    let full = cards[0].full_output.borrow();
    let displayed = cards[0].displayed_output.borrow();
    resolved_finished_output(&full, &displayed).to_string()
}

fn collapsed_output_navigation(mode: crate::config::TerminalMode) {
    gtk::init().expect("GTK display");
    let mut config = Config::safe_defaults();
    config.terminal_mode = mode.clone();
    let view = TermView::new_with_spawner(
        &config,
        &mode,
        &["sh".into()],
        None,
        None,
        &[],
        |_, _, _, _| crate::pty::OwnedPty::for_tests(crate::pty::PtyForeground::Shell),
    )
    .unwrap();
    // A long document keeps the last card parked even after bulk folding.
    for id in 1..=32 {
        let output = "long command output\r\n".repeat(200);
        let card = FinishedBlock::new(
            id,
            "$ ",
            "seq 200",
            None,
            &output,
            Some(0),
            &config,
            None,
            None,
            None,
            80,
        );
        if mode == crate::config::TerminalMode::Chat {
            card.set_chat_layout();
        }
        let height = card.set_virtualized(true);
        card.widget()
            .insert_before(&view.block_list, Some(view.active.borrow().widget()));
        view.block_data.borrow_mut().push_back(BlockData {
            id,
            prompt: "$ ".into(),
            cmd: "seq 200".into(),
            cmd_markup: None,
            output,
            exit_code: Some(0),
            lifecycle_schema: blocks::BLOCK_LIFECYCLE_SCHEMA,
            completion_provenance: CompletionProvenance::ShellReported.into(),
            start_mark_seen: true,
            estimated_height: height,
            line_count: 200,
            start_time_ms: None,
            end_time_ms: None,
            duration_ms: None,
            cwd: None,
            cols: 80,
            command_exact: true,
            command_truncated: false,
            output_notice: None,
        });
        view.finished_blocks.borrow_mut().push(card);
    }
    let window = gtk::Window::builder()
        .default_width(640)
        .default_height(480)
        .build();
    window.set_child(Some(&view.root));
    window.present();
    settle();
    view.scroll_debouncer.user_scrolled_up.set(true);
    view.block_scroll.vadjustment().set_value(0.0);
    view.update_viewport();
    view.update_block_visibility();
    settle();
    assert_eq!(
        view.finished_blocks.borrow()[0].widget().height_request(),
        -1
    );
    assert!(view.finished_blocks.borrow()[0].widget().height() > 1);
    assert!(view.finished_blocks.borrow()[31].widget().height_request() > 0);

    let searchable_before = searchable_output(&view);
    assert_eq!(view.set_all_blocks_collapsed(true), 32);
    settle();
    assert!(view.finished_blocks.borrow()[0].widget().is_mapped());
    assert!(!view.finished_blocks.borrow()[0].output_vte.is_mapped());
    assert!(view.scroll_debouncer.user_scrolled_up.get());
    // This is the production path used by a command-only palette hit.
    assert_eq!(
        view.navigate_to_record_id(1, false),
        find::RecordNavigationResult::Navigated
    );
    assert!(view
        .finished_blocks
        .borrow()
        .iter()
        .all(|card| card.is_collapsed()));
    // The palette passes is_output=true for an output hit, before its
    // exact-occurrence highlighter runs. Only that target may unfold.
    assert_eq!(
        view.navigate_to_record_id(1, true),
        find::RecordNavigationResult::Navigated
    );
    assert!(
        !view.finished_blocks.borrow()[0].is_collapsed(),
        "output navigation reported success but left its output hidden"
    );
    assert!(
        view.finished_blocks.borrow()[1..]
            .iter()
            .all(|card| card.is_collapsed()),
        "unrelated folds must remain unchanged"
    );
    assert!(
        view.focus_match_in_block(
            1,
            "long command output",
            find::CrossBlockSearchOptions::default(),
            true,
            0,
        ),
        "the palette's immediate highlighter must reach the requested output hit"
    );
    assert_eq!(
        searchable_output(&view),
        searchable_before,
        "navigation must preserve the current searchable output/filter view"
    );
    assert_eq!(
        view.navigate_to_record_id(1, true),
        find::RecordNavigationResult::Navigated
    );
    settle();
    assert!(
        view.finished_blocks.borrow()[0].output_vte.is_mapped(),
        "the selected output must be visible after layout"
    );
    assert!(
        view.scroll_debouncer.user_scrolled_up.get(),
        "search navigation must not resume follow-bottom"
    );
    let parked = view.block_scroll.vadjustment().value();
    view.scroll_debouncer.mark_dirty(&view.block_scroll);
    view.scroll_debouncer
        .pin_to_bottom_deferred(&view.block_scroll);
    settle();
    assert!(
        (view.block_scroll.vadjustment().value() - parked).abs() < 1.0,
        "streaming follow-bottom must respect the search position"
    );
    window.close();
}

#[test]
#[ignore = "requires an isolated GTK process and display"]
fn block_output_search_unfolds_only_target() {
    collapsed_output_navigation(crate::config::TerminalMode::Block);
}

#[test]
#[ignore = "requires an isolated GTK process and display"]
fn chat_output_search_unfolds_only_target() {
    collapsed_output_navigation(crate::config::TerminalMode::Chat);
}

fn retained_scrollback_find(mode: crate::config::TerminalMode) {
    for new_lines in [0, 3, 80] {
        retained_scrollback_case(mode.clone(), new_lines);
    }
}

fn retained_scrollback_case(mode: crate::config::TerminalMode, new_lines: usize) {
    super::alt_screen::set_find_render_trace(std::env::var_os("FORGE_TEST_TRACE_FIND").is_some());
    gtk::init().expect("GTK display");
    let mut config = Config::safe_defaults();
    config.terminal_mode = mode.clone();
    config.preserve_live_scrollback = true;
    let view = TermView::new_with_spawner(
        &config,
        &mode,
        &["sh".into()],
        None,
        None,
        &[],
        |_, _, _, _| crate::pty::OwnedPty::for_tests(crate::pty::PtyForeground::Shell),
    )
    .unwrap();
    let window = gtk::Window::builder()
        .default_width(640)
        .default_height(320)
        .build();
    window.set_child(Some(&view.root));
    window.present();
    // Establish the running grid before recording absolute source rows. A
    // synthetic idle-to-running resize after capture is not a shell boundary.
    view.ftcs_seen.set(true);
    view.bstate.set(BlockState::CollectingOutput);
    settle();
    {
        view.active_vte
            .feed("needle old\r\n".repeat(100).as_bytes());
        settle();
        let origin = view.active_vte.cursor_position().1;
        view.active.borrow().live_cursor_origin().set(Some(origin));
        view.active_vte.feed(b"$ run\r\n");
        view.active_vte
            .feed("needle new\r\n".repeat(new_lines).as_bytes());
        settle();
        assert_eq!(
            view.active.borrow().live_cursor_origin().get(),
            Some(origin),
            "layout must preserve the captured origin"
        );
        let result = view.find_in_blocks("needle.*", true);
        if new_lines == 0 {
            assert_eq!(result, find::FindSearchResult::NoMatches);
            assert!(!view.active_vte.has_selection());
            window.close();
            settle();
            return;
        }
        let find::FindSearchResult::Matches(progress) = result else {
            panic!("running output must match");
        };
        assert_eq!(
            progress.total, new_lines,
            "old scrollback must not be counted"
        );
        assert_eq!(progress.current, 1);
        assert_eq!(
            view.active_vte.text_selected(vte4::Format::Text).as_deref(),
            Some("needle new")
        );
        // Previous at the first match wraps only within the running range.
        let find::FindNavigationResult::Progress(progress) = view.find_prev() else {
            panic!("wrap backwards");
        };
        assert_eq!(progress.current, new_lines);
        assert_eq!(
            view.active_vte.text_selected(vte4::Format::Text).as_deref(),
            Some("needle new")
        );
        let find::FindNavigationResult::Progress(progress) = view.find_next() else {
            panic!("wrap forwards");
        };
        assert_eq!(progress.current, 1);
        assert_eq!(
            view.active_vte.text_selected(vte4::Format::Text).as_deref(),
            Some("needle new")
        );
        for expected in 2..=new_lines {
            let next = view.find_next();
            let find::FindNavigationResult::Progress(progress) = next else {
                panic!("{new_lines} lines, step {expected}: {next:?}");
            };
            assert_eq!(progress.current, expected);
            assert_eq!(
                view.active_vte.text_selected(vte4::Format::Text).as_deref(),
                Some("needle new")
            );
        }
        let find::FindNavigationResult::Progress(progress) = view.find_prev() else {
            panic!("reverse navigation must keep the same range");
        };
        assert_eq!(progress.current, new_lines - 1);
        // An absent origin conservatively retains the old whole-ring scope.
        view.active.borrow().live_cursor_origin().set(None);
        let find::FindSearchResult::Matches(progress) = view.find_in_blocks("needle.*", true)
        else {
            panic!("unknown origin must remain searchable");
        };
        assert_eq!(progress.total, 100 + new_lines);
        view.active.borrow().live_cursor_origin().set(Some(origin));
    }
    // A finished surface followed by the live surface exercises returning in
    // both directions, including a logical wrap that must skip old ring rows.
    let card = FinishedBlock::new(
        1,
        "$ ",
        "done",
        None,
        "needle finished\r\n",
        Some(0),
        &config,
        None,
        None,
        None,
        80,
    );
    if mode == crate::config::TerminalMode::Chat {
        card.set_chat_layout();
    }
    card.widget()
        .insert_before(&view.block_list, Some(view.active.borrow().widget()));
    view.finished_blocks.borrow_mut().push(card);
    settle();
    let find::FindSearchResult::Matches(progress) = view.find_in_blocks("needle.*", true) else {
        panic!("finished and live matches");
    };
    assert_eq!(progress.total, new_lines + 1);
    assert_eq!(progress.current, 1);
    let find::FindNavigationResult::Progress(progress) = view.find_prev() else {
        panic!("return to live backwards");
    };
    assert_eq!(progress.current, new_lines + 1);
    assert_eq!(
        view.active_vte.text_selected(vte4::Format::Text).as_deref(),
        Some("needle new")
    );
    let find::FindNavigationResult::Progress(progress) = view.find_next() else {
        panic!("return to finished");
    };
    assert_eq!(progress.current, 1);
    let find::FindNavigationResult::Progress(progress) = view.find_next() else {
        panic!("return to live forwards");
    };
    assert_eq!(progress.current, 2);
    assert_eq!(
        view.active_vte.text_selected(vte4::Format::Text).as_deref(),
        Some("needle new")
    );
    window.close();
    settle();
}

#[test]
#[ignore = "requires an isolated GTK process and display"]
fn block_live_find_excludes_retained_scrollback() {
    retained_scrollback_find(crate::config::TerminalMode::Block);
}

#[test]
#[ignore = "requires an isolated GTK process and display"]
fn chat_live_find_excludes_retained_scrollback() {
    retained_scrollback_find(crate::config::TerminalMode::Chat);
}

fn live_find_observes_native_changes(mode: crate::config::TerminalMode) {
    for change in ["append", "rewrite", "resize"] {
        live_find_change_case(mode.clone(), change);
    }
}

fn live_find_change_case(mode: crate::config::TerminalMode, change: &str) {
    gtk::init().expect("GTK display");
    let mut config = Config::safe_defaults();
    config.terminal_mode = mode.clone();
    config.preserve_live_scrollback = true;
    let view = TermView::new_with_spawner(
        &config,
        &mode,
        &["sh".into()],
        None,
        None,
        &[],
        |_, _, _, _| crate::pty::OwnedPty::for_tests(crate::pty::PtyForeground::Shell),
    )
    .unwrap();
    let window = gtk::Window::builder()
        .default_width(640)
        .default_height(320)
        .build();
    window.set_child(Some(&view.root));
    window.present();
    view.ftcs_seen.set(true);
    view.bstate.set(BlockState::CollectingOutput);
    settle();
    view.active_vte
        .feed("older output\r\n".repeat(40).as_bytes());
    settle();
    let origin = view.active_vte.cursor_position().1;
    view.active.borrow().live_cursor_origin().set(Some(origin));
    view.active_vte.feed(b"$ run\r\nneedle\r\n");
    settle();
    let find::FindSearchResult::Matches(progress) = view.find_in_blocks("needle", false) else {
        panic!("initial live match");
    };
    assert_eq!(progress.total, 1);
    assert_eq!(
        view.active_vte.text_selected(vte4::Format::Text).as_deref(),
        Some("needle")
    );

    // Ordinary scrolling/selection do not change terminal contents and must
    // not invalidate a query or create a find/rebuild feedback loop.
    let generation = view.contents_generation.get();
    let adjustment = view.active_vte.vadjustment().unwrap();
    adjustment.set_value(adjustment.lower());
    view.active_vte.select_all();
    view.active_vte.unselect_all();
    settle();
    assert_eq!(view.contents_generation.get(), generation);
    assert!(matches!(
        view.find_next(),
        find::FindNavigationResult::Progress(_)
    ));
    assert!(matches!(
        view.find_in_blocks("needle", false),
        find::FindSearchResult::Matches(_)
    ));

    match change {
        "append" => view.active_vte.feed(b"needle\r\n"),
        "rewrite" => view.active_vte.feed(b"\x1b[1A\r\x1b[2Kgone\r\n"),
        "resize" => {
            let old_grid = (view.active_vte.row_count(), view.active_vte.column_count());
            window.set_default_size(860, 440);
            settle();
            assert_ne!(
                (view.active_vte.row_count(), view.active_vte.column_count()),
                old_grid,
                "fixture must change the native search geometry"
            );
        }
        _ => unreachable!(),
    }
    settle();
    assert_eq!(
        view.find_next(),
        find::FindNavigationResult::Invalidated,
        "{change}: stale one-hit cursor must not report old progress"
    );
    // This is the existing find bar's recovery path: keep its query and
    // rebuild on Invalidated, without replaying any input to the PTY.
    let result = view.find_in_blocks("needle", false);
    if change == "rewrite" {
        assert_eq!(result, find::FindSearchResult::NoMatches);
        assert!(!view.active_vte.has_selection());
    } else {
        let find::FindSearchResult::Matches(progress) = result else {
            panic!("refreshed live match");
        };
        assert_eq!(progress.total, if change == "append" { 2 } else { 1 });
        assert_eq!(
            view.active_vte.text_selected(vte4::Format::Text).as_deref(),
            Some("needle")
        );
        assert!(matches!(
            view.find_next(),
            find::FindNavigationResult::Progress(_)
        ));
    }
    window.close();
    settle();
}

#[test]
#[ignore = "requires an isolated GTK process and display"]
fn block_live_find_rebuilds_after_native_changes() {
    live_find_observes_native_changes(crate::config::TerminalMode::Block);
}

#[test]
#[ignore = "requires an isolated GTK process and display"]
fn chat_live_find_rebuilds_after_native_changes() {
    live_find_observes_native_changes(crate::config::TerminalMode::Chat);
}

/// Characterize the public native API before changing Forge's counting model.
/// This is deliberately not a claim that F01 is implemented: native forward
/// and backward cursors have different identities on a softwrapped line.
#[test]
#[ignore = "requires an isolated GTK process and display"]
fn native_search_identity_characterization() {
    gtk::init().expect("GTK display");
    // SAFETY: these version functions have no pointer arguments or ownership.
    let version = unsafe {
        (
            vte4::ffi::vte_get_major_version(),
            vte4::ffi::vte_get_minor_version(),
            vte4::ffi::vte_get_micro_version(),
        )
    };
    // SAFETY: a null destination queries the required storage size.
    let pcre_size =
        unsafe { pcre2_sys::pcre2_config_8(pcre2_sys::PCRE2_CONFIG_VERSION, std::ptr::null_mut()) };
    assert!((1..=128).contains(&pcre_size));
    let mut pcre_version = vec![0u8; pcre_size as usize];
    // SAFETY: the buffer has exactly the size PCRE2 just reported, including
    // its terminator; the same immutable configuration is queried below.
    let pcre_result = unsafe {
        pcre2_sys::pcre2_config_8(
            pcre2_sys::PCRE2_CONFIG_VERSION,
            pcre_version.as_mut_ptr().cast(),
        )
    };
    assert!(pcre_result > 0 && (pcre_result as usize) <= pcre_version.len());
    let end = pcre_version.iter().position(|byte| *byte == 0).unwrap();
    eprintln!(
        "F01 native contract: VTE {}.{}.{}; PCRE2 {}",
        version.0,
        version.1,
        version.2,
        String::from_utf8_lossy(&pcre_version[..end])
    );

    let terminal = vte4::Terminal::new();
    terminal.set_size(40, 8);
    terminal.set_scrollback_lines(256);
    let window = gtk::Window::new();
    window.set_child(Some(&terminal));
    window.present();
    settle();
    let columns = terminal.column_count();
    assert!(columns >= 20, "fixture needs two unwrapped distinct hits");
    let contents_revision = Rc::new(Cell::new(0u64));
    let observed = contents_revision.clone();
    terminal.connect_contents_changed(move |_| observed.set(observed.get() + 1));
    let flags = pcre2_sys::PCRE2_MULTILINE | pcre2_sys::PCRE2_UTF | pcre2_sys::PCRE2_UCP;
    // Limits apply inside each native PCRE2 call, not just between calls.
    let pattern = "(*LIMIT_MATCH=10000)(*LIMIT_DEPTH=100)(*LIMIT_HEAP=1024)needle-[A-C]";
    let regex = vte4::Regex::for_search(pattern, flags).unwrap();
    terminal.feed(b"needle-A needle-B\r\nneedle-C\r\n");
    settle();
    let adjustment = terminal.vadjustment().unwrap();
    let reset = |backwards: bool, wrap: bool| {
        terminal.unselect_all();
        terminal.search_set_regex(Some(&regex), 0);
        terminal.search_set_wrap_around(wrap);
        adjustment.set_value(if backwards {
            (adjustment.upper() - adjustment.page_size()).max(adjustment.lower())
        } else {
            adjustment.lower()
        });
    };
    let selected = || {
        terminal
            .text_selected(vte4::Format::Text)
            .expect("successful nonempty search has a selection")
            .to_string()
    };
    let enumerate = |backwards: bool| {
        let mut found = Vec::new();
        for _ in 0..16 {
            let success = if backwards {
                terminal.search_find_previous()
            } else {
                terminal.search_find_next()
            };
            if !success {
                return found;
            }
            found.push(selected());
        }
        panic!("bounded fixture did not terminate with native wrapping disabled");
    };
    reset(false, false);
    let forward = enumerate(false);
    reset(true, false);
    let backward = enumerate(true);
    eprintln!("F01 same physical line: forward={forward:?}; backward={backward:?}");
    assert_eq!(forward, ["needle-A", "needle-C"]);
    assert_eq!(backward, ["needle-C", "needle-A"]);

    // Exact physical geometry is derived from the REAL allocated grid. There
    // is one hard newline after B; A and B belong to one softwrapped line.
    terminal.reset(true, true);
    terminal.feed(
        format!(
            "needle-A{}needle-B\r\nneedle-C\r\n",
            " ".repeat(columns as usize - "needle-A".len())
        )
        .as_bytes(),
    );
    settle();
    reset(false, false);
    let forward = enumerate(false);
    reset(true, false);
    let backward = enumerate(true);
    eprintln!("F01 softwrap columns={columns}: forward={forward:?}; backward={backward:?}");
    assert_eq!(forward, ["needle-A", "needle-B", "needle-C"]);
    assert_eq!(backward, ["needle-C", "needle-A"]);

    // Wrap produces a cycle, not an end sentinel. Text alone cannot be a
    // cycle identity in production (distinct rows may contain the same text).
    reset(false, true);
    let wrapped: Vec<_> = (0..6)
        .map(|_| {
            assert!(terminal.search_find_next());
            selected()
        })
        .collect();
    eprintln!("F01 wrap cycle: {wrapped:?}");
    assert_eq!(
        wrapped,
        ["needle-A", "needle-B", "needle-C", "needle-A", "needle-B", "needle-C"]
    );

    // Only a search-owned known ordinal can be restored with public search
    // APIs. A general user selection has no public exact-range restore API.
    reset(false, false);
    assert!(terminal.search_find_next());
    assert!(terminal.search_find_next());
    let before = selected();
    let scroll_before = adjustment.value();
    let revision_before = contents_revision.get();
    reset(false, false);
    assert_eq!(enumerate(false), forward);
    reset(false, false);
    for _ in 0..2 {
        assert!(terminal.search_find_next());
    }
    adjustment.set_value(scroll_before);
    assert_eq!(selected(), before);
    assert_eq!(adjustment.value(), scroll_before);
    assert_eq!(contents_revision.get(), revision_before);

    terminal.feed(b"needle-A\r\n");
    settle();
    assert_ne!(
        contents_revision.get(),
        revision_before,
        "append retires a counted revision"
    );
    let geometry_before = (terminal.column_count(), terminal.row_count());
    window.set_default_size(window.width() + 160, window.height() + 80);
    settle();
    assert_ne!(
        (terminal.column_count(), terminal.row_count()),
        geometry_before,
        "resize retires geometry identity"
    );

    // Unicode ranges use VTE's actual PCRE2 UTF/UCP flags, including a wide
    // glyph and combining text. Distinct hard lines avoid the same-row skip.
    terminal.reset(true, true);
    terminal.feed("测试-A\r\ne\u{301}-B\r\n١-C\r\n".as_bytes());
    settle();
    let unicode = vte4::Regex::for_search("测试-A|e\\p{M}-B|\\d-C", flags).unwrap();
    terminal.unselect_all();
    terminal.search_set_regex(Some(&unicode), 0);
    terminal.search_set_wrap_around(false);
    adjustment.set_value(adjustment.lower());
    assert_eq!(enumerate(false), ["测试-A", "e\u{301}-B", "١-C"]);
    // Native NOTEMPTY behavior does not grant the application permission to
    // silently reinterpret an empty-match query. The future admission layer
    // must reject it before claiming a complete count.
    for empty in [r"^", r"\b", r"测试\K"] {
        let empty_regex = vte4::Regex::for_search(empty, flags).unwrap();
        terminal.unselect_all();
        terminal.search_set_regex(Some(&empty_regex), 0);
        adjustment.set_value(adjustment.lower());
        assert!(!terminal.search_find_next(), "empty selection: {empty}");
        assert!(!terminal.has_selection());
    }
    assert!(vte4::Regex::for_search(r"\C", flags).is_err());

    // Equal selected strings are different native positions. Text equality
    // therefore cannot be used as a cycle detector by the eventual adapter.
    terminal.reset(true, true);
    terminal.feed(b"same-hit\r\nsame-hit\r\n");
    settle();
    let repeated = vte4::Regex::for_search("same-hit", flags).unwrap();
    terminal.unselect_all();
    terminal.search_set_regex(Some(&repeated), 0);
    terminal.search_set_wrap_around(false);
    adjustment.set_value(adjustment.lower());
    assert_eq!(enumerate(false), ["same-hit", "same-hit"]);
    terminal.search_set_regex(None::<&vte4::Regex>, 0);
    terminal.unselect_all();
    window.close();
    settle();
}
