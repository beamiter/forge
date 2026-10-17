//! Display-backed font contract across actual native pane controllers.
use super::{css, PtyForeground, TermView};
use crate::config::{Config, TerminalMode};
use gtk::prelude::*;
use gtk4 as gtk;
use vte4::TerminalExt;

fn settle() {
    let context = gtk::glib::MainContext::default();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(150);
    while std::time::Instant::now() < until {
        for _ in 0..128 {
            if !context.iteration(false) {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn pane(config: &Config, mode: TerminalMode) -> TermView {
    TermView::new_with_spawner(
        config,
        &mode,
        &["sh".into()],
        None,
        None,
        &[],
        |_, _, _, _| crate::pty::OwnedPty::for_tests(PtyForeground::Shell),
    )
    .expect("nonexecuting font fixture PTY")
}

fn finish_command(view: &TermView) {
    view.pty.write_test_slave(
        b"\x1b]133;A\x07$ \x1b]133;B\x07echo font\r\n\x1b]133;C;cmdline=echo font\x07font\r\n",
    );
    settle();
    view.pty
        .write_test_slave(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
    settle();
}

fn labels(widget: &gtk::Widget) -> Vec<gtk::Label> {
    let mut all = Vec::new();
    if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
        all.push(label);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        all.extend(labels(&current));
    }
    all
}

fn size(widget: &impl IsA<gtk::Widget>) -> i32 {
    widget.pango_context().font_description().unwrap().size()
}

fn assert_configured_size(widget: &impl IsA<gtk::Widget>, config: &Config, scale: f64) {
    let configured = gtk::pango::FontDescription::from_string(&config.font_desc);
    let actual = widget.pango_context().font_description().unwrap();
    let mut expected = f64::from(configured.size()) * scale;
    // CSS resolves absolute pixels against the pinned 96 dpi. Account for
    // whether GTK exposes its Pango context description in points or pixels.
    match (configured.is_size_absolute(), actual.is_size_absolute()) {
        (true, false) => expected *= 72.0 / 96.0,
        (false, true) => expected *= 96.0 / 72.0,
        _ => {}
    }
    assert!(
        (f64::from(actual.size()) - expected).abs() <= 1.0,
        "configured {} × {} must resolve to {expected}, got {actual:?}",
        config.font_desc,
        scale,
    );
}

#[test]
#[ignore = "requires an isolated GTK/VTE display"]
fn font_groups_stay_independent_across_native_panes_and_late_rows() {
    gtk::init().expect("GTK display");
    // Make point/pixel expectations independent of the runner's Xft defaults.
    gtk::Settings::default()
        .unwrap()
        .set_property("gtk-xft-dpi", 96 * 1024);
    let mut config = Config::safe_defaults();
    config.font_desc = "Monospace 12.5".into();
    config.default_font_scale = 1.025;
    config.chrome_font_size = "12.5".into();
    config.icon_font = Some("none".into());
    let display = gtk::gdk::Display::default().unwrap();
    let base = gtk::CssProvider::new();
    base.load_from_string(&format!(
        "{} .file-tree-root {{ font-size: 0.85em; }} \
         .tab-process-indicator {{ font-size: 0.8em; }} \
         .tab-conn-dot {{ font-size: 0.7em; }}",
        crate::ui::PANE_HEADER_CSS
    ));
    gtk::style_context_add_provider_for_display(
        &display,
        &base,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let chrome = gtk::CssProvider::new();
    let errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let caught = errors.clone();
    chrome.connect_parsing_error(move |_, _, error| caught.borrow_mut().push(error.to_string()));
    gtk::style_context_add_provider_for_display(
        &display,
        &chrome,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    let refresh = |config: &Config| {
        chrome.load_from_string(&format!(
            "{} {}",
            crate::font::menu_theme_css(&config.font_desc, &config.background, &config.foreground),
            crate::font::window_font_css(
                &config.font_desc,
                config.default_font_scale,
                &config.chrome_font_size,
                config.chrome_font_scale
            ),
        ));
    };
    refresh(&config);

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // Match the production overlay/scroller sizing contract, including the
    // width clamp which keeps long tabs from widening the application window.
    let top = gtk::Overlay::new();
    top.add_css_class("top-bar");
    top.add_css_class("forge-font-chrome");
    top.set_height_request(40);
    top.set_child(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    let tab_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let title = gtk::Label::new(Some("Top tab"));
    tab_row.append(&title);
    let tab_scroll = gtk::ScrolledWindow::new();
    tab_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
    tab_scroll.set_min_content_width(0);
    tab_scroll.set_max_content_width(1);
    tab_scroll.set_propagate_natural_width(false);
    tab_scroll.set_child(Some(&tab_row));
    top.add_overlay(&tab_scroll);
    top.set_measure_overlay(&tab_scroll, true);
    let dot = gtk::Label::new(Some("●"));
    dot.add_css_class("tab-conn-dot");
    tab_row.append(&dot);
    let icon = gtk::Image::from_icon_name("window-close-symbolic");
    icon.set_pixel_size(16);
    tab_row.append(&icon);
    root.append(&top);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar.add_css_class("sidebar-box");
    sidebar.add_css_class("forge-font-chrome");
    let file = gtk::Label::new(Some("Files"));
    file.add_css_class("file-tree-root");
    sidebar.append(&file);
    let process = gtk::Label::new(Some("shell"));
    process.add_css_class("tab-process-indicator");
    sidebar.append(&process);
    let entry = gtk::Entry::new();
    sidebar.append(&entry);
    let text = gtk::TextView::new();
    text.buffer().set_text("Agent input");
    sidebar.append(&text);
    body.append(&sidebar);
    let notebook = gtk::Notebook::new();
    notebook.set_hexpand(true);
    notebook.set_vexpand(true);
    body.append(&notebook);
    root.append(&body);

    let block = pane(&config, TerminalMode::Block);
    let chat = pane(&config, TerminalMode::Chat);
    let split = gtk::Paned::new(gtk::Orientation::Horizontal);
    split.set_start_child(Some(&block.widget()));
    split.set_end_child(Some(&chat.widget()));
    notebook.append_page(&split, Some(&gtk::Label::new(Some("Split"))));
    let classic = crate::terminal::create_terminal(&config);
    notebook.append_page(&classic, Some(&gtk::Label::new(Some("Classic"))));
    for view in [&block, &chat] {
        view.pane_header()
            .set_status(1, "Pane", Some("/workspace"), Some("sh"));
        view.pane_header().set_header_visible(true);
    }
    let window = gtk::Window::builder()
        .default_width(1100)
        .default_height(700)
        .child(&root)
        .build();
    window.add_css_class("forge-window");
    window.present();
    settle();
    finish_command(&block);
    finish_command(&chat);

    let check = |config: &Config| {
        let expected = size(&title);
        assert!(expected > 0);
        for label in labels(&root.clone().upcast()) {
            // GTK updates a hidden widget's Pango context lazily. Assert the
            // rendered contract here; reveal previously hidden text below.
            if label.is_mapped() && !label.has_css_class("tab-conn-dot") {
                let scale = if label.is_ancestor(&top) || label.is_ancestor(&sidebar) {
                    config.chrome_font_scale
                } else {
                    config.default_font_scale
                };
                let mut expected_config = config.clone();
                if label.is_ancestor(&top) || label.is_ancestor(&sidebar) {
                    expected_config.font_desc = config.chrome_font_size.clone();
                }
                assert_configured_size(&label, &expected_config, scale);
            }
        }
        assert_eq!(size(&entry), expected);
        assert_eq!(size(&text), expected);
        assert!(
            size(&dot) < expected,
            "the connection glyph retains its scale"
        );
        assert_eq!(icon.pixel_size(), 16, "text zoom must not resize images");
        assert!(errors.borrow().is_empty(), "{:?}", errors.borrow());
    };
    check(&config);
    assert_eq!(size(&title), size(&entry));
    let original_size = size(&title);
    // Setting change, fractional hotkey steps, reset, and absolute-pixel fonts.
    for (desc, scale) in [
        ("Monospace Bold 13.5", 1.0),
        ("Monospace Bold 13.5", 1.025),
        ("Monospace Bold 13.5", 0.9),
        ("Monospace Bold 13.5", 1.0),
        ("Monospace 17px", 1.25),
    ] {
        config.font_desc = desc.into();
        config.default_font_scale = scale;
        for view in [&block, &chat] {
            view.set_font(desc);
            view.set_font_scale(scale);
            assert_eq!(view.config.borrow().font_desc, desc);
            assert_eq!(view.config.borrow().default_font_scale, scale);
            assert_eq!(view.vte().font_scale(), scale);
            for finished in view.finished_blocks.borrow().iter() {
                assert_eq!(
                    finished.command_vte.font().unwrap().size(),
                    view.vte().font().unwrap().size()
                );
                assert_eq!(
                    finished.output_vte.font().unwrap().size(),
                    view.vte().font().unwrap().size()
                );
            }
        }
        classic.set_font(Some(&crate::font::terminal_font_description(desc, &config)));
        classic.set_font_scale(scale);
        refresh(&config);
        settle();
        check(&config);
        assert_eq!(size(&title), size(&entry));
        // These native bookmark labels existed before the font change but
        // stayed hidden through it. Revealing them must resolve the new size.
        for view in [&block, &chat] {
            for finished in view.finished_blocks.borrow().iter() {
                finished.bookmark_star.set_visible(true);
            }
        }
        settle();
        check(&config);
        for view in [&block, &chat] {
            for finished in view.finished_blocks.borrow().iter() {
                assert!(finished.bookmark_star.is_mapped());
                assert_configured_size(&finished.bookmark_star, &config, config.default_font_scale);
                finished.bookmark_star.set_visible(false);
            }
        }
    }
    assert_eq!(
        size(&title),
        original_size,
        "main font edits leave chrome fixed"
    );

    // Chrome-only zoom must not resize any live/history VTE.
    let content_scale = classic.font_scale();
    config.chrome_font_scale = 1.8;
    refresh(&config);
    settle();
    check(&config);
    assert_eq!(classic.font_scale(), content_scale);
    assert_eq!(block.vte().font_scale(), content_scale);
    assert_eq!(chat.vte().font_scale(), content_scale);

    // Rows, tabs, pane headers and finished cards created after zoom need no
    // one-off style patch. A Unified pane uses the same native font contract.
    let late_file = gtk::Label::new(Some("New file"));
    late_file.add_css_class("file-tree-root");
    sidebar.append(&late_file);
    let unified = pane(&config, TerminalMode::Unified);
    notebook.append_page(&unified.widget(), Some(&gtk::Label::new(Some("New tab"))));
    finish_command(&block);
    finish_command(&chat);
    assert_eq!(block.finished_blocks.borrow().len(), 2);
    assert_eq!(chat.finished_blocks.borrow().len(), 2);
    // Remapping older pages must refresh their text too; hidden contexts are
    // not an eager GTK contract, but visible stale labels would be a defect.
    for page in [2, 1, 0, 2] {
        notebook.set_current_page(Some(page));
        settle();
        check(&config);
    }
    assert_eq!(unified.vte().font_scale(), config.default_font_scale);
    assert_eq!(
        unified.vte().font().unwrap().to_string(),
        classic.font().unwrap().to_string()
    );
    assert_eq!(classic.font().unwrap().size(), 17 * gtk::pango::SCALE);
    assert_eq!(classic.font_scale(), 1.25);
    assert_eq!(size(&late_file), size(&title));
    assert!(css::block_css(&config).contains("font-size: 21.25px;"));

    // An enlarged top tab must grow the overlay above its compact minimum,
    // without turning its natural text width into a window minimum.
    config.chrome_font_size = "60px".into();
    config.default_font_scale = 1.0;
    refresh(&config);
    settle();
    let title_height = title.measure(gtk::Orientation::Vertical, -1).0;
    let top_height = top.measure(gtk::Orientation::Vertical, -1).0;
    assert!(
        title_height > 40,
        "fixture must exceed the old fixed bar height"
    );
    assert!(
        top_height >= title_height,
        "the top overlay must measure its text"
    );
    assert!(
        top.height() >= title_height,
        "the allocated top bar must fit its text"
    );
    assert_eq!(size(&title), size(&entry));
    assert!(top.measure(gtk::Orientation::Horizontal, -1).0 < 400);
    assert_eq!(icon.pixel_size(), 16);
    window.close();
    gtk::style_context_remove_provider_for_display(&display, &chrome);
    gtk::style_context_remove_provider_for_display(&display, &base);
}
