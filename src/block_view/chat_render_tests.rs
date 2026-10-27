//! F13 stage one: native rendering and PTY grid evidence, not shell editing.
use super::*;
use vte4::TerminalExt;

fn settle() {
    let until = std::time::Instant::now() + std::time::Duration::from_millis(250);
    while std::time::Instant::now() < until {
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn child_with_class(root: &gtk4::Widget, class: &str) -> gtk4::Widget {
    if root.has_css_class(class) {
        return root.clone();
    }
    let mut pending = vec![root.clone()];
    while let Some(parent) = pending.pop() {
        let mut child = parent.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if widget.has_css_class(class) {
                return widget;
            }
            pending.push(widget);
        }
    }
    panic!("missing {class}");
}

#[test]
#[ignore = "requires isolated Wayland compositor; scripts/test-chat-render-matrix.py"]
fn chat_native_unicode_render_geometry() {
    gtk4::init().unwrap();
    let out = std::path::PathBuf::from(std::env::var("FORGE_F13_OUTPUT").unwrap());
    let font = std::env::var("FORGE_F13_FONT").unwrap();
    assert!(["DejaVu Sans Mono", "Liberation Mono"].contains(&font.as_str()));
    let mut config = crate::config::Config::safe_defaults();
    config.font_desc = format!("{font} 14");
    config.default_font_scale = 1.0;
    let view = TermView::new_with_spawner(
        &config,
        &crate::config::TerminalMode::Chat,
        &["sh".into()],
        None,
        None,
        &[],
        |_, _, _, _| crate::pty::OwnedPty::for_tests(crate::pty::PtyForeground::Shell),
    )
    .unwrap();
    let window = gtk4::Window::builder()
        .title("Forge F13 native render")
        .default_width(800)
        .default_height(640)
        .child(&view.root)
        .build();
    window.present();
    settle();
    assert!(gtk4::prelude::WidgetExt::display(&window)
        .type_()
        .name()
        .contains("Wayland"));
    let native = view.active_vte.clone();
    // Actual resolved Pango face; a missing requested font must not silently pass.
    let loaded = native
        .pango_context()
        .load_font(&native.font().unwrap())
        .unwrap();
    assert_eq!(loaded.describe().family().as_deref(), Some(font.as_str()));
    let corpus = [
        ("ascii", "printf long-command-argument ".repeat(12)),
        ("zwj", "👩‍💻 👨‍👩‍👧‍👦 ".repeat(12)),
        ("combining", "e\u{301} a\u{308} o\u{302} ".repeat(24)),
        ("wide", "中文 ＡＢＣ 界面 ".repeat(20)),
        ("tabs", "first\tsecond\t界\tend ".repeat(8)),
        (
            "multiline",
            "中文\n👩‍💻 e\u{301}\nfull-width ＡＢＣ\n".repeat(4),
        ),
    ];
    let replies = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen = replies.clone();
    native.connect_commit(move |_, text, _| seen.borrow_mut().push(text.to_string()));
    let mut stage = 0;
    for (label, content) in corpus {
        // Synthetic output exercises the production parser and native renderer.
        // It deliberately does not claim a real shell accepted these bytes.
        let command = format!("{content}F13-END");
        let layout = native.create_pango_layout(Some(&command));
        layout.set_font_description(native.font().as_ref());
        assert_eq!(
            layout.unknown_glyphs_count(),
            0,
            "font fallback must cover this corpus"
        );
        let mut fallback_faces = Vec::new();
        let mut iter = layout.iter();
        loop {
            if let Some(run) = iter.run_readonly() {
                fallback_faces.push(run.item().analysis().font().describe().to_string());
            }
            if !iter.next_run() {
                break;
            }
        }
        assert!(!fallback_faces.is_empty());
        view.pty.write_test_slave(
            format!(
                "\x1b[2J\x1b[H\x1b]133;A\x07$ \x1b]133;B\x07{}",
                command.replace('\n', "\r\n")
            )
            .as_bytes(),
        );
        settle();
        *view.typed_cmd.borrow_mut() = command.clone();
        for width in [800, 480, 800] {
            window.set_default_size(width, 640);
            settle();
            view.render_backend.sync_geometry_to_pty();
            settle();
            assert_eq!(window.width(), width, "compositor must apply actual resize");
            assert_eq!(native, view.active_vte);
            let grid = pty_grid_size(&native, &view.block_scroll);
            assert_eq!(view.pty.test_slave_winsize(), grid);
            assert_eq!(native.column_count(), i64::from(grid.0));
            assert_eq!(native.row_count(), i64::from(grid.1));
            let clip = view.active.borrow().live_clip();
            let controls = child_with_class(view.root.upcast_ref(), "chat-controls");
            let clip_bounds = clip.compute_bounds(&view.root).unwrap();
            let controls_bounds = controls.compute_bounds(&view.root).unwrap();
            for bounds in [&clip_bounds, &controls_bounds] {
                assert!(bounds.width() > 0.0 && bounds.height() > 0.0);
                assert!(bounds.x() >= 0.0 && bounds.y() >= 0.0);
                assert!(bounds.x() + bounds.width() <= view.root.width() as f32 + 1.0);
                assert!(bounds.y() + bounds.height() <= view.root.height() as f32 + 1.0);
            }
            assert!(clip_bounds.y() + clip_bounds.height() <= controls_bounds.y() + 1.0);
            replies.borrow_mut().clear();
            native.feed(b"\x1b[6n");
            settle();
            let (row, col) = replies
                .borrow()
                .iter()
                .find_map(|reply| {
                    let (r, c) = reply
                        .strip_prefix("\x1b[")?
                        .strip_suffix('R')?
                        .split_once(';')?;
                    Some((r.parse::<i64>().ok()?, c.parse::<i64>().ok()?))
                })
                .expect("native screen-relative CPR");
            let origin = native
                .compute_point(&clip, &gtk4::graphene::Point::new(0.0, 0.0))
                .unwrap();
            assert_eq!(native.yalign(), vte4::Align::Start);
            let cursor_top = f64::from(origin.y()) + ((row - 1) * native.char_height()) as f64;
            let cursor_bottom = cursor_top + native.char_height() as f64;
            assert!(row > 0 && col > 0 && col <= native.column_count());
            assert!(cursor_top >= -1.0 && cursor_bottom <= f64::from(clip.height()) + 1.0);
            assert!(native
                .text_format(vte4::Format::Text)
                .unwrap()
                .contains("F13-END"));
            let scrollbar = view.active.borrow().live_scrollbar.clone();
            let bar = scrollbar.compute_bounds(&clip).unwrap();
            let native_bounds = native.compute_bounds(&clip).unwrap();
            let grid_right =
                f64::from(origin.x()) + (native.column_count() * native.char_width()) as f64;
            let scrollbar_paints =
                scrollbar.is_visible() && scrollbar.is_mapped() && scrollbar.opacity() > 0.0;
            // These are independently allocated native objects: do not infer
            // scrollbar clearance from the terminal's own reported columns.
            let visible_right = if scrollbar_paints {
                f64::from(bar.x()).min(f64::from(clip.width()))
            } else {
                f64::from(clip.width())
            };
            let bar_min = scrollbar.measure(gtk4::Orientation::Horizontal, -1);
            assert!(native.is_mapped() && clip.is_mapped());
            let report = serde_json::json!({
                "stage": stage, "case": label, "font": font, "pango_fallback_faces": fallback_faces, "window": [window.width(), window.height()],
                "gtk_buffer_scale": window.scale_factor(), "pty_grid": [grid.0, grid.1],
                "vte_grid": [native.column_count(), native.row_count()],
                "cell": [native.char_width(), native.char_height()], "cursor_cpr": [row,col],
                "horizontal": {
                    "clip_width": clip.width(), "native_content_width": native.width(),
                    "native_bounds": [native_bounds.x(), native_bounds.y(), native_bounds.width(), native_bounds.height()],
                    "native_content_origin": [origin.x(), origin.y()],
                    "grid_right": grid_right, "unobscured_right": visible_right,
                    "scrollbar_bounds": [bar.x(), bar.y(), bar.width(), bar.height()],
                    "scrollbar_paints": scrollbar_paints,
                    "scrollbar_measure": [bar_min.0, bar_min.1],
                    "clearance": visible_right - grid_right,
                    "overlap": (grid_right - visible_right).max(0.0)
                },
                "cursor_clip_y": [cursor_top,cursor_bottom], "clip_height": clip.height(),
                "scope": "synthetic PTY render only; shell draft and paste unverified"
            });
            let ready = out.join(format!("stage-{stage}.json"));
            let tmp = out.join("stage.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
            std::fs::rename(tmp, ready).unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while !out.join(format!("stage-{stage}.ack")).exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "external measured capture acknowledgement"
                );
                while glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            stage += 1;
        }
    }
    assert_eq!(stage, 18);
    // Stable reservation must survive actual native scrollback transitions at
    // each width, rather than merely hiding a scrollbar in a static screenshot.
    let mut stability = Vec::new();
    let mut wide_columns = None;
    *view.typed_cmd.borrow_mut() = "F13".into();
    for width in [800, 480, 800] {
        window.set_default_size(width, 640);
        settle();
        let mut columns = None;
        let mut measured_gutter = None;
        for overflows in [false, true, false] {
            native.reset(true, true);
            if overflows {
                native.feed("scrollback line\r\n".repeat(80).as_bytes());
            } else {
                native.feed(b"F13");
            }
            settle();
            view.render_backend.sync_geometry_to_pty();
            settle();
            assert_eq!(window.width(), width);
            let active = view.active.borrow();
            let clip = active.live_clip();
            let bar = &active.live_scrollbar;
            let adjustment = native.vadjustment().unwrap();
            assert_eq!(
                adjustment.upper() - adjustment.lower() > adjustment.page_size() + f64::EPSILON,
                overflows
            );
            assert!(bar.is_visible() && bar.is_mapped());
            assert_eq!(bar.opacity(), if overflows { 1.0 } else { 0.0 });
            assert_eq!(bar.can_target(), overflows);
            assert_eq!(bar.can_focus(), overflows);
            if !overflows {
                assert!(!bar.grab_focus());
            }
            let gutter = bar.measure(gtk4::Orientation::Horizontal, -1).1;
            assert!(gutter > 0);
            assert_eq!(*measured_gutter.get_or_insert(gutter), gutter);
            let bounds = native.compute_bounds(&clip).unwrap();
            let bar_bounds = bar.compute_bounds(&clip).unwrap();
            let origin = native
                .compute_point(&clip, &gtk4::graphene::Point::new(0.0, 0.0))
                .unwrap();
            let right =
                f64::from(origin.x()) + (native.column_count() * native.char_width()) as f64;
            assert!(f64::from(origin.x()) >= -1e-6);
            assert!(right <= f64::from(bar_bounds.x()) + 1e-6);
            assert!(f64::from(bounds.x() + bounds.width()) <= f64::from(bar_bounds.x()) + 1e-6);
            assert_eq!(bounds.width(), (clip.width() - gutter) as f32);
            let grid = pty_grid_size(&native, &view.block_scroll);
            assert_eq!(view.pty.test_slave_winsize(), grid);
            assert_eq!(i64::from(grid.0), native.column_count());
            assert_eq!(*columns.get_or_insert(grid.0), grid.0);
            if width == 800 {
                assert_eq!(*wide_columns.get_or_insert(grid.0), grid.0);
            }
            stability.push(serde_json::json!({
                "window_width": width, "scrollback": overflows,
                "columns": grid.0, "gutter": gutter, "clip_width": clip.width(),
                "native_bounds": [bounds.x(), bounds.width()],
                "grid_right": right, "scrollbar_left": bar_bounds.x(),
                "scrollbar_opacity": bar.opacity(), "scrollbar_targetable": bar.can_target()
            }));
        }
    }
    std::fs::write(
        out.join("gutter-stability.json"),
        serde_json::to_vec_pretty(&stability).unwrap(),
    )
    .unwrap();
    let before_tiny_columns = native.column_count();
    let tiny = view.active.borrow().test_tiny_chat_allocations();
    std::fs::write(
        out.join("tiny-gutter.json"),
        serde_json::to_vec_pretty(&tiny).unwrap(),
    )
    .unwrap();
    view.render_backend.sync_geometry_to_pty();
    settle();
    assert_eq!(window.width(), 800);
    assert_eq!(native.column_count(), before_tiny_columns);
    assert_eq!(
        view.pty.test_slave_winsize(),
        pty_grid_size(&native, &view.block_scroll)
    );
    window.close();
}
