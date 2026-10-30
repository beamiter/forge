//! Exercise production divider bindings with real X11 pointer drags.
use super::*;
use libadwaita::{self as adw, prelude::*};
use std::process::Command;
use std::time::{Duration, Instant};

fn settle() {
    let until = Instant::now() + Duration::from_millis(120);
    while Instant::now() < until {
        while gtk::glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn x11(arguments: &[String]) -> String {
    let mut child = Command::new("xdotool")
        .args(arguments)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("xdotool");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        while gtk::glib::MainContext::default().iteration(false) {}
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = Command::new("xdotool").args(["mouseup", "1"]).status();
            panic!("native pointer command timed out: {arguments:?}");
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{arguments:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
#[ignore = "requires isolated X11 DISPLAY and xdotool"]
fn native_sidebar_drags_preserve_requests_and_input_focus() {
    adw::init().expect("GTK/Adwaita display");
    crate::ui::reference_shell::install_css();
    let config = Rc::new(RefCell::new(Config::safe_defaults()));
    config.borrow_mut().sidebar_width = 237;
    config.borrow_mut().ai_panel_width = 306;
    config.borrow_mut().sidebar_visible = true;
    let navigation = gtk::Box::new(gtk::Orientation::Vertical, 0);
    navigation.add_css_class("reference-navigation");
    navigation.set_width_request(NAV_MIN);
    navigation.append(&gtk::Label::new(Some("Navigation")));
    let inspector = gtk::Box::new(gtk::Orientation::Vertical, 0);
    inspector.add_css_class("reference-inspector");
    inspector.set_width_request(INSPECTOR_MIN);
    inspector.append(&gtk::Label::new(Some("Inspector")));
    let input = gtk::Entry::new();
    let draft = "printf '保留 draft — no submission'";
    input.set_text(draft);
    let center = gtk::Box::new(gtk::Orientation::Vertical, 0);
    center.set_hexpand(true);
    center.append(&input);
    let right = gtk::Paned::new(gtk::Orientation::Horizontal);
    right.add_css_class("reference-layout-paned");
    right.set_start_child(Some(&center));
    right.set_end_child(Some(&inspector));
    right.set_resize_start_child(true);
    right.set_resize_end_child(false);
    right.set_shrink_start_child(true);
    right.set_shrink_end_child(false);
    let left = gtk::Paned::new(gtk::Orientation::Horizontal);
    left.add_css_class("reference-layout-paned");
    left.set_start_child(Some(&navigation));
    left.set_end_child(Some(&right));
    left.set_resize_start_child(false);
    left.set_resize_end_child(true);
    left.set_shrink_start_child(false);
    left.set_shrink_end_child(true);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let rail = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rail.set_width_request(token::RAIL_WIDTH as i32);
    body.append(&rail);
    left.set_hexpand(true);
    body.append(&left);
    let observer = gtk::DrawingArea::new();
    observer.set_can_target(false);
    let frame = gtk::Overlay::new();
    frame.set_child(Some(&body));
    frame.add_overlay(&observer);
    frame.set_measure_overlay(&observer, false);
    // Use the same CSD native window family as the app. A plain GtkWindow has
    // no surface inset and would miss surface-to-widget hit-testing mistakes.
    let window = adw::Window::new();
    window.add_css_class("forge-window");
    window.add_css_class("reference-shell");
    window.set_default_size(1356, 704);
    window.set_title(Some(&format!(
        "forge-divider-regression-{}",
        std::process::id()
    )));
    window.set_content(Some(&frame));
    let slot = Rc::new(RefCell::new(std::rc::Weak::<SidebarResize>::new()));
    let drawer = Rc::new(Cell::new(false));
    let drawer_grip = Rc::new(gtk::glib::WeakRef::<gtk::DrawingArea>::new());
    let reflow = {
        let slot = slot.clone();
        let drawer = drawer.clone();
        let drawer_grip = drawer_grip.clone();
        let frame = frame.downgrade();
        let navigation = navigation.downgrade();
        let inspector = inspector.downgrade();
        let config = config.clone();
        Rc::new(move || {
            let (Some(sizing), Some(frame), Some(navigation), Some(inspector)) = (
                slot.borrow().upgrade(),
                frame.upgrade(),
                navigation.upgrade(),
                inspector.upgrade(),
            ) else {
                return;
            };
            let widths = sizing.preferences();
            let layout = token::Layout::for_size_with_sidebars(
                frame.width() as f32,
                frame.height() as f32,
                if config.borrow().sidebar_visible {
                    widths.0 as f32
                } else {
                    0.0
                },
                widths.1 as f32,
            );
            navigation.set_visible(layout.navigation.is_some());
            if drawer.get() {
                inspector.set_visible(true);
                super::super::reference_shell::apply_inspector_geometry(
                    &frame,
                    &inspector,
                    drawer_grip.upgrade().as_ref(),
                    true,
                    (widths.1 as i32).min(frame.width() - 32),
                    false,
                );
                sizing.apply(layout.navigation.map(|r| r.width as i32), None);
                return;
            }
            inspector.set_visible(layout.inspector.is_some());
            sizing.apply(
                layout.navigation.map(|r| r.width as i32),
                layout.inspector.map(|r| r.width as i32),
            );
        })
    };
    let saves = Rc::new(Cell::new(0));
    let changed = saves.clone();
    let update = reflow.clone();
    let sizing = SidebarResize::new(
        &left,
        &right,
        config.clone(),
        move || changed.set(changed.get() + 1),
        move || update(),
    );
    *slot.borrow_mut() = Rc::downgrade(&sizing);
    let update = reflow.clone();
    observer.connect_resize(move |_, _, _| update());
    window.present();
    settle();
    reflow();
    settle();
    let (surface_x, surface_y) = window.surface_transform();
    assert!(
        surface_x > 0.0 && surface_y > 0.0,
        "native regression must exercise real CSD surface insets"
    );
    let title = format!("^forge-divider-regression-{}$", std::process::id());
    let windows = x11(&[
        "search".into(),
        "--onlyvisible".into(),
        "--name".into(),
        title,
    ]);
    let ids: Vec<_> = windows.lines().collect();
    assert_eq!(ids.len(), 1);
    let id = ids[0].to_string();
    x11(&["windowfocus".into(), "--sync".into(), id.clone()]);
    settle();
    let owns_input = || {
        gtk::prelude::RootExt::focus(&window).is_some_and(|focused| {
            focused == *input.upcast_ref::<gtk::Widget>() || focused.is_ancestor(&input)
        })
    };
    let focus_input = || {
        assert_eq!(input.text().as_str(), draft);
        assert!(input.grab_focus());
        settle();
        assert!(owns_input(), "input must own focus before pointer resizing");
    };
    focus_input();
    let drag_widget = |handle: &gtk::Widget, offset: i32| {
        let owned_before = owns_input();
        assert!(owned_before, "pointer gesture requires focused input");
        let bounds = handle.compute_bounds(&window).unwrap();
        assert!(bounds.width() >= token::SIDEBAR_DIVIDER_WIDTH);
        let (surface_x, surface_y) = window.surface_transform();
        let x = (f64::from(bounds.x() + bounds.width() / 2.0) + surface_x).round() as i32;
        let y = (f64::from(bounds.y() + bounds.height() / 2.0) + surface_y).round() as i32;
        x11(&[
            "mousemove".into(),
            "--window".into(),
            id.clone(),
            x.to_string(),
            y.to_string(),
            "mousedown".into(),
            "1".into(),
            "sleep".into(),
            "0.08".into(),
            "mousemove".into(),
            "--window".into(),
            id.clone(),
            (x + offset).to_string(),
            y.to_string(),
            "sleep".into(),
            "0.08".into(),
            "mouseup".into(),
            "1".into(),
        ]);
        settle();
        assert!(
            owns_input(),
            "pointer resizing must retain input/IME focus: offset={offset}, window_width={}, handle={:?}, owned_before={owned_before}, active={}, focused={:?}",
            window.width(),
            bounds,
            window.is_active(),
            gtk::prelude::RootExt::focus(&window).map(|widget| (
                widget.type_().name(),
                widget.css_name(),
                widget.widget_name(),
                widget.css_classes(),
            )),
        );
        assert_eq!(input.text().as_str(), draft);
    };
    let drag = |paned: &gtk::Paned, offset: i32| {
        drag_widget(&handle(paned).expect("real GtkPaned handle"), offset)
    };
    drag(&left, 60);
    assert!((config.borrow().sidebar_width as i32 - 297).abs() <= 2);
    drag(&right, -40);
    assert!((config.borrow().ai_panel_width as i32 - 346).abs() <= 2);
    let saved = (
        config.borrow().sidebar_width,
        config.borrow().ai_panel_width,
    );
    assert_eq!(saves.get(), 2);
    window.set_default_size(700, 600);
    settle();
    assert!(!inspector.is_visible());
    assert!(
        left.position() < saved.0 as i32,
        "narrow layout must exercise a real clamp"
    );
    // GtkPaned's automatic child collapse can select its keyboard handle.
    // Establish the input owner after that separate layout operation, then
    // verify that the actual no-motion pointer gesture preserves it.
    focus_input();
    drag(&left, 0);
    assert_eq!(
        (
            config.borrow().sidebar_width,
            config.borrow().ai_panel_width
        ),
        saved,
        "no-motion press on a temporary clamp must preserve the request"
    );
    config.borrow_mut().sidebar_visible = false;
    reflow();
    settle();
    config.borrow_mut().sidebar_visible = true;
    window.set_default_size(1600, 704);
    settle();
    focus_input();
    assert_eq!(left.position(), saved.0 as i32);
    assert_eq!(
        inspector.compute_bounds(&right).unwrap().width(),
        saved.1 as f32
    );
    assert_eq!(
        saves.get(),
        2,
        "allocation/hide/show cannot become a persisted drag"
    );
    assert!(owns_input());
    assert_eq!(input.text().as_str(), draft);
    // Reparent the same inspector into a non-measuring overlay, as the real
    // narrow-window drawer does. Its native allocation and hit target must
    // follow a width request made after it leaves the Paned.
    window.set_default_size(1000, 704);
    settle();
    right.set_end_child(None::<&gtk::Widget>);
    inspector.set_halign(gtk::Align::End);
    inspector.add_css_class("reference-inspector-drawer");
    frame.add_overlay(&inspector);
    frame.set_measure_overlay(&inspector, false);
    let grip = sizing.drawer_grip(&inspector);
    drawer_grip.set(Some(&grip));
    frame.add_overlay(&grip);
    frame.set_measure_overlay(&grip, false);
    drawer.set(true);
    reflow();
    settle();
    let assert_drawer_edge = || {
        let panel = inspector.compute_bounds(&frame).unwrap();
        let edge = grip.compute_bounds(&frame).unwrap();
        assert_eq!(panel.width(), config.borrow().ai_panel_width as f32);
        assert!((panel.x() - edge.x()).abs() <= 1.0);
        assert!((panel.x() + panel.width() - frame.width() as f32).abs() <= 1.0);
    };
    assert_drawer_edge();
    focus_input();
    drag_widget(grip.upcast_ref(), -50);
    assert!((config.borrow().ai_panel_width as i32 - saved.1 as i32 - 50).abs() <= 2);
    assert_drawer_edge();
    assert_eq!(saves.get(), 3);
    assert!(owns_input());
    assert_eq!(input.text().as_str(), draft);
    window.close();
}
