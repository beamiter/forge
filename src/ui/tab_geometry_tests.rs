//! Native characterization of production top-tab geometry; no PTY or organism.
//! Generated shape fragments are checked by scripts/check-tab-geometry-shape.py.
use gtk::prelude::*;
use gtk::{Label, ScrolledWindow, ToggleButton};
use gtk4 as gtk;

fn settle() {
    let context = gtk::glib::MainContext::default();
    let until = std::time::Instant::now() + std::time::Duration::from_millis(70);
    while std::time::Instant::now() < until {
        for _ in 0..128 {
            if !context.iteration(false) {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn tab(label_text: String, status: bool, width: i32) -> (ToggleButton, Label, gtk::Image) {
    let strip_label = Label::new(Some(&label_text));
    strip_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    strip_label.set_hexpand(true);
    strip_label.set_xalign(0.0);

    let strip_close_icon = gtk4::Image::from_icon_name("window-close-symbolic");
    strip_close_icon.add_css_class("tab-strip-close");
    strip_close_icon.set_opacity(0.0);

    // Process indicator label
    let process_label = Label::new(None);
    process_label.add_css_class("tab-process-indicator");
    process_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    process_label.set_max_width_chars(15);

    // Pin indicator icon
    let pin_icon = gtk4::Image::new();
    pin_icon.set_icon_name(Some("bookmark-symbolic"));
    pin_icon.add_css_class("tab-pin-icon");
    pin_icon.set_visible(false); // Hidden by default

    // Connection-status dot (remote tabs only): yellow→green→red.
    let conn_dot = Label::new(Some("\u{25CF}"));
    conn_dot.add_css_class("tab-conn-dot");
    if status {
        conn_dot.add_css_class("tab-connecting");
    } else {
        conn_dot.set_visible(false);
    }

    let strip_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    strip_box.append(&conn_dot);
    strip_box.append(&strip_label);
    strip_box.append(&process_label);
    strip_box.append(&pin_icon);
    strip_box.append(&strip_close_icon);

    let strip_btn = ToggleButton::new();
    strip_btn.set_child(Some(&strip_box));

    strip_btn.add_css_class("tab-strip-btn");
    strip_btn.add_css_class("flat");
    strip_btn.set_active(true);
    strip_btn.set_hexpand(false);
    strip_btn.set_width_request(width);
    super::tab_strip::install_tab_reveal_on_map(&strip_btn);
    process_label.set_label("synthetic-process-with-a-long-name");
    process_label.set_visible(status);
    pin_icon.set_visible(status);
    if status {
        strip_btn.add_css_class("tab-pinned");
    }
    let handle = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    handle.add_css_class("tab-resize-handle");
    handle.set_width_request(8);
    handle.set_vexpand(true);
    handle.set_cursor_from_name(Some("col-resize"));
    handle.set_tooltip_text(Some("Drag to resize tabs"));
    handle.update_property(&[gtk4::accessible::Property::Label("Resize tabs")]);
    strip_box.append(&handle);

    (strip_btn, strip_label, strip_close_icon)
}

fn scroller() -> ScrolledWindow {
    let top_tab_scroll = ScrolledWindow::new();
    top_tab_scroll.set_hexpand(true);
    top_tab_scroll.set_vexpand(false);
    top_tab_scroll.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
    top_tab_scroll.set_overflow(gtk4::Overflow::Hidden);
    top_tab_scroll.set_width_request(0);
    top_tab_scroll.set_min_content_width(0);
    top_tab_scroll.set_max_content_width(1);
    top_tab_scroll.set_propagate_natural_width(false);
    top_tab_scroll.add_css_class("top-tab-scroll");
    top_tab_scroll.set_visible(true);
    top_tab_scroll.set_margin_start(164);
    top_tab_scroll.set_margin_end(104);

    top_tab_scroll
}

fn assert_tab_revealed(button: &ToggleButton, scroll: &ScrolledWindow) {
    let bounds = button
        .compute_bounds(scroll)
        .expect("tab in viewport hierarchy");
    let right = bounds.x() + bounds.width();
    let viewport_width = scroll.width() as f32;
    if bounds.width() <= viewport_width {
        assert!(
            bounds.x() >= -1.0 && right <= viewport_width + 1.0,
            "fitting tab must be fully visible: {bounds:?}, viewport={viewport_width}"
        );
    } else {
        assert!(
            bounds.x() < viewport_width && right > 0.0,
            "oversize tab must intersect viewport: {bounds:?}"
        );
    }
}

fn measure(widget: &impl IsA<gtk::Widget>) -> (i32, i32, i32, i32, i32, i32) {
    let (min, natural, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
    let (height, natural_height, _, _) = widget.measure(gtk::Orientation::Vertical, -1);
    (
        min,
        natural,
        widget.width(),
        height,
        natural_height,
        widget.height(),
    )
}

#[test]
#[ignore = "requires isolated hosted GTK display; never run in restricted cloud executor"]
fn production_top_tabs_bound_viewport_minimum_across_font_extremes() {
    gtk::init().expect("GTK display");
    gtk::Settings::default()
        .unwrap()
        .set_property("gtk-xft-dpi", 96 * 1024);
    // Make viewport scroll-to assertions deterministic instead of sampling
    // midway through GTK's adjustment animation.
    gtk::Settings::default()
        .unwrap()
        .set_property("gtk-enable-animations", false);
    let display = gtk::gdk::Display::default().unwrap();
    let base = gtk::CssProvider::new();
    base.load_from_string(&format!("{} {}", super::PANE_HEADER_CSS, r#".tab-strip-btn { transition: none; padding: 4px 8px; border-radius: 4px; border-bottom: 1px solid alpha(currentColor, 0.1); margin-bottom: 2px; }
             .tab-strip-btn:checked { font-weight: bold; border-radius: 4px; background-color: alpha(currentColor, 0.14); outline: 2px solid alpha(currentColor, 0.8); outline-offset: -2px; }
             .tab-strip-close { min-width: 16px; min-height: 16px; padding: 0; margin: 0; }
             .file-tree-drop-hover { background-color: alpha(currentColor, 0.16); border-radius: 4px; outline: 1px dashed alpha(currentColor, 0.45); outline-offset: -1px; }
             .tab-resize-handle { min-width: 8px; margin-left: 2px; border-left: 1px solid alpha(currentColor, 0.24); }
             .tab-resize-handle:hover { border-left-color: currentColor; }
             .sidebar-box { min-width: 140px; padding: 2px 4px; }
             .top-bar { padding: 2px 4px; }
             .hidden-tabs > header { min-height: 0; border: none; background: none; padding: 0; margin: 0; }
             .hidden-tabs > header > * { min-height: 0; min-width: 0; padding: 0; margin: 0; }
             .terminal-box scrollbar slider { min-width: 6px; border-radius: 3px; }
             .terminal-box scrollbar { padding: 0; }
             .tab-activity { font-style: italic; }
             @keyframes bell-flash { 0% { opacity: 1.0; } 50% { opacity: 0.5; } 100% { opacity: 1.0; } }
             .tab-bell-flash { animation: bell-flash 0.3s ease-in-out 2; }
             .tab-pinned { font-weight: bold; }
             .tab-dragging { opacity: 0.5; }
             .tab-drop-target { background-color: alpha(currentColor, 0.15); }
             .top-tabs .tab-drop-before { box-shadow: inset 3px 0 currentColor; }
             .top-tabs .tab-drop-after { box-shadow: inset -3px 0 currentColor; }
             .tab-strip:not(.top-tabs) .tab-drop-before { box-shadow: inset 0 3px currentColor; }
             .tab-strip:not(.top-tabs) .tab-drop-after { box-shadow: inset 0 -3px currentColor; }
             .tab-process-indicator { font-size: 0.8em; opacity: 0.6; margin-left: 4px; }
             .tab-pin-icon { font-size: 0.9em; opacity: 0.8; margin-right: 2px; }
             .tab-selected { background-color: alpha(currentColor, 0.14); outline: 2px solid alpha(currentColor, 0.8); outline-offset: -2px; }
             .tab-conn-dot { font-size: 0.7em; margin-right: 2px; }
             .window-controls { margin-left: 2px; }
             .window-control { min-width: 24px; min-height: 24px; padding: 0; border-radius: 999px; }
             @keyframes conn-pulse { 0% { opacity: 1.0; } 50% { opacity: 0.35; } 100% { opacity: 1.0; } }
             .tab-conn-dot.tab-connecting { animation: conn-pulse 1.2s ease-in-out infinite; }
             .tab-strip-search { padding: 4px 8px; margin: 2px 4px; }
             .top-tabs .tab-strip-btn { border-bottom: none; margin-bottom: 0; margin-right: 2px; }
             .bottom-bar { padding: 0 8px; border-top: 1px solid alpha(currentColor, 0.20); font-size: 0.85em; }
             .file-tree-box { border-top: 1px solid alpha(currentColor, 0.15); }
             .file-tree-header { padding: 2px 4px; }
             .file-tree-root { font-size: 0.85em; opacity: 0.7; }
             .file-tree { padding: 2px; }"#));
    gtk::style_context_add_provider_for_display(
        &display,
        &base,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let font = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(
        &display,
        &font,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
    );
    let window = gtk::Window::new();
    window.add_css_class("forge-window");
    window.add_css_class("reference-shell");
    super::reference_shell::install_css();
    let scroll = scroller();
    super::tab_strip::install_top_tab_geometry_reveal(&scroll);
    let top = super::reference_shell::center_header(&scroll);
    assert_eq!(scroll.parent().as_ref(), Some(top.upcast_ref()));
    assert!(scroll.has_css_class("forge-font-chrome"));
    assert_eq!((scroll.margin_start(), scroll.margin_end()), (0, 0));
    let actions = top.last_child().expect("shared header actions");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&top);
    let sidebar = gtk::ScrolledWindow::new();
    sidebar.add_css_class("forge-font-chrome");
    sidebar.set_hexpand(false);
    sidebar.set_vexpand(true);
    sidebar.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    sidebar.set_visible(false);
    root.append(&sidebar);
    window.set_child(Some(&root));
    window.set_default_size(1200, 900);
    window.present();
    let mut failures = Vec::new();
    let mut measurements = 0;
    let mut icon_requisition = None;
    let mut last_strip = None;
    // Points and absolute pixels both resolve to the explicitly named pixel size at 96 dpi.
    for (description, scale, expected_px) in [
        ("60px", 1.0, 60.0),
        ("60px", 1.8, 108.0),
        ("45", 1.0, 60.0),
        ("81", 1.0, 108.0),
        ("72px", 10.0, 720.0),
        ("12px", 1.0, 12.0),
    ] {
        font.load_from_string(&crate::font::window_font_css(
            "Monospace 12",
            1.0,
            description,
            scale,
        ));
        for width in [
            crate::config::MIN_TAB_WIDTH,
            crate::config::DEFAULT_TAB_WIDTH,
            crate::config::MAX_TAB_WIDTH,
        ] {
            for status in [false, true] {
                for long in [false, true] {
                    let mut single_tab_minimum = [0; 2];
                    for count in [1, 3, 20] {
                        let strip = gtk::Box::new(gtk::Orientation::Horizontal, 2);
                        strip.add_css_class("tab-strip");
                        strip.add_css_class("top-tabs");
                        strip.set_valign(gtk::Align::Center);
                        strip.set_hexpand(true);
                        strip.set_vexpand(false);
                        strip.set_width_request(1);
                        let mut tabs = Vec::new();
                        for index in 0..count {
                            let tab = tab(
                                if long {
                                    "synthetic-title-".repeat(30)
                                } else {
                                    "tab".into()
                                },
                                status,
                                width as i32,
                            );
                            tab.0.set_active(index == 0);
                            strip.append(&tab.0);
                            tabs.push(tab);
                        }
                        scroll.set_child(Some(&strip));
                        for (size_index, requested) in [1200, 360].into_iter().enumerate() {
                            window.set_default_size(requested, 900);
                            settle();
                            let image = measure(&tabs[0].2);
                            let requisition = (image.0, image.1, image.3, image.4);
                            if let Some(expected) = icon_requisition {
                                assert_eq!(
                                    requisition, expected,
                                    "image geometry must not scale with text"
                                );
                            } else {
                                icon_requisition = Some(requisition);
                            }
                            let actual = tabs[0].1.pango_context().font_description().unwrap();
                            let px = f64::from(actual.size()) / f64::from(gtk::pango::SCALE)
                                * if actual.is_size_absolute() {
                                    1.0
                                } else {
                                    96.0 / 72.0
                                };
                            assert!(
                                (px - expected_px).abs() < 0.02,
                                "font expected {expected_px}px, got {actual:?}"
                            );
                            assert_eq!(tabs[0].1.ellipsize(), gtk::pango::EllipsizeMode::End);
                            assert_eq!(scroll.hscrollbar_policy(), gtk::PolicyType::External);
                            let action_bounds = actions
                                .compute_bounds(&top)
                                .expect("actions remain in the measured header");
                            assert!(
                                action_bounds.x() >= 0.0
                                    && action_bounds.x() + action_bounds.width()
                                        <= top.width() as f32 + 1.0,
                                "header actions remain reachable: {action_bounds:?}"
                            );
                            assert!(scroll.width() > 0, "tabs retain a usable viewport");
                            measurements += 1;
                            println!("TAB_GEOMETRY font={description} scale={scale} px={px} width={width} status={status} long={long} count={count} requested={requested} policy={:?} scroll={:?} strip={:?} top={:?} window={:?} title={:?} image={:?}", scroll.hscrollbar_policy(), measure(&scroll), measure(&strip), measure(&top), measure(&window), measure(&tabs[0].1), measure(&tabs[0].2));
                            // Compare like-for-like single/multiple tabs. Fixed tab requests,
                            // icon/handle minima and large font ellipsis are legitimate in the
                            // single-tab baseline; their SUM must not become the window minimum
                            // (the explicit contract in layout::apply_tab_placement).
                            // max-content-width only caps natural size, not GTK minimum size.
                            let viewport_min =
                                measure(&scroll).0 - scroll.margin_start() - scroll.margin_end();
                            if count == 1 {
                                single_tab_minimum[size_index] = viewport_min;
                            } else if viewport_min > single_tab_minimum[size_index] {
                                failures.push(format!("{description}x{scale}/{width}/{status}/{long}/{count}/{requested}: viewport minimum {viewport_min} exceeds single-tab baseline {}", single_tab_minimum[size_index]));
                            }
                            if window.width() != requested {
                                failures.push(format!("{description}x{scale}/{width}/{status}/{long}/{count}: requested width {requested}, allocated {}", window.width()));
                            }
                            println!("TAB_GEOMETRY_RESIZE requested={requested} allocated={} honored={} single_tab_min={} viewport_min={viewport_min}", window.width(), window.width() == requested, single_tab_minimum[size_index]);
                            if tabs[0].1.height()
                                < tabs[0].1.measure(gtk::Orientation::Vertical, -1).0
                            {
                                failures.push(format!("{description}x{scale}/{width}/{count}: title vertically clipped"));
                            }
                        }
                        // External must retain a usable scroll range, not merely clip tabs.
                        // This exercises GTK adjustment/viewport allocation, not physical input.
                        let adjustment = scroll.hadjustment();
                        let end = adjustment.upper() - adjustment.page_size();
                        if strip.measure(gtk::Orientation::Horizontal, -1).0 > scroll.width() {
                            assert!(
                                end > adjustment.lower(),
                                "overflow must expose horizontal scroll range"
                            );
                            super::tab_strip::sync_strip_buttons_active(&strip, (count - 1) as u32);
                            settle();
                            if count > 1 {
                                assert!(
                                    adjustment.value() > adjustment.lower(),
                                    "selecting trailing tab must scroll"
                                );
                            }
                            let bounds = tabs[count - 1]
                                .0
                                .compute_bounds(&scroll)
                                .expect("tab in scroll hierarchy");
                            assert_tab_revealed(&tabs[count - 1].0, &scroll);
                            println!("TAB_GEOMETRY_OVERFLOW count={count} value={} upper={} page={} last={bounds:?}", adjustment.value(), adjustment.upper(), adjustment.page_size());
                            super::tab_strip::sync_strip_buttons_active(&strip, 0);
                            settle();
                            assert_tab_revealed(&tabs[0].0, &scroll);
                        }
                        // Exercise hidden/remapped rows and placement transfer, without PTYs.
                        tabs[count - 1].0.set_visible(false);
                        settle();
                        tabs[count - 1].0.set_visible(true);
                        scroll.set_child(None::<&gtk::Widget>);
                        strip.set_orientation(gtk::Orientation::Vertical);
                        strip.set_valign(gtk::Align::Start);
                        strip.set_hexpand(false);
                        strip.set_vexpand(true);
                        strip.set_width_request(-1);
                        strip.remove_css_class("top-tabs");
                        for (button, _, _) in &tabs {
                            button.set_hexpand(true);
                            button.set_width_request(-1);
                            button
                                .child()
                                .unwrap()
                                .last_child()
                                .unwrap()
                                .set_visible(false);
                        }
                        sidebar.set_child(Some(&strip));
                        sidebar.set_visible(true);
                        settle();
                        sidebar.set_child(None::<&gtk::Widget>);
                        sidebar.set_visible(false);
                        strip.set_orientation(gtk::Orientation::Horizontal);
                        strip.set_valign(gtk::Align::Center);
                        strip.set_hexpand(true);
                        strip.set_vexpand(false);
                        strip.set_width_request(1);
                        strip.add_css_class("top-tabs");
                        for (button, _, _) in &tabs {
                            button.set_hexpand(false);
                            button.set_width_request(width as i32);
                            button
                                .child()
                                .unwrap()
                                .last_child()
                                .unwrap()
                                .set_visible(true);
                        }
                        scroll.set_child(Some(&strip));
                        last_strip = Some(strip.clone());
                        settle();
                        // Exercise authoritative selection, not a fixture-only toggle.
                        super::tab_strip::sync_strip_buttons_active(&strip, (count - 1) as u32);
                        settle();
                        assert_tab_revealed(&tabs[count - 1].0, &scroll);
                        assert!(tabs[count - 1].0.is_active());
                        println!("TAB_GEOMETRY_REMAP font={description} count={count} scroll={:?} strip={:?}", measure(&scroll), measure(&strip));
                    }
                }
            }
        }
    }
    assert_eq!(measurements, 432, "all matrix measurements must complete");
    println!("TAB_GEOMETRY_MATRIX_COMPLETE measurements={measurements}");
    let live_strip = last_strip.unwrap();
    super::tab_strip::sync_strip_buttons_active(&live_strip, 19);
    settle();
    let trailing = live_strip
        .last_child()
        .unwrap()
        .downcast::<ToggleButton>()
        .unwrap();
    assert_tab_revealed(&trailing, &scroll);
    assert!(scroll.hadjustment().value() > scroll.hadjustment().lower());
    // A value-only user scroll must not be overridden by the geometry hook.
    let adjustment = scroll.hadjustment();
    adjustment.set_value(adjustment.lower());
    settle();
    assert_eq!(
        adjustment.value(),
        adjustment.lower(),
        "manual scrolling must remain in place"
    );
    super::tab_strip::sync_strip_buttons_active(&live_strip, 19);
    settle();
    for (description, scale, expected_px) in [
        ("60px", 1.0, 60.0),
        ("60px", 1.8, 108.0),
        ("72px", 10.0, 720.0),
        ("12px", 1.0, 12.0),
    ] {
        font.load_from_string(&crate::font::window_font_css(
            "Monospace 12",
            1.0,
            description,
            scale,
        ));
        settle();
        // Find the title after the decorative connection dot in the retained strip.
        let button = live_strip
            .first_child()
            .unwrap()
            .downcast::<ToggleButton>()
            .unwrap();
        let title = button
            .child()
            .unwrap()
            .first_child()
            .unwrap()
            .next_sibling()
            .unwrap()
            .downcast::<Label>()
            .unwrap();
        let actual = title.pango_context().font_description().unwrap();
        let px = f64::from(actual.size()) / f64::from(gtk::pango::SCALE)
            * if actual.is_size_absolute() {
                1.0
            } else {
                96.0 / 72.0
            };
        assert!(
            (px - expected_px).abs() < 0.02,
            "live font change {description} x {scale}: {actual:?}"
        );
        let active = live_strip
            .last_child()
            .unwrap()
            .downcast::<ToggleButton>()
            .unwrap();
        assert_tab_revealed(&active, &scroll);
        println!("TAB_GEOMETRY_LIVE_FONT font={description} scale={scale} scroll={:?} strip={:?} title={:?}", measure(&scroll), measure(&live_strip), measure(&title));
    }
    // Same widget width requests as preview_tab_width: let the production
    // adjustment notification reveal selection, without calling sync here.
    for width in [80, 480] {
        let mut child = live_strip.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            widget.set_width_request(width);
        }
        settle();
        let active = live_strip
            .last_child()
            .unwrap()
            .downcast::<ToggleButton>()
            .unwrap();
        assert_tab_revealed(&active, &scroll);
        println!(
            "TAB_GEOMETRY_LIVE_WIDTH width={width} scroll={:?} strip={:?}",
            measure(&scroll),
            measure(&live_strip)
        );
    }
    for width in [1200, 360] {
        window.set_default_size(width, 900);
        settle();
        assert_eq!(window.width(), width);
        let active = live_strip
            .last_child()
            .unwrap()
            .downcast::<ToggleButton>()
            .unwrap();
        assert_tab_revealed(&active, &scroll);
    }
    window.close();
    gtk::style_context_remove_provider_for_display(&display, &font);
    gtk::style_context_remove_provider_for_display(&display, &base);
    assert!(
        failures.is_empty(),
        "{} geometry violations: {failures:#?}",
        failures.len()
    );
}
