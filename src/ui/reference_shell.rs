//! Native bindings for the shared reference workspace contract.
//!
//! This module only arranges existing GTK surfaces. The notebook, terminal,
//! shell editor, history and task reducers keep their existing ownership.
use super::UiState;
use crate::config::{SidebarView, TabPlacement, TerminalMode};
use gtk4::prelude::*;
use gtk4::{glib, Orientation};
use jterm_core::developer_workspace as token;
use std::rc::Rc;
use vte4::TerminalExt;

const INSPECTOR_CHROME_KEY: &str = "forge-reference-inspector-chrome";
const OBSERVED_INSPECTOR_KEY: &str = "forge-observed-inspector-model";
const REVIEW_TASK_KEY: &str = "forge-reference-review-task";
const COMPOSER_CONTROL_HEIGHT: f64 = 22.0;

fn composer_control_height(scale: f64) -> i32 {
    COMPOSER_CONTROL_HEIGHT.max(14.0 * scale + 6.0).ceil() as i32
}

#[derive(PartialEq, Eq)]
struct ObservedInspector {
    task: Option<crate::agent_task::TaskId>,
    model: jterm_core::reference_conversation::Inspector,
}

/// Window-owned visibility intent, independent of the optional AI/Tasks pages.
/// The same inspector widget moves into a narrow-window drawer; its selected
/// page, scroll position and domain objects therefore survive every toggle.
struct InspectorChrome {
    requested: std::cell::Cell<Option<bool>>,
    actual: std::cell::Cell<bool>,
    button: glib::WeakRef<gtk4::ToggleButton>,
    frame: glib::WeakRef<gtk4::Overlay>,
    sizing: Rc<super::sidebar_resize::SidebarResize>,
    drawer_grip: glib::WeakRef<gtk4::DrawingArea>,
}

impl InspectorChrome {
    fn sync(&self, ui: &Rc<UiState>, width: i32, height: i32) {
        let Some(frame) = self.frame.upgrade() else {
            return;
        };
        let (navigation_width, inspector_width) = self.sizing.preferences();
        let layout = token::Layout::for_size_with_sidebars(
            width as f32,
            height as f32 + token::TITLEBAR_HEIGHT,
            if ui.config.borrow().sidebar_visible {
                navigation_width as f32
            } else {
                0.0
            },
            if self.requested.get() == Some(false) {
                0.0
            } else {
                inspector_width as f32
            },
        );
        ui.sidebar.set_visible(layout.navigation.is_some());
        let inline = layout.inspector.is_some();
        let usable = width >= 360 && height >= 120;
        let inspector = &ui.reference_inspector;
        let focus = gtk4::prelude::RootExt::focus(&ui.window);
        let focused_inside = focus.as_ref().is_some_and(|focus| {
            focus == inspector.upcast_ref::<gtk4::Widget>() || focus.is_ancestor(inspector)
        });
        if focused_inside && !inline && self.requested.get().is_none() {
            // A responsive resize must not hide the user's focused editor.
            self.requested.set(Some(true));
        }
        let visible = usable && self.requested.get().unwrap_or(inline);
        let drawer = visible && !inline;
        let in_drawer = inspector.parent().as_ref() == Some(frame.upcast_ref());
        if drawer != in_drawer {
            if in_drawer {
                frame.remove_overlay(inspector);
            } else if ui.ai_paned.end_child().as_ref() == Some(inspector.upcast_ref()) {
                ui.ai_paned.set_end_child(None::<&gtk4::Widget>);
            }
            if drawer {
                inspector.set_halign(gtk4::Align::End);
                inspector.set_valign(gtk4::Align::Fill);
                frame.add_overlay(inspector);
                frame.set_measure_overlay(inspector, false);
                inspector.add_css_class("reference-inspector-drawer");
            } else {
                inspector.set_halign(gtk4::Align::Fill);
                inspector.remove_css_class("reference-inspector-drawer");
                ui.ai_paned.set_end_child(Some(inspector));
            }
        }
        let drawer_width = (inspector_width as i32)
            .clamp(
                token::MIN_INSPECTOR_WIDTH as i32,
                token::MAX_INSPECTOR_WIDTH as i32,
            )
            .min((width - 32).max(0));
        if let Some(grip) = self
            .drawer_grip
            .upgrade()
            .filter(|_| drawer && drawer != in_drawer)
        {
            // Put the six-pixel grip above the drawer, never above any
            // other portion of the terminal canvas.
            frame.remove_overlay(&grip);
            frame.add_overlay(&grip);
            frame.set_measure_overlay(&grip, false);
        }
        inspector.set_visible(visible);
        apply_inspector_geometry(
            &frame,
            inspector,
            self.drawer_grip.upgrade().as_ref(),
            drawer,
            drawer_width,
            drawer != in_drawer,
        );
        if drawer != in_drawer && visible && focused_inside {
            if let Some(focus) = &focus {
                // Reparenting can synchronously assign GTK's scroll container
                // as a fallback owner. Finish this same-window transfer before
                // arming the existing guard; later user focus choices still win.
                if focus.is_mapped() && focus.root().as_ref() == Some(ui.window.upcast_ref()) {
                    focus.grab_focus();
                    crate::block_view::restore_live_focus_deferred(focus);
                }
            }
        }
        self.sizing.apply(
            layout.navigation.map(|rect| rect.width.round() as i32),
            layout
                .inspector
                .filter(|_| visible)
                .map(|rect| rect.width.round() as i32),
        );
        self.actual.set(visible);
        if let Some(button) = self.button.upgrade() {
            let title = if visible {
                "Hide right sidebar"
            } else {
                "Show right sidebar"
            };
            button.set_active(visible);
            button.set_sensitive(usable);
            button.set_tooltip_text(Some(if usable {
                title
            } else {
                "Right sidebar needs a larger window"
            }));
            button.update_property(&[gtk4::accessible::Property::Label(title)]);
            button.update_state(&[gtk4::accessible::State::Expanded(Some(visible))]);
        }
        if focused_inside && !visible {
            ui.focus_current_terminal();
        }
    }
}

/// Non-measuring overlays do not propagate their children's resize requests.
/// Queue the host allocation when drawer geometry changes so its visible edge
/// and the native drag target both use the requested width in the same frame.
pub(super) fn apply_inspector_geometry(
    frame: &gtk4::Overlay,
    inspector: &gtk4::Box,
    grip: Option<&gtk4::DrawingArea>,
    drawer: bool,
    drawer_width: i32,
    reparented: bool,
) {
    let mut changed = reparented;
    let width = if drawer {
        drawer_width
    } else {
        token::MIN_INSPECTOR_WIDTH as i32
    };
    if inspector.width_request() != width {
        inspector.set_width_request(width);
        changed = true;
    }
    if let Some(grip) = grip {
        let margin = (drawer_width - token::SIDEBAR_DIVIDER_WIDTH as i32).max(0);
        if grip.margin_end() != margin {
            grip.set_margin_end(margin);
            changed = true;
        }
        if grip.is_visible() != drawer {
            grip.set_visible(drawer);
            changed = true;
        }
    }
    if changed {
        frame.queue_allocate();
    }
}

/// Optional AI/Tasks content shares the inspector slot, but its legacy width
/// preference must never become a second writer of the common shell geometry.
/// Only an explicit user Open action requests visibility; background syncing
/// leaves a deliberately hidden inspector hidden.
pub(crate) fn sync_inspector(ui: &UiState, reveal: bool) -> bool {
    if !ui.window.has_css_class("reference-shell") {
        return false;
    }
    let controller = unsafe {
        ui.window
            .data::<std::rc::Weak<InspectorChrome>>(INSPECTOR_CHROME_KEY)
            .and_then(|ptr| ptr.as_ref().upgrade())
    };
    if let Some(controller) = controller {
        if let (Some(frame), Some(ui)) = (
            controller.frame.upgrade(),
            ui_for_widget(ui.window.upcast_ref()),
        ) {
            if reveal {
                controller.requested.set(Some(true));
            }
            controller.sync(&ui, frame.width(), frame.height());
        }
    }
    true
}

pub(crate) fn finish_sidebar_resize(ui: &UiState) {
    let controller = unsafe {
        ui.window
            .data::<std::rc::Weak<InspectorChrome>>(INSPECTOR_CHROME_KEY)
            .and_then(|ptr| ptr.as_ref().upgrade())
    };
    if let Some(controller) = controller {
        controller.sizing.finish(true);
    }
}

fn active_mode(ui: &UiState) -> Option<TerminalMode> {
    ui.current_pane_leaf().map(|leaf| match leaf {
        super::PaneLeaf::Block(view) => view.terminal_mode(),
        super::PaneLeaf::Vte(_) => TerminalMode::Vte,
    })
}

fn native_grid_input(ui: &UiState) -> bool {
    ui.current_pane_leaf().is_some_and(|leaf| match leaf {
        super::PaneLeaf::Block(view) => !view.has_input_dock(),
        super::PaneLeaf::Vte(_) => true,
    })
}

fn product_identity(product: &str, size: i32) -> gtk4::Image {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<String, gtk4::gdk::Texture>> = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let texture = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(texture) = cache.get(product) {
            return Some(texture.clone());
        }
        let bytes = jterm_core::reference_identity::png(product)?;
        let texture = gtk4::gdk::Texture::from_bytes(&glib::Bytes::from_static(bytes)).ok()?;
        cache.insert(product.to_string(), texture.clone());
        Some(texture)
    });
    let image = gtk4::Image::from_paintable(texture.as_ref());
    image.set_pixel_size(size);
    image.set_valign(gtk4::Align::Center);
    image
}

fn label(value: &str, class: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(value));
    label.set_xalign(0.0);
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    label.add_css_class(class);
    label.set_tooltip_text(Some(value));
    label
}

/// Keep the reference hierarchy at zoom 1, scaling only its authored text sizes.
/// The surface provider intentionally outranks the general application theme;
/// these role declarations must share that authority without lifting unrelated
/// dynamic colors or changing every role to one global size.
pub(crate) fn role_font_css(chrome_scale: f64, content_scale: f64) -> String {
    let valid_scale = |scale: f64| {
        if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        }
    };
    let chrome_scale = valid_scale(chrome_scale);
    let content_scale = valid_scale(content_scale);
    let mut css = String::new();
    for rule in include_str!("reference_shell.css")
        .split('}')
        .chain(include_str!("developer_block.css").split('}'))
        .chain(include_str!("reference_popovers.css").split('}'))
        .chain(include_str!("reference_dialogs.css").split('}'))
    {
        let Some((selectors, body)) = rule.split_once('{') else {
            continue;
        };
        let size = body.split(';').find_map(|declaration| {
            let (property, value) = declaration.split_once(':')?;
            (property.trim() == "font-size")
                .then(|| value.trim().strip_suffix("px")?.trim().parse::<f64>().ok())
                .flatten()
        });
        let Some(size) = size else { continue };
        for selector in selectors.split(',') {
            let rich_content = [
                ".reference-rich-conversation",
                ".reference-role-header",
                ".reference-rich-terminal",
            ]
            .iter()
            .any(|scope| selector.contains(scope));
            let native_input = [
                ".term-chat-root",
                ".chat-controls",
                ".reference-block-composer",
                ".reference-composer-controls",
                ".reference-unavailable-tool",
                ".reference-model-selector",
            ]
            .iter()
            .any(|scope| selector.contains(scope));
            // The native editor keeps its existing font authority. Footer
            // labels are GUI chrome; semantic messages alone follow Content.
            let composer_chrome = selector.contains(".reference-composer-controls")
                || selector.contains(".reference-unavailable-tool")
                || selector.contains(".reference-model-selector");
            if native_input && !rich_content && !composer_chrome {
                continue;
            }
            let scale = if rich_content {
                content_scale
            } else {
                chrome_scale
            };
            css.push_str(&format!(
                "{} {{ font-size: {:.6}px; }}\n",
                selector.trim(),
                size * scale
            ));
        }
    }
    css
}

/// Test-only after-paint evidence reads actual native labels, not config values.
fn reference_role_fonts(ui: &UiState) -> serde_json::Value {
    let mut roles = serde_json::Map::new();
    let mut add = |name: &str, widget: Option<gtk4::Widget>| {
        if let Some(label) = widget.and_then(|widget| widget.downcast::<gtk4::Label>().ok()) {
            let font = label.pango_context().font_description();
            let (ink, logical) = label.layout().pixel_extents();
            roles.insert(name.to_string(), serde_json::json!({
                "text": label.text().to_string(),
                "color":label.color().to_string(),
                "classes":label.css_classes().iter().map(|value|value.as_str()).collect::<Vec<_>>(),
                "font_description":font.as_ref().map(ToString::to_string),
                "font_size_pango_units":font.as_ref().map(|font|font.size()),
                "font_size_absolute":font.as_ref().map(|font|font.is_size_absolute()),
                "ink_xywh":[ink.x(),ink.y(),ink.width(),ink.height()],
                "logical_xywh":[logical.x(),logical.y(),logical.width(),logical.height()],
                "bounds":label.compute_bounds(&ui.window).map(|r|serde_json::json!([r.x(),r.y(),r.width(),r.height()])),
            }));
        }
    };
    for (name, class) in [
        ("canvas_title", "reference-canvas-title"),
        ("canvas_subtitle", "reference-canvas-subtitle"),
        ("agent_title", "reference-card-title"),
        ("agent_badge", "reference-neutral-badge"),
        ("metadata_label", "reference-inspector-metadata-label"),
        ("metadata_value", "reference-inspector-metadata-value"),
        ("toolbar_product", "reference-product"),
        ("content_heading", "reference-message-heading"),
        ("content_paragraph", "reference-message-paragraph"),
        ("content_code", "reference-code-line"),
    ] {
        add(name, widget_with_class(ui.window.upcast_ref(), class));
    }
    if let Some(content) = widget_with_class(ui.window.upcast_ref(), "reference-rich-conversation")
    {
        for (name, class) in [
            ("content_author", "reference-message-author"),
            ("content_timing", "reference-message-timing"),
            ("content_metadata", "reference-chip-text"),
            ("content_task", "reference-step-title"),
            ("content_task_elapsed", "reference-step-elapsed"),
            ("content_diff_path", "reference-code-path"),
            ("content_gutter", "reference-line-number"),
        ] {
            add(name, widget_with_class(&content, class));
        }
        if let Some(assistant) = widget_with_class(&content, "reference-assistant-message") {
            add(
                "content_assistant_body",
                widget_with_class(&assistant, "reference-message-paragraph"),
            );
        }
    }
    let copy = widget_with_class(ui.window.upcast_ref(), "reference-rich-diff")
        .and_then(|diff| widget_with_class(&diff, "reference-code-action"))
        .and_then(|button| button.compute_bounds(&ui.window))
        .map(|r| serde_json::json!([r.x(), r.y(), r.width(), r.height()]));
    let config = ui.config.borrow();
    serde_json::json!({"roles":roles,"copy_action_xywh":copy,"chrome_size":config.chrome_font_size,"chrome_scale":config.chrome_font_scale,"chrome_role_multiplier":crate::font::reference_ui_role_multiplier(&config.chrome_font_size,config.chrome_font_scale,&ui.window.pango_context()),"content_desc":config.font_desc,"content_scale":ui.font_scale.get(),"content_role_multiplier":crate::font::reference_content_role_multiplier(&config.font_desc,ui.font_scale.get(),&ui.window.pango_context())})
}

/// GTK4 renders a native widget snapshot after adding this API translation;
/// event dispatch subtracts it. Keep the raw value and positive client origin
/// so opt-in evidence can normalize a measured native surface without guessing.
fn native_surface_geometry(ui: &UiState) -> serde_json::Value {
    let (x, y) = ui.window.surface_transform();
    serde_json::json!({
        "raw_native_surface_transform":[x,y],
        "snapshot_translation":[x,y],
        "widget_origin_in_surface":[x,y],
        "widget_logical":{"width":ui.window.width(),"height":ui.window.height()},
        "surface_logical":ui.window.surface().map(|surface|serde_json::json!({
            "width":surface.width(),"height":surface.height(),"scale_factor":surface.scale_factor()
        })),
        "frame_time":ui.window.frame_clock().map(|clock|clock.frame_time()),
    })
}

/// Shared vector paths, painted by Cairo rather than font-dependent glyphs.
pub(crate) fn vector_icon(icon: jterm_core::reference_icons::Icon, size: i32) -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::new();
    area.set_content_width(size);
    area.set_content_height(size);
    area.set_valign(gtk4::Align::Center);
    area.set_halign(gtk4::Align::Center);
    area.set_can_target(false);
    area.set_draw_func(move |area, cr, width, height| {
        let _ = cr.save();
        cr.scale(f64::from(width) / 24.0, f64::from(height) / 24.0);
        let color = area.color();
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );
        cr.set_line_width(1.5);
        cr.set_line_cap(gtk4::cairo::LineCap::Round);
        cr.set_line_join(gtk4::cairo::LineJoin::Round);
        for path in icon.paths() {
            for (index, (x, y)) in path.iter().enumerate() {
                if index == 0 {
                    cr.move_to((*x).into(), (*y).into());
                } else {
                    cr.line_to((*x).into(), (*y).into());
                }
            }
            let _ = cr.stroke();
        }
        let _ = cr.restore();
    });
    area
}

pub(crate) fn icon_label(key: &str, value: &str) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 6);
    row.append(&vector_icon(
        jterm_core::reference_icons::Icon::from_key(key),
        16,
    ));
    if !value.is_empty() {
        row.append(&label(value, "reference-icon-label"));
    }
    row
}

fn composer_icon_label(key: &str, value: &str) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 4);
    row.add_css_class("reference-composer-icon-label");
    let glyph = vector_icon(jterm_core::reference_icons::Icon::from_key(key), 12);
    glyph.add_css_class("reference-composer-glyph");
    row.append(&glyph);
    if !value.is_empty() {
        let caption = label(value, "reference-icon-label");
        caption.set_width_chars(0);
        caption.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        caption.set_hexpand(true);
        row.append(&caption);
    }
    row
}

/// Keep real disabled semantics while drawing a readable muted label outside
/// the theme's insensitive alpha. The overlay never receives input itself.
pub(crate) fn unavailable_tool(key: &str, title: &str) -> gtk4::Overlay {
    let root = gtk4::Overlay::new();
    root.add_css_class("reference-unavailable-tool");
    root.set_tooltip_text(Some("Unavailable — this composer tool is not connected"));
    let button = gtk4::Button::new();
    let measure = composer_icon_label(key, title);
    measure.set_opacity(0.0);
    button.set_child(Some(&measure));
    button.set_sensitive(false);
    button.add_css_class("flat");
    button.update_property(&[gtk4::accessible::Property::Label(&format!(
        "{} (unavailable)",
        if title.is_empty() { "Add" } else { title }
    ))]);
    root.set_child(Some(&button));
    let visible = composer_icon_label(key, title);
    visible.set_can_target(false);
    visible.set_halign(gtk4::Align::Center);
    visible.set_valign(gtk4::Align::Center);
    root.add_overlay(&visible);
    root.set_measure_overlay(&visible, false);
    root
}

const REFERENCE_UI_KEY: &str = "forge-reference-ui-owner";

fn ui_for_widget(widget: &gtk4::Widget) -> Option<Rc<UiState>> {
    let root = widget.root()?;
    unsafe {
        root.data::<std::rc::Weak<UiState>>(REFERENCE_UI_KEY)
            .and_then(|weak| weak.as_ref().upgrade())
    }
}

pub(crate) fn trigger_existing_action(widget: &gtk4::Widget, action: crate::keybindings::Action) {
    if let Some(ui) = ui_for_widget(widget) {
        ui.execute_action(action);
    }
}

fn tools_history_viewport(widget: &gtk4::Widget) -> Option<gtk4::Viewport> {
    let mut ancestor = Some(widget.clone());
    while let Some(root) = ancestor {
        if root.has_css_class("term-view-root") {
            let scroll = widget_with_class_any(&root, "block-scroll")?
                .downcast::<gtk4::ScrolledWindow>()
                .ok()?;
            return scroll.child().and_downcast::<gtk4::Viewport>();
        }
        ancestor = root.parent();
    }
    None
}

/// Suppress GTK's independent focus reveal only while the popup acquires and
/// returns focus. History Tab navigation keeps its normal native policy.
fn guard_tools_focus(tools: &gtk4::MenuButton, popup: &gtk4::Popover) {
    let begin = guard_history_popup_focus(tools.upcast_ref(), popup);
    let click = gtk4::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk4::PropagationPhase::Capture);
    click.connect_pressed(move |_, _, _, _| begin());
    tools.add_controller(click);

    // Escape/dismissal returns shell input to the same active leaf. A menu
    // action or outside click that has already focused another control wins.
    // Run once before the history guard restores native focus reveal, so
    // merely closing Tools does not scroll the reader back to the editor.
    let owner: Rc<std::cell::RefCell<Option<glib::WeakRef<vte4::Terminal>>>> =
        Rc::new(std::cell::RefCell::new(None));
    let generation = Rc::new(std::cell::Cell::new(0u64));
    let weak = tools.downgrade();
    let saved_owner = owner.clone();
    let open_generation = generation.clone();
    popup.connect_show(move |_| {
        open_generation.set(open_generation.get().wrapping_add(1));
        *saved_owner.borrow_mut() = weak
            .upgrade()
            .and_then(|tools| ui_for_widget(tools.upcast_ref()))
            .and_then(|ui| ui.current_terminal())
            .filter(|terminal| terminal.is_mapped())
            .map(|terminal| terminal.downgrade());
    });
    let weak = tools.downgrade();
    let popup_weak = popup.downgrade();
    popup.connect_closed(move |_| {
        let Some(tools) = weak.upgrade() else { return };
        let saved_owner = owner.borrow_mut().take();
        let ticket = generation.get();
        let generation = generation.clone();
        let popup_weak = popup_weak.clone();
        tools.add_tick_callback(move |tools, _| {
            if generation.get() != ticket
                || popup_weak.upgrade().is_some_and(|popup| popup.is_mapped())
            {
                return glib::ControlFlow::Break;
            }
            let Some(ui) = ui_for_widget(tools.upcast_ref()) else {
                return glib::ControlFlow::Break;
            };
            let Some(terminal) = saved_owner.as_ref().and_then(glib::WeakRef::upgrade) else {
                return glib::ControlFlow::Break;
            };
            if !terminal.is_mapped()
                || ui.current_terminal().as_ref() != Some(&terminal)
                || !gtk4::prelude::RootExt::focus(&ui.window)
                    .is_some_and(|focused| focused.is_ancestor(tools))
            {
                return glib::ControlFlow::Break;
            }
            ui.focus_current_terminal();
            glib::ControlFlow::Break
        });
    });
}

/// A popup may return focus to history without rounding the reader's
/// fractional position. Restore native focus reveal after that short exchange;
/// normal keyboard navigation must still bring offscreen controls into view.
pub(crate) fn guard_history_popup_focus(
    anchor: &gtk4::Widget,
    popup: &gtk4::Popover,
) -> Rc<dyn Fn()> {
    let saved: Rc<std::cell::RefCell<Option<(gtk4::Viewport, bool)>>> =
        Rc::new(std::cell::RefCell::new(None));
    let generation = Rc::new(std::cell::Cell::new(0u64));
    let begin: Rc<dyn Fn()> = {
        let weak = anchor.downgrade();
        let saved = saved.clone();
        let generation = generation.clone();
        Rc::new(move || {
            generation.set(generation.get().wrapping_add(1));
            if saved.borrow().is_none() {
                if let Some(viewport) = weak
                    .upgrade()
                    .and_then(|tools| tools_history_viewport(tools.upcast_ref()))
                {
                    let previous = viewport.is_scroll_to_focus();
                    viewport.set_scroll_to_focus(false);
                    *saved.borrow_mut() = Some((viewport, previous));
                }
            }
        })
    };
    let action = begin.clone();
    popup.connect_show(move |_| action());
    let weak = anchor.downgrade();
    popup.connect_closed(move |_| {
        let Some(tools) = weak.upgrade() else {
            return;
        };
        let saved = saved.clone();
        let generation = generation.clone();
        let ticket = generation.get();
        let frames = std::cell::Cell::new(0);
        tools.add_tick_callback(move |_, _| {
            if generation.get() != ticket {
                return glib::ControlFlow::Break;
            }
            frames.set(frames.get() + 1);
            if frames.get() < 2 {
                return glib::ControlFlow::Continue;
            }
            if let Some((viewport, previous)) = saved.borrow_mut().take() {
                viewport.set_scroll_to_focus(previous);
            }
            glib::ControlFlow::Break
        });
    });
    begin
}

fn tool_menu_row(icon: &str, name: &str, detail: &str) -> gtk4::Button {
    let row = gtk4::Button::new();
    row.add_css_class("reference-tool-row");
    row.set_height_request(jterm_core::reference_popup::ROW_HEIGHT as i32);
    let content = gtk4::Box::new(Orientation::Horizontal, 8);
    content.append(&vector_icon(
        jterm_core::reference_icons::Icon::from_key(icon),
        jterm_core::reference_popup::ICON_SIZE as i32,
    ));
    let title = label(name, "reference-tool-name");
    title.set_hexpand(true);
    title.set_width_chars(1);
    content.append(&title);
    let info = label(detail, "reference-tool-detail");
    info.set_xalign(1.0);
    content.append(&info);
    row.set_child(Some(&content));
    row
}

/// One native tool strip for both presentations. Callers retain the only
/// input owner and append their existing, generation-bound send/stop controls.
pub(crate) fn composer_controls(on_terminal: impl Fn() + 'static) -> (gtk4::Box, gtk4::Label) {
    let on_terminal: Rc<dyn Fn()> = Rc::new(on_terminal);
    let controls = gtk4::Box::new(Orientation::Horizontal, 0);
    controls.add_css_class("chat-controls");
    controls.add_css_class("reference-composer-controls");
    controls.add_css_class("reference-fixed-controls");
    controls.set_valign(gtk4::Align::End);
    // Extra pixels in the minimum dock belong above this compact footer,
    // including when the native editor is seated in a running command card.
    controls.set_vexpand(true);
    let group = gtk4::Box::new(Orientation::Horizontal, token::METADATA_GAP as i32);
    group.add_css_class("reference-composer-left-group");
    for (key, title) in [
        ("plus", ""),
        ("attach", "Attach"),
        ("image", "Image"),
        ("code", "Code"),
    ] {
        let tool = unavailable_tool(key, title);
        tool.add_css_class(&format!("reference-composer-{key}"));
        tool.add_css_class("reference-sized-control");
        group.append(&tool);
    }
    let terminal = gtk4::Button::new();
    terminal.set_child(Some(&composer_icon_label("terminal", "Terminal")));
    terminal.add_css_class("flat");
    terminal.add_css_class("reference-composer-terminal");
    terminal.add_css_class("reference-sized-control");
    terminal.set_focus_on_click(false);
    terminal.set_tooltip_text(Some("Focus the current native shell editor"));
    let terminal_action = on_terminal.clone();
    terminal.connect_clicked(move |_| terminal_action());
    group.append(&terminal);
    let scroll = gtk4::ScrolledWindow::new();
    scroll.add_css_class("reference-composer-left-scroll");
    scroll.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
    scroll.set_propagate_natural_width(true);
    scroll.set_min_content_width(0);
    scroll.set_max_content_width(i32::MAX);
    scroll.set_hexpand(false);
    scroll.set_child(Some(&group));
    controls.append(&scroll);
    let tools = gtk4::MenuButton::new();
    tools.set_direction(gtk4::ArrowType::Up);
    tools.set_always_show_arrow(false);
    tools.add_css_class("reference-tools");
    tools.add_css_class("reference-composer-tools");
    tools.add_css_class("reference-sized-control");
    tools.add_css_class("flat");
    let tool_label = gtk4::Box::new(Orientation::Horizontal, 4);
    tool_label.add_css_class("reference-composer-icon-label");
    let grid = vector_icon(jterm_core::reference_icons::Icon::Grid, 12);
    grid.add_css_class("reference-composer-glyph");
    tool_label.append(&grid);
    let name = label("Tools", "reference-tools-title");
    name.set_width_chars(0);
    name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    tool_label.append(&name);
    let chevron = vector_icon(jterm_core::reference_icons::Icon::ChevronDown, 12);
    chevron.add_css_class("reference-composer-glyph");
    tool_label.append(&chevron);
    tools.set_child(Some(&tool_label));
    tools.set_tooltip_text(Some("Tools"));
    let popover = gtk4::Popover::new();
    popover.add_css_class("forge-popup");
    popover.add_css_class("reference-tools-popover");
    popover.set_position(gtk4::PositionType::Top);
    popover.set_has_arrow(false);
    let list = gtk4::Box::new(Orientation::Vertical, 0);
    list.set_width_request(
        (jterm_core::reference_popup::WIDTH - 2.0 * jterm_core::reference_popup::PADDING - 2.0)
            as i32,
    );
    for command in jterm_core::reference_popup::COMMANDS {
        use jterm_core::reference_popup::ToolCommandId;
        if command.separator_before {
            list.append(&gtk4::Separator::new(Orientation::Horizontal));
        }
        let index = command.id;
        let entry = tool_menu_row(command.icon_key, command.title, command.description);
        entry.set_widget_name(&format!("reference-tool-{:?}", command.id));
        if command.placeholder {
            entry.set_sensitive(false);
        } else {
            let popup = popover.downgrade();
            let terminal = on_terminal.clone();
            entry.connect_clicked(move |button| {
                if let Some(popup) = popup.upgrade() {
                    popup.popdown();
                }
                if index == ToolCommandId::Terminal {
                    terminal();
                } else if let Some(ui) = ui_for_widget(button.upcast_ref()) {
                    ui.execute_action(if index == ToolCommandId::CommandPalette {
                        crate::keybindings::Action::ToggleCommandPalette
                    } else {
                        crate::keybindings::Action::ToggleSearch
                    });
                }
            });
            if index != ToolCommandId::Terminal {
                entry.set_sensitive(false);
                entry.connect_map(|button| {
                    button.set_sensitive(ui_for_widget(button.upcast_ref()).is_some())
                });
            }
        }
        list.append(&entry);
    }
    popover.set_child(Some(&list));
    tools.set_popover(Some(&popover));
    guard_tools_focus(&tools, &popover);
    group.append(&tools);
    let fill = gtk4::Box::new(Orientation::Horizontal, 0);
    fill.add_css_class("reference-composer-gap");
    fill.set_width_request(token::METADATA_GAP as i32);
    fill.set_hexpand(true);
    controls.append(&fill);
    let model = disabled(
        token::MODEL_UNAVAILABLE_LABEL,
        "No agent model is selected for this shell conversation",
    );
    model.add_css_class("reference-model-selector");
    model.add_css_class("reference-sized-control");
    model.set_width_request(token::COMPOSER_MODEL_MIN_WIDTH as i32);
    model.set_margin_end(token::METADATA_GAP as i32);
    controls.append(&model);
    let status = label("Shell", "chat-input-hint");
    status.set_width_chars(1);
    status.set_max_width_chars(12);
    status.set_tooltip_text(Some("Shell · Tab completes · ↑ history"));
    status.set_accessible_role(gtk4::AccessibleRole::Status);
    status.set_visible(false);
    controls.append(&status);
    (controls, status)
}

/// Observe the whole fixed footer, never a zero-width spacer. The original
/// buttons and their input-authority callbacks remain owned by `controls`.
pub(crate) fn composer_controls_frame(controls: &gtk4::Box) -> gtk4::Overlay {
    let frame = gtk4::Overlay::new();
    frame.set_child(Some(controls));
    frame.set_vexpand(true);
    frame.set_valign(gtk4::Align::End);
    let observer = gtk4::DrawingArea::new();
    observer.set_can_target(false);
    observer.set_focusable(false);
    frame.add_overlay(&observer);
    frame.set_measure_overlay(&observer, false);
    let row = controls.downgrade();
    observer.connect_resize(move |_, _, _| {
        if let Some(row) = row.upgrade() {
            refresh_composer_row(&row);
        }
    });
    let row = controls.downgrade();
    frame.connect_map(move |_| {
        if let Some(row) = row.upgrade() {
            refresh_composer_row(&row);
        }
    });
    frame
}

fn refresh_composer_row(row: &gtk4::Box) {
    let scale = ui_for_widget(row.upcast_ref())
        .map_or(1.0, |ui| {
            let config = ui.config.borrow();
            crate::font::reference_ui_role_multiplier(
                &config.chrome_font_size,
                config.chrome_font_scale,
                &row.pango_context(),
            )
        })
        .max(1.0);
    let px = |value: f32| (f64::from(value) * scale).round() as i32;
    let mut ancestor = row.parent();
    let mut composer_width = None;
    while let Some(widget) = ancestor {
        if widget.has_css_class("chat-dock") || widget.has_css_class("reference-block-composer") {
            composer_width = row
                .root()
                .and_downcast::<gtk4::Window>()
                .and_then(|window| widget.compute_bounds(&window))
                .map(|bounds| f64::from(bounds.width()) / scale);
            break;
        }
        ancestor = widget.parent();
    }
    let width = composer_width.filter(|width| *width > 0.0);
    let mut pending = vec![row.clone().upcast::<gtk4::Widget>()];
    while let Some(widget) = pending.pop() {
        let mut child = widget.first_child();
        while let Some(next) = child {
            child = next.next_sibling();
            pending.push(next);
        }
        let dimension = [
            ("reference-composer-plus", token::COMPOSER_PLUS_WIDTH),
            ("reference-composer-attach", token::COMPOSER_ATTACH_WIDTH),
            ("reference-composer-image", token::COMPOSER_IMAGE_WIDTH),
            ("reference-composer-code", token::COMPOSER_CODE_WIDTH),
            (
                "reference-composer-terminal",
                token::COMPOSER_TERMINAL_WIDTH,
            ),
            ("reference-composer-tools", token::COMPOSER_TOOLS_WIDTH),
            ("reference-model-selector", token::COMPOSER_MODEL_MIN_WIDTH),
            ("chat-command-control", token::COMPOSER_SEND_WIDTH),
            ("chat-submit", token::COMPOSER_SEND_WIDTH),
            ("reference-native-tools-close", token::COMPOSER_SEND_WIDTH),
        ]
        .into_iter()
        .find_map(|(class, width)| widget.has_css_class(class).then_some(width));
        if let Some(dimension) = dimension {
            widget.add_css_class("reference-sized-control");
            widget.set_size_request(px(dimension), composer_control_height(scale));
            widget.set_valign(gtk4::Align::Center);
            widget.set_overflow(gtk4::Overflow::Hidden);
        }
        if let Some(width) = width {
            if ["attach", "image", "code"]
                .iter()
                .any(|key| widget.has_css_class(&format!("reference-composer-{key}")))
            {
                widget.set_visible(width >= f64::from(token::COMPOSER_SECONDARY_MIN_WIDTH));
            } else if widget.has_css_class("reference-composer-terminal") {
                widget.set_visible(width >= f64::from(token::COMPOSER_TERMINAL_MIN_AVAILABLE));
            } else if widget.has_css_class("reference-model-selector") {
                widget.set_visible(width >= f64::from(token::COMPOSER_MODEL_MIN_AVAILABLE));
            }
        }
        if widget.has_css_class("reference-model-selector") {
            widget.set_margin_end(px(token::METADATA_GAP));
        }
        if widget.has_css_class("reference-composer-gap") {
            widget.set_width_request(px(token::METADATA_GAP));
        }
        if widget.has_css_class("reference-composer-glyph") {
            if let Some(glyph) = widget.downcast_ref::<gtk4::DrawingArea>() {
                let size = (f64::from(jterm_core::reference_popup::FOOTER_ICON_SIZE)
                    * scale.min(1.25))
                .round() as i32;
                glyph.set_content_width(size);
                glyph.set_content_height(size);
            }
        }
        if let Some(group) = widget.downcast_ref::<gtk4::Box>() {
            if widget.has_css_class("reference-composer-left-group") {
                group.set_spacing(px(token::METADATA_GAP));
            } else if widget.has_css_class("reference-composer-icon-label") {
                group.set_spacing(px(jterm_core::reference_popup::FOOTER_ICON_LABEL_GAP));
            }
        }
        if widget.has_css_class("reference-composer-left-scroll") {
            if let Some(scroll) = widget.downcast_ref::<gtk4::ScrolledWindow>() {
                let full = token::COMPOSER_PLUS_WIDTH
                    + token::COMPOSER_ATTACH_WIDTH
                    + token::COMPOSER_IMAGE_WIDTH
                    + token::COMPOSER_CODE_WIDTH
                    + token::COMPOSER_TERMINAL_WIDTH
                    + token::COMPOSER_TOOLS_WIDTH
                    + 5.0 * token::METADATA_GAP;
                scroll.set_max_content_width(px(full));
            }
        }
    }
    row.set_height_request(composer_control_height(scale));
}

/// A UI font change can retain the same allocation. Refresh once from the
/// existing config-apply path as well as on real layout changes, without a
/// permanent frame timer or any terminal/input work.
pub(crate) fn refresh_composer_control_geometry(root: &gtk4::Widget) {
    if root.has_css_class("reference-composer-controls") {
        if let Some(row) = root.downcast_ref::<gtk4::Box>() {
            refresh_composer_row(row);
        }
        return;
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        refresh_composer_control_geometry(&widget);
    }
}

/// The canvas header names the active pane like the reference breadcrumb:
/// `Workspace  /  <tab title>` over `<Presentation> session · Local`, the
/// same two lines Anvil shows. The labelled fixture keeps its authored text.
pub(crate) fn sync_canvas_header(ui: &UiState, page: Option<&gtk4::Widget>) {
    if jterm_core::reference_conversation::fixture_enabled() {
        return;
    }
    let page = page.cloned().or_else(|| {
        ui.notebook
            .current_page()
            .and_then(|index| ui.notebook.nth_page(Some(index)))
    });
    let tab = page
        .as_ref()
        .and_then(|page| crate::state::tab_label_text(&ui.notebook, page))
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "Terminal".to_string());
    let presentation = page.as_ref().map_or("Terminal", |page| {
        if widget_with_class_any(page, "developer-block-canvas").is_some() {
            "Block"
        } else if widget_with_class_any(page, "term-chat-root").is_some() {
            "Chat"
        } else {
            "Terminal"
        }
    });
    let location = if private_session(ui) {
        "Private"
    } else {
        "Local"
    };
    let root = ui.window.upcast_ref::<gtk4::Widget>();
    for (class, text) in [
        ("reference-canvas-title", format!("Workspace  /  {tab}")),
        (
            "reference-canvas-subtitle",
            format!("{presentation} session · {location}"),
        ),
    ] {
        let Some(label) = widget_with_class_any(root, class).and_downcast::<gtk4::Label>() else {
            continue;
        };
        if label.text().as_str() != text {
            label.set_text(&text);
            label.set_tooltip_text(Some(&text));
        }
    }
}

fn widget_with_class(root: &gtk4::Widget, class: &str) -> Option<gtk4::Widget> {
    if root.has_css_class(class) && root.is_mapped() {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = widget_with_class(&widget, class) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn widget_with_class_any(root: &gtk4::Widget, class: &str) -> Option<gtk4::Widget> {
    if root.has_css_class(class) {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = widget_with_class_any(&widget, class) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn disabled(value: &str, description: &str) -> gtk4::Button {
    let button = gtk4::Button::with_label(value);
    button.set_sensitive(false);
    button.set_tooltip_text(Some(description));
    button.add_css_class("flat");
    button
}

fn icon(name: &str, description: &str) -> gtk4::Button {
    let button = gtk4::Button::from_icon_name(name);
    button.set_tooltip_text(Some(description));
    button.set_focus_on_click(false);
    button.add_css_class("flat");
    button
}

fn private_session(ui: &UiState) -> bool {
    ui.notebook
        .current_page()
        .and_then(|index| ui.notebook.nth_page(Some(index)))
        .as_ref()
        .and_then(super::tab_private_title_cell)
        .is_some_and(|flag| flag.get())
}

fn workspace_menu_snapshot(ui: &UiState) -> Vec<(String, Option<String>)> {
    (0..ui.notebook.n_pages())
        .filter_map(|index| {
            let page = ui.notebook.nth_page(Some(index))?;
            Some((
                page.widget_name().to_string(),
                super::tab_display_title(&ui.notebook, &page),
            ))
        })
        .collect()
}

/// A remote pane may only contribute its reported remote directory. Looking
/// up the local SSH client's process directory would describe another host.
fn session_directory(ui: &UiState) -> Option<String> {
    let terminal = ui.current_terminal()?;
    context_directory(
        private_session(ui),
        ui.current_pane_leaf().is_some_and(|leaf| leaf.is_remote()),
        terminal.current_directory_uri().as_deref(),
        || crate::terminal::terminal_working_directory(&terminal).filter(|path| !path.is_empty()),
    )
}

fn context_directory(
    private: bool,
    remote: bool,
    reported_uri: Option<&str>,
    local_directory: impl FnOnce() -> Option<String>,
) -> Option<String> {
    if private {
        return None;
    }
    if !remote {
        return local_directory();
    }
    let uri = reported_uri.filter(|uri| uri.len() <= 16 * 1024)?;
    let (path, _) = glib::filename_from_uri(uri).ok()?;
    let path = path.to_str()?;
    (std::path::Path::new(path).is_absolute()
        && jterm_core::execution_journal::is_valid_jsh_cwd(path))
    .then(|| path.into())
}

fn set_context_value(stack: &gtk4::Stack, key: &str, value: &str) {
    if let Some(page) = stack.child_by_name("context") {
        if let Some(label) = widget_with_class_any(&page, &format!("reference-context-{key}"))
            .and_then(|widget| widget.downcast::<gtk4::Label>().ok())
        {
            let safe = jterm_core::review_input::safe_inline_display(value, 1024);
            if label.text().as_str() != safe {
                label.set_text(&safe);
                label.set_tooltip_text(Some(&safe));
            }
        }
    }
}

fn context_action(title: &str, key: &'static str) -> gtk4::Button {
    let button = gtk4::Button::with_label(title);
    button.add_css_class(&format!("reference-context-{key}"));
    button.connect_clicked(move |button| {
        let Some(ui) = ui_for_widget(button.upcast_ref()) else {
            return;
        };
        // Recheck at activation: a privacy toggle or tab switch can happen
        // between the most recent heartbeat and the user's click.
        if private_session(&ui) {
            return;
        }
        match key {
            "copy" => {
                if let Some(directory) = session_directory(&ui) {
                    button.clipboard().set_text(&directory);
                }
            }
            "browse" => {
                ui.file_tree_goto_current_cwd();
                ui.apply_sidebar_view(SidebarView::Files, false);
            }
            _ => {}
        }
    });
    button
}

fn agent_actions() -> gtk4::Box {
    let actions = gtk4::Box::new(Orientation::Vertical, 6);
    actions.add_css_class("reference-agent-actions");
    for (title, action) in [
        (
            "Manage agent tasks",
            crate::keybindings::Action::ToggleTasksPanel,
        ),
        ("Open AI assistant", crate::keybindings::Action::OpenAiPanel),
        (
            "Saved agent sessions",
            crate::keybindings::Action::OpenAgentSessions,
        ),
    ] {
        let button = gtk4::Button::with_label(title);
        if action == crate::keybindings::Action::ToggleTasksPanel {
            button.set_tooltip_text(Some("Open recorded tasks. Creating tasks requires agent_tasks_enabled = true in the config file."));
        }
        button.connect_clicked(move |button| {
            let Some(ui) = ui_for_widget(button.upcast_ref()) else {
                return;
            };
            if action == crate::keybindings::Action::ToggleTasksPanel {
                if !ui.config.borrow().agent_tasks_enabled
                    && ui.agent_tasks.borrow().task_manager.tasks().is_empty()
                {
                    ui.toggle_tasks_panel();
                    return;
                }
                let displayed = {
                    let domain = ui.agent_tasks.borrow();
                    jterm_core::reference_conversation::observed_inspector_task(
                        domain.task_manager.tasks(),
                        domain.selected_task,
                    )
                    .map(|task| task.id)
                };
                if let Some(id) = displayed {
                    ui.execute_task_panel_action(crate::agent_task_ui::TaskPanelAction::Select(id));
                }
                if !ui.agent_tasks.borrow().panel_visible {
                    ui.toggle_tasks_panel();
                }
                ui.workspace_stack.set_visible_child_name("tasks");
            } else if action == crate::keybindings::Action::OpenAiPanel {
                if !ui.config.borrow().ai_enabled {
                    ui.execute_action(crate::keybindings::Action::ToggleSettings);
                    return;
                }
                // The explicit assistant action takes precedence over a task
                // page without closing or discarding its live domain state.
                ui.open_ai_panel();
                ui.workspace_stack.set_visible_child_name("chats");
            } else {
                ui.execute_action(action);
            }
        });
        actions.append(&button);
    }
    let review = gtk4::Button::with_label("Review task changes");
    review.set_tooltip_text(Some(
        "Review changes for the task displayed in this overview",
    ));
    review.add_css_class("reference-review-task");
    review.set_sensitive(false);
    review.connect_clicked(|button| {
        let Some(ui) = ui_for_widget(button.upcast_ref()) else {
            return;
        };
        if private_session(&ui) {
            return;
        }
        let displayed = unsafe {
            button
                .data::<Option<crate::agent_task::TaskId>>(REVIEW_TASK_KEY)
                .and_then(|target| *target.as_ref())
        };
        let displayed = {
            let domain = ui.agent_tasks.borrow();
            review_task_target(domain.task_manager.tasks(), displayed)
        };
        let Some(id) = displayed else {
            return;
        };
        ui.execute_task_panel_action(crate::agent_task_ui::TaskPanelAction::Select(id));
        if !ui.agent_tasks.borrow().panel_visible {
            ui.toggle_tasks_panel();
        }
        ui.workspace_stack.set_visible_child_name("tasks");
        ui.execute_task_panel_action(crate::agent_task_ui::TaskPanelAction::ReviewDiff(id));
    });
    actions.append(&review);
    actions
}

fn review_task_target(
    tasks: &[crate::agent_task::AgentTask],
    displayed: Option<crate::agent_task::TaskId>,
) -> Option<crate::agent_task::TaskId> {
    displayed.filter(|id| {
        tasks
            .iter()
            .any(|task| task.id == *id && task.status != crate::agent_task::TaskStatus::Archived)
    })
}

/// Keep the overview a projection of the same task domain as the native task
/// manager. Only the passive card is replaced; entry actions and scroll state
/// survive task updates and never own a separate task selection.
pub(crate) fn refresh_task_overview(ui: &UiState) {
    if jterm_core::reference_conversation::fixture_enabled() {
        return;
    }
    let Some(overview) = ui
        .side_stack
        .child_by_name("overview")
        .and_then(|page| widget_with_class_any(&page, "reference-agent-overview"))
        .and_then(|widget| widget.downcast::<gtk4::Box>().ok())
    else {
        return;
    };
    let domain = ui.agent_tasks.borrow();
    let mut task = jterm_core::reference_conversation::observed_inspector_task(
        domain.task_manager.tasks(),
        domain.selected_task,
    )
    .map(|task| task.id);
    let mut model = jterm_core::reference_conversation::observed_task_inspector(
        domain.task_manager.tasks(),
        domain.selected_task,
        |id| crate::agent_task::DashboardActivity {
            has_active_agent_stream: domain.task_manager.has_active_agent_event_stream(id),
            native_preparing: domain.agent_runtime.has_preparing(id),
            ..Default::default()
        },
    );
    drop(domain);
    let private = private_session(ui);
    if private {
        task = None;
        model = jterm_core::reference_conversation::Inspector::empty();
        model.title = "Private session".into();
        model.subtitle = "Open Tasks to inspect recorded work explicitly".into();
        model.badge = "Private".into();
        model.output_title = "Task privacy".into();
        model.output = "Task details are hidden for this private session.".into();
    }
    let snapshot = ObservedInspector { task, model };
    let unchanged = unsafe {
        overview
            .data::<ObservedInspector>(OBSERVED_INSPECTOR_KEY)
            .is_some_and(|previous| previous.as_ref() == &snapshot)
    };
    if unchanged {
        return;
    }
    if let Some(card) = overview
        .last_child()
        .filter(|child| child.has_css_class("reference-rich-inspector"))
    {
        overview.remove(&card);
    }
    overview.append(&super::reference_conversation::inspector(
        &snapshot.model,
        false,
    ));
    if let Some(review) = widget_with_class_any(overview.upcast_ref(), "reference-review-task") {
        review.set_sensitive(snapshot.task.is_some());
        unsafe {
            review.set_data(REVIEW_TASK_KEY, snapshot.task);
        }
    }
    unsafe {
        overview.set_data(OBSERVED_INSPECTOR_KEY, snapshot);
    }
}

/// The always-present inspector has an honest empty state until a real task
/// or chat owns its existing pages. The Files page is the real file browser.
pub(crate) fn inspector(stack: &gtk4::Stack) -> gtk4::Box {
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    let fixture = jterm_core::reference_conversation::fixture_enabled();
    let inspector = if fixture {
        jterm_core::reference_conversation::acceptance_fixture().inspector
    } else {
        jterm_core::reference_conversation::Inspector::empty()
    };
    let overview = if fixture {
        super::reference_conversation::inspector(&inspector, true)
    } else {
        let overview = gtk4::Box::new(Orientation::Vertical, 8);
        overview.add_css_class("reference-agent-overview");
        overview.append(&agent_actions());
        overview.append(&super::reference_conversation::inspector(&inspector, false));
        unsafe {
            overview.set_data(
                OBSERVED_INSPECTOR_KEY,
                ObservedInspector {
                    task: None,
                    model: inspector,
                },
            );
        }
        overview
    };
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroll.set_child(Some(&overview));
    scroll.set_vexpand(true);
    stack.add_named(&scroll, Some("overview"));
    let context = gtk4::Box::new(Orientation::Vertical, 12);
    context.add_css_class("reference-agent-card");
    context.add_css_class("reference-session-context");
    context.append(&label("Session context", "reference-card-title"));
    context.append(&label(
        "The current terminal owns its shell and working directory.",
        "reference-muted",
    ));
    context.append(&label("Directory", "reference-section-label"));
    let path = label("Waiting for a terminal", "reference-context-path");
    path.set_wrap(true);
    path.set_selectable(true);
    context.append(&path);
    for (title, key) in [
        ("Session", "session"),
        ("Connection", "connection"),
        ("Git branch", "branch"),
        ("Presentation", "mode"),
        ("Foreground process", "process"),
        ("Prompt", "prompt"),
        ("Terminal size", "size"),
        ("AI command context", "sharing"),
    ] {
        context.append(&label(title, "reference-section-label"));
        let value = label("Waiting for a terminal", "reference-metadata");
        value.add_css_class(&format!("reference-context-{key}"));
        value.set_selectable(true);
        value.set_wrap(true);
        context.append(&value);
    }
    let context_actions = gtk4::Box::new(Orientation::Horizontal, 6);
    context_actions.append(&context_action("Copy directory", "copy"));
    context_actions.append(&context_action("Browse files", "browse"));
    context.append(&context_actions);
    let settings = gtk4::Button::with_label("Sharing settings");
    settings.connect_clicked(|button| {
        if let Some(ui) = ui_for_widget(button.upcast_ref()) {
            ui.execute_action(crate::keybindings::Action::ToggleSettings);
        }
    });
    context.append(&settings);
    context.append(&label(
        "These are local session facts. AI requests follow your context-sharing and redaction settings.",
        "reference-muted",
    ));
    stack.add_named(&context, Some("context"));
    stack.set_visible_child_name("overview");
    let root = gtk4::Box::new(Orientation::Vertical, 0);
    root.add_css_class("reference-inspector");
    root.set_width_request(token::MIN_INSPECTOR_WIDTH as i32);
    root.set_vexpand(true);
    let tabs = gtk4::Box::new(Orientation::Horizontal, 6);
    tabs.add_css_class("reference-inspector-tabs");
    tabs.set_height_request(token::INSPECTOR_TABS_HEIGHT as i32);
    let agent = gtk4::ToggleButton::with_label("Agent");
    let context_tab = gtk4::ToggleButton::with_label("Context");
    let files = gtk4::ToggleButton::with_label("Files");
    for (button, page) in [
        (&agent, "overview"),
        (&context_tab, "context"),
        (&files, "files"),
    ] {
        button.add_css_class("flat");
        button.set_width_request(token::INSPECTOR_TAB_WIDTH as i32);
        button.set_valign(gtk4::Align::Center);
        let stack = stack.clone();
        button.connect_clicked(move |_| stack.set_visible_child_name(page));
        tabs.append(button);
    }
    let fill = gtk4::Box::new(Orientation::Horizontal, 0);
    fill.set_hexpand(true);
    tabs.append(&fill);
    let menu = gtk4::MenuButton::new();
    menu.set_label("⋮");
    menu.add_css_class("flat");
    menu.set_tooltip_text(Some("Inspector actions"));
    let popup = gtk4::Popover::new();
    popup.add_css_class("forge-popup");
    let menu_actions = gtk4::Box::new(Orientation::Vertical, 4);
    for (title, page) in [
        ("Agent overview", "overview"),
        ("Session context", "context"),
        ("Workspace files", "files"),
    ] {
        let button = gtk4::Button::with_label(title);
        let stack = stack.downgrade();
        let popup = popup.downgrade();
        button.connect_clicked(move |_| {
            if let Some(stack) = stack.upgrade() {
                stack.set_visible_child_name(page);
            }
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }
        });
        menu_actions.append(&button);
    }
    popup.set_child(Some(&menu_actions));
    menu.set_popover(Some(&popup));
    tabs.append(&menu);
    let agent_for_sync = agent.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        let name = stack.visible_child_name();
        agent_for_sync.set_active(matches!(
            name.as_deref(),
            Some("overview" | "tasks" | "chats")
        ));
        context_tab.set_active(name.as_deref() == Some("context"));
        files.set_active(name.as_deref() == Some("files"));
    });
    agent.set_active(true);
    let tabs_scroll = gtk4::ScrolledWindow::new();
    tabs_scroll.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
    tabs_scroll.set_min_content_width(0);
    tabs_scroll.set_max_content_width(1);
    tabs_scroll.set_propagate_natural_width(false);
    tabs_scroll.set_focusable(false);
    tabs_scroll.set_child(Some(&tabs));
    root.append(&tabs_scroll);
    // Live AI/Tasks pages contain native editors and legacy controls whose
    // minimum size can exceed the inspector contract. Keep those widgets and
    // their focus/drafts intact inside a bounded, scrollable host; the Paned
    // must not steal canvas width when a page changes or a long label arrives.
    let pages = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Automatic)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .min_content_width(0)
        .max_content_width(1)
        .propagate_natural_width(false)
        .propagate_natural_height(false)
        .hexpand(true)
        .vexpand(true)
        .child(stack)
        .build();
    pages.add_css_class("reference-inspector-pages");
    root.append(&pages);
    root
}

pub(crate) fn context_label(stack: &gtk4::Stack) -> Option<gtk4::Label> {
    let page = stack.child_by_name("context")?;
    let mut child = page.first_child();
    while let Some(widget) = child {
        if widget.has_css_class("reference-context-path") {
            return widget.downcast().ok();
        }
        child = widget.next_sibling();
    }
    None
}

pub(crate) fn center_header(top_tabs: &gtk4::ScrolledWindow) -> gtk4::Box {
    let header = gtk4::Box::new(Orientation::Horizontal, 10);
    header.add_css_class("reference-canvas-header");
    header.set_height_request(token::CANVAS_HEADER_HEIGHT as i32);
    let title = gtk4::Box::new(Orientation::Vertical, 3);
    title.set_valign(gtk4::Align::Center);
    if jterm_core::reference_conversation::fixture_enabled() {
        let model = jterm_core::reference_conversation::acceptance_fixture();
        title.append(&label(&model.title, "reference-canvas-title"));
        title.append(&label(&model.subtitle, "reference-canvas-subtitle"));
    } else {
        title.append(&label("Workspace  /  Terminal", "reference-canvas-title"));
        title.append(&label("Terminal session", "reference-canvas-subtitle"));
    }
    header.append(&title);
    // The retained strip now lives in this ordinary measured header, rather
    // than in the titlebar overlay. Keep chrome font routing on the scroller
    // itself so placement changes do not make tabs inherit the terminal font.
    top_tabs.add_css_class("forge-font-chrome");
    top_tabs.set_margin_start(0);
    top_tabs.set_margin_end(0);
    header.append(top_tabs);
    let fill = gtk4::Box::new(Orientation::Horizontal, 0);
    fill.set_hexpand(true);
    header.append(&fill);
    let actions = gtk4::Box::new(Orientation::Horizontal, token::HEADER_ACTION_GAP as i32);
    actions.add_css_class("reference-header-actions");
    for key in ["plus", "search", "more"] {
        let action = gtk4::Button::new();
        action.set_child(Some(&vector_icon(
            jterm_core::reference_icons::Icon::from_key(key),
            14,
        )));
        action.set_size_request(
            token::HEADER_ACTION_SIZE as i32,
            token::HEADER_ACTION_SIZE as i32,
        );
        action.set_valign(gtk4::Align::Center);
        action.add_css_class("reference-header-action");
        action.add_css_class(&format!("reference-header-{key}"));
        actions.append(&action);
    }
    header.append(&actions);
    header
}

/// Rebuild only the outer shell once. Real session widgets are reparented,
/// never reconstructed when a frame, command or allocation changes.
#[allow(clippy::too_many_arguments)]
pub(crate) fn install(
    ui: &Rc<UiState>,
    main: &gtk4::Box,
    content: &gtk4::Paned,
    top: &gtk4::Overlay,
    top_background: &gtk4::Box,
    leading: &gtk4::Box,
    trailing: &gtk4::Box,
    palette: &gtk4::Button,
    sidebar_toggle: &gtk4::Button,
    placement_toggle: &gtk4::Button,
    vim: &gtk4::Button,
    add: &gtk4::Button,
    sessions: &gtk4::Button,
    minimize: &gtk4::Button,
    maximize: &gtk4::Button,
    close: &gtk4::Button,
) {
    ui.window.add_css_class("reference-shell");
    unsafe {
        ui.window.set_data(REFERENCE_UI_KEY, Rc::downgrade(ui));
    }
    let legacy_tools_visible = Rc::new(std::cell::Cell::new(false));
    let legacy_tools_footer = gtk4::Box::new(Orientation::Vertical, 0);
    legacy_tools_footer.add_css_class("reference-block-composer");
    legacy_tools_footer.add_css_class("reference-native-tools");
    // A hidden native-grid tools strip must never compete for vertical space.
    legacy_tools_footer.set_vexpand(false);
    legacy_tools_footer.set_visible(false);
    // Native-grid input has no second composer. Keep only an optional,
    // closable tools strip; the actual Chat/Block editor docks are unchanged.
    if let Some(center) = ui.notebook.parent().and_downcast::<gtk4::Box>() {
        let footer = legacy_tools_footer.clone();
        let ui_for_terminal = Rc::downgrade(ui);
        let (actions, _) = composer_controls(move || {
            if let Some(ui) = ui_for_terminal.upgrade() {
                ui.focus_current_terminal();
            }
        });
        actions.set_vexpand(false);
        let close_tools = gtk4::Button::new();
        close_tools.set_child(Some(&vector_icon(
            jterm_core::reference_icons::Icon::Close,
            12,
        )));
        close_tools.set_focus_on_click(false);
        close_tools.set_size_request(token::CONTROL_HEIGHT as i32, token::CONTROL_HEIGHT as i32);
        close_tools.add_css_class("reference-native-tools-close");
        close_tools.add_css_class("reference-sized-control");
        close_tools.set_tooltip_text(Some(
            "Hide terminal tools; input stays in the terminal grid",
        ));
        let footer_for_close = footer.downgrade();
        let visible_for_close = legacy_tools_visible.clone();
        let ui_for_close = Rc::downgrade(ui);
        close_tools.connect_clicked(move |_| {
            visible_for_close.set(false);
            if let Some(footer) = footer_for_close.upgrade() {
                footer.set_visible(false);
            }
            if let Some(ui) = ui_for_close.upgrade() {
                if let Some(toggle) =
                    widget_with_class_any(ui.window.upcast_ref(), "reference-native-tools-toggle")
                        .and_then(|widget| widget.downcast::<gtk4::CheckButton>().ok())
                {
                    toggle.set_active(false);
                }
                ui.focus_current_terminal();
            }
        });
        actions.append(&close_tools);
        let controls_frame = composer_controls_frame(&actions);
        controls_frame.set_vexpand(false);
        footer.append(&controls_frame);
        footer.set_visible(false);
        center.append(&footer);
        if ui.config.borrow().terminal_mode == TerminalMode::Block {
            center.add_css_class("reference-developer-active");
            if let Some(header) =
                widget_with_class_any(center.upcast_ref(), "reference-canvas-header")
            {
                header.set_height_request(token::CANVAS_HEADER_HEIGHT as i32);
            }
        }
    }
    top.add_css_class("reference-titlebar");
    top.set_height_request(token::TITLEBAR_HEIGHT as i32);
    top.remove_overlay(leading);
    top.remove_overlay(trailing);
    for button in [palette, sidebar_toggle, placement_toggle, vim] {
        leading.remove(button);
    }
    for button in [sessions, add, minimize, maximize, close] {
        trailing.remove(button);
    }
    // The retained task/agent controls move to the rail below.
    trailing.remove(&ui.tasks_button);
    trailing.remove(&ui.agent_toggle);
    let toolbar = gtk4::Box::new(Orientation::Horizontal, 6);
    toolbar.add_css_class("reference-toolbar");
    for control in [close, minimize, maximize] {
        control.add_css_class("reference-window-control");
        control.set_valign(gtk4::Align::Center);
        toolbar.append(control);
    }
    let identity = gtk4::Box::new(Orientation::Horizontal, 6);
    identity.append(&product_identity(
        "forge",
        jterm_core::reference_identity::PRODUCT_ICON_SIZE as i32,
    ));
    identity.append(&label("Forge", "reference-product"));
    toolbar.append(&identity);
    let path = label("Workspace", "reference-toolbar-path");
    path.set_width_chars(16);
    path.set_max_width_chars(24);
    toolbar.append(&path);
    let search = gtk4::Box::new(Orientation::Horizontal, 8);
    search.append(&vector_icon(jterm_core::reference_icons::Icon::Search, 16));
    let search_label = label("Search commands…", "reference-muted");
    search_label.set_hexpand(true);
    search.append(&search_label);
    search.append(&label("Ctrl ⇧ P", "reference-shortcut"));
    palette.set_child(Some(&search));
    palette.add_css_class("reference-search");
    palette.set_hexpand(false);
    palette.set_width_request(token::SEARCH_WIDTH as i32);
    palette.set_height_request(token::SEARCH_HEIGHT as i32);
    palette.set_halign(gtk4::Align::Center);
    palette.set_valign(gtk4::Align::Start);
    palette.set_margin_start((token::SEARCH_CENTER_OFFSET * 2.0) as i32);
    palette.set_margin_top(token::SEARCH_TOP as i32);
    top.add_overlay(palette);
    top.set_measure_overlay(palette, false);
    let toolbar_fill = gtk4::Box::new(Orientation::Horizontal, 0);
    toolbar_fill.set_hexpand(true);
    toolbar.append(&toolbar_fill);
    let modes = gtk4::Box::new(Orientation::Horizontal, 2);
    modes.add_css_class("reference-mode-switch");
    modes.set_valign(gtk4::Align::Start);
    modes.set_margin_top(token::MODE_CONTROL_TOP as i32);
    modes.set_height_request(token::MODE_CONTROL_HEIGHT as i32);
    let mut group: Option<gtk4::ToggleButton> = None;
    for (title, mode) in [("Block", TerminalMode::Block), ("Chat", TerminalMode::Chat)] {
        let button = gtk4::ToggleButton::with_label(title);
        button.add_css_class(if mode == TerminalMode::Chat {
            "reference-mode-chat"
        } else {
            "reference-mode-block"
        });
        button.set_width_request(token::MODE_CONTROL_MIN_WIDTH as i32);
        button.set_focus_on_click(false);
        if let Some(first) = &group {
            button.set_group(Some(first));
        } else {
            group = Some(button.clone());
        }
        button.set_active(
            (mode == TerminalMode::Chat)
                == (ui.config.borrow().terminal_mode == TerminalMode::Chat),
        );
        button.set_tooltip_text(Some(&format!(
            "Open a new {title} tab; existing terminal sessions are retained"
        )));
        let ui = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            if let Some(ui) = ui.upgrade() {
                if active_mode(&ui).is_some_and(|active| {
                    (active == TerminalMode::Chat) == (mode == TerminalMode::Chat)
                }) {
                    return;
                }
                ui.config.borrow_mut().terminal_mode = mode.clone();
                ui.persist_config();
                ui.execute_action(crate::keybindings::Action::NewTab);
            }
        });
        modes.append(&button);
    }
    toolbar.append(&modes);
    sessions.set_valign(gtk4::Align::Center);
    toolbar.append(sessions);
    let inspector_toggle = gtk4::ToggleButton::new();
    inspector_toggle.set_child(Some(&vector_icon(
        jterm_core::reference_icons::Icon::PanelRight,
        16,
    )));
    inspector_toggle.add_css_class("reference-inspector-toggle");
    inspector_toggle.set_size_request(token::CONTROL_HEIGHT as i32, token::CONTROL_HEIGHT as i32);
    inspector_toggle.set_valign(gtk4::Align::Center);
    inspector_toggle.set_focus_on_click(false);
    inspector_toggle.set_tooltip_text(Some("Hide right sidebar"));
    inspector_toggle.update_property(&[gtk4::accessible::Property::Label("Hide right sidebar")]);
    toolbar.append(&inspector_toggle);
    add.set_valign(gtk4::Align::Center);
    let header_root = ui
        .notebook
        .parent()
        .unwrap_or_else(|| ui.window.clone().upcast());
    if let Some(placeholder) = widget_with_class_any(&header_root, "reference-header-plus") {
        if let Some(actions) = placeholder.parent().and_downcast::<gtk4::Box>() {
            add.set_child(Some(&vector_icon(
                jterm_core::reference_icons::Icon::Plus,
                14,
            )));
            add.add_css_class("reference-header-action");
            add.set_size_request(
                token::HEADER_ACTION_SIZE as i32,
                token::HEADER_ACTION_SIZE as i32,
            );
            actions.insert_child_after(add, placeholder.prev_sibling().as_ref());
            actions.remove(&placeholder);
        }
    }
    for (class, action) in [
        (
            "reference-header-search",
            crate::keybindings::Action::ToggleSearch,
        ),
        (
            "reference-header-more",
            crate::keybindings::Action::ToggleCommandPalette,
        ),
    ] {
        if let Some(button) = widget_with_class_any(&header_root, class)
            .and_then(|w| w.downcast::<gtk4::Button>().ok())
        {
            let weak = Rc::downgrade(ui);
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.execute_action(action);
                }
            });
        }
    }
    top_background.append(&toolbar);

    ui.sidebar.add_css_class("reference-navigation");
    ui.sidebar
        .set_width_request(token::MIN_NAVIGATION_WIDTH as i32);
    if let Some(switcher) = ui
        .sidebar
        .first_child()
        .filter(|w| w.has_css_class("sidebar-switcher"))
    {
        ui.sidebar.remove(&switcher);
    }
    let section = label("WORKSPACE", "reference-section-label");
    ui.sidebar.prepend(&section);
    let workspace = gtk4::MenuButton::new();
    workspace.set_always_show_arrow(false);
    let workspace_slots = gtk4::Box::new(Orientation::Horizontal, 6);
    workspace_slots.append(&vector_icon(jterm_core::reference_icons::Icon::Files, 16));
    let workspace_name = label("Local workspace", "reference-workspace-name");
    workspace_name.set_width_chars(1);
    workspace_name.set_hexpand(true);
    workspace_slots.append(&workspace_name);
    let workspace_branch = label("—", "reference-workspace-branch");
    workspace_branch.set_max_width_chars(9);
    workspace_slots.append(&workspace_branch);
    workspace_slots.append(&vector_icon(
        jterm_core::reference_icons::Icon::ChevronDown,
        12,
    ));
    workspace.set_child(Some(&workspace_slots));
    workspace.add_css_class("reference-workspace-selector");
    workspace.set_tooltip_text(Some("Switch terminal workspace"));
    let workspace_popup = gtk4::Popover::new();
    workspace_popup.add_css_class("forge-popup");
    workspace_popup.set_has_arrow(false);
    let workspace_list = gtk4::Box::new(Orientation::Vertical, 4);
    workspace_list.set_width_request(240);
    let workspace_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .max_content_height(420)
        .propagate_natural_height(true)
        .child(&workspace_list)
        .build();
    workspace_popup.set_child(Some(&workspace_scroll));
    workspace.set_popover(Some(&workspace_popup));
    let workspace_snapshot = Rc::new(std::cell::RefCell::new(Vec::new()));
    let snapshot_for_privacy = workspace_snapshot.clone();
    let workspace_popup_for_privacy = workspace_popup.downgrade();
    let ui_for_privacy = Rc::downgrade(ui);
    ui.window.connect_title_notify(move |_| {
        // Ordinary terminal heartbeat notifications can repeat the same title.
        // Dismiss only when displayed row identities/titles actually changed;
        // privacy changes therefore remove the previous public rows promptly.
        if let (Some(popup), Some(ui)) = (
            workspace_popup_for_privacy.upgrade(),
            ui_for_privacy.upgrade(),
        ) {
            if popup.is_visible() && *snapshot_for_privacy.borrow() != workspace_menu_snapshot(&ui)
            {
                popup.popdown();
            }
        }
    });
    let weak_ui = Rc::downgrade(ui);
    workspace_popup.connect_show(move |popup| {
        while let Some(child) = workspace_list.first_child() {
            workspace_list.remove(&child);
        }
        let Some(ui) = weak_ui.upgrade() else {
            return;
        };
        *workspace_snapshot.borrow_mut() = workspace_menu_snapshot(&ui);
        workspace_list.append(&label("TERMINAL WORKSPACES", "reference-section-label"));
        for index in 0..ui.notebook.n_pages() {
            let Some(page) = ui.notebook.nth_page(Some(index)) else {
                continue;
            };
            let title = super::tab_display_title(&ui.notebook, &page)
                .unwrap_or_else(|| format!("Terminal {}", index + 1));
            let safe = jterm_core::review_input::safe_inline_display(&title, 160);
            let button = gtk4::Button::with_label(&format!(
                "{}{}",
                if ui.notebook.current_page() == Some(index) {
                    "✓  "
                } else {
                    ""
                },
                safe
            ));
            button.set_tooltip_text(Some(&safe));
            if let Some(title) = button.child().and_downcast::<gtk4::Label>() {
                title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                title.set_max_width_chars(36);
                title.set_xalign(0.0);
            }
            let ui = Rc::downgrade(&ui);
            let page = page.downgrade();
            let popup = popup.downgrade();
            button.connect_clicked(move |_| {
                if let (Some(ui), Some(page)) = (ui.upgrade(), page.upgrade()) {
                    if let Some(index) = ui.notebook.page_num(&page) {
                        ui.notebook.set_current_page(Some(index));
                        ui.focus_current_terminal();
                    }
                }
                if let Some(popup) = popup.upgrade() {
                    popup.popdown();
                }
            });
            workspace_list.append(&button);
        }
        let new_terminal = gtk4::Button::with_label("New terminal workspace");
        let ui = Rc::downgrade(&ui);
        let popup = popup.downgrade();
        new_terminal.connect_clicked(move |_| {
            if let Some(ui) = ui.upgrade() {
                ui.execute_action(crate::keybindings::Action::NewTab);
            }
            if let Some(popup) = popup.upgrade() {
                popup.popdown();
            }
        });
        workspace_list.append(&new_terminal);
    });
    ui.sidebar.insert_child_after(&workspace, Some(&section));
    let navigation = gtk4::Box::new(Orientation::Vertical, token::NAVIGATION_ROW_GAP as i32);
    navigation.add_css_class("reference-navigation-links");
    for (index, (key, title)) in jterm_core::reference_conversation::NAVIGATION
        .iter()
        .enumerate()
    {
        let title = if index == 0 {
            if ui.config.borrow().terminal_mode == TerminalMode::Chat {
                "Chat"
            } else {
                "Block"
            }
        } else {
            title
        };
        let button = gtk4::Button::new();
        button.set_child(Some(&icon_label(key, title)));
        button.set_height_request(token::NAVIGATION_ROW_HEIGHT as i32);
        button.add_css_class("reference-navigation-link");
        if index == 0 {
            button.add_css_class("reference-navigation-active");
        }
        let ui = Rc::downgrade(ui);
        button.connect_clicked(move |_| {
            if let Some(ui) = ui.upgrade() {
                match index {
                    0 => ui.apply_sidebar_view(SidebarView::Tabs, false),
                    1 => ui.apply_sidebar_view(SidebarView::Files, false),
                    2 => {
                        if let Some(terminal) = ui.current_terminal() {
                            terminal.grab_focus();
                        }
                    }
                    3 => ui.execute_action(crate::keybindings::Action::ToggleTasksPanel),
                    _ => ui.execute_action(crate::keybindings::Action::ToggleSettings),
                }
            }
        });
        navigation.append(&button);
    }
    if jterm_core::reference_conversation::fixture_enabled() {
        ui.sidebar.insert_child_after(&navigation, Some(&workspace));
    }
    let recent = label("RECENT", "reference-section-label");
    recent.add_css_class("reference-recent-section");
    if jterm_core::reference_conversation::fixture_enabled() {
        ui.sidebar.insert_child_after(&recent, Some(&navigation));
    }
    if jterm_core::reference_conversation::fixture_enabled() {
        ui.sidebar_stack.set_visible(false);
        let entries = gtk4::Box::new(Orientation::Vertical, 0);
        entries.set_vexpand(false);
        entries.set_height_request((token::RECENT_ROW_HEIGHT * 5.0) as i32);
        entries.add_css_class("reference-recent-entries");
        for item in jterm_core::reference_conversation::fixture_recent_entries() {
            let row = gtk4::Box::new(Orientation::Horizontal, 4);
            row.add_css_class("reference-recent-row");
            row.set_height_request(token::RECENT_ROW_HEIGHT as i32);
            if item.selected {
                row.add_css_class("reference-recent-selected");
            }
            row.append(&vector_icon(
                jterm_core::reference_icons::Icon::from_key(item.icon_key),
                14,
            ));
            let content = gtk4::Box::new(Orientation::Vertical, 2);
            content.set_hexpand(true);
            content.set_valign(gtk4::Align::Center);
            content.append(&label(&item.title, "reference-recent-title"));
            content.append(&label(&item.detail, "reference-recent-detail"));
            row.append(&content);
            row.append(&label(&item.time, "reference-recent-time"));
            row.set_tooltip_text(Some(
                "Visual acceptance fixture; not inserted in session history",
            ));
            entries.append(&row);
        }
        ui.sidebar.insert_child_after(&entries, Some(&recent));
    }
    let spaces = gtk4::Box::new(Orientation::Vertical, 0);
    spaces.add_css_class("reference-spaces");
    spaces.set_margin_top(25);
    spaces.append(&label("SPACES", "reference-section-label"));
    for (name, description) in jterm_core::reference_conversation::SPACES {
        let row = gtk4::Box::new(Orientation::Horizontal, 0);
        row.set_tooltip_text(Some(
            "Unavailable — cross-application spaces are not connected",
        ));
        let contents = gtk4::Box::new(Orientation::Horizontal, 8);
        let identity = gtk4::Box::new(Orientation::Horizontal, 0);
        identity.set_valign(gtk4::Align::Center);
        identity.set_halign(gtk4::Align::Center);
        identity.add_css_class("reference-space-icon");
        identity.add_css_class(&format!("reference-space-{}", name.to_lowercase()));
        identity.append(&product_identity(
            name,
            jterm_core::reference_identity::SPACE_ICON_SIZE as i32,
        ));
        contents.append(&identity);
        let names = gtk4::Box::new(Orientation::Vertical, 2);
        let title_row = gtk4::Box::new(Orientation::Horizontal, 6);
        let name = label(name, "reference-space-title");
        name.set_hexpand(true);
        title_row.append(&name);
        title_row.append(&label("Unavailable", "reference-space-unavailable"));
        names.append(&title_row);
        names.append(&label(description, "reference-space-description"));
        contents.append(&names);
        row.append(&contents);
        row.add_css_class("reference-space-row");
        row.set_height_request(token::SPACES_ROW_HEIGHT as i32);
        spaces.append(&row);
    }
    if jterm_core::reference_conversation::fixture_enabled() {
        ui.sidebar.append(&spaces);
    }
    let fill = gtk4::Box::new(Orientation::Vertical, 0);
    fill.set_vexpand(true);
    ui.sidebar.append(&fill);
    main.remove(&ui.bottom_bar);
    let status_scroll = gtk4::ScrolledWindow::new();
    status_scroll.add_css_class("reference-navigation-status");
    status_scroll.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
    status_scroll.set_propagate_natural_width(false);
    status_scroll.set_max_content_width(1);
    status_scroll.set_min_content_width(0);
    // Include the existing one-pixel top stroke inside the49px border box.
    status_scroll.set_height_request((token::NAVIGATION_STATUS_HEIGHT - 1.0) as i32);
    ui.bottom_bar.set_valign(gtk4::Align::Start);
    ui.bottom_bar_left.set_visible(false);
    ui.bottom_bar_right.set_visible(false);
    let local = gtk4::Box::new(Orientation::Horizontal, 6);
    let dot = label("●", "reference-status-dot");
    local.append(&dot);
    local.append(&label("Local session", "reference-local-label"));
    local.add_css_class("reference-local-session");
    ui.bottom_bar.append(&local);
    status_scroll.set_child(Some(&ui.bottom_bar));
    ui.sidebar.append(&status_scroll);

    // Keep the public sidebar/controller and bottom status in place. Only
    // the navigation document scrolls when a short window cannot fit all
    // workspace/recent/space rows; its minimum must not resize the window.
    let navigation_document = gtk4::Box::new(Orientation::Vertical, 0);
    navigation_document.add_css_class("reference-navigation-document");
    navigation_document.set_vexpand(true);
    while let Some(child) = ui.sidebar.first_child() {
        if child == status_scroll.clone().upcast::<gtk4::Widget>() {
            break;
        }
        ui.sidebar.remove(&child);
        navigation_document.append(&child);
    }
    let navigation_scroll = gtk4::ScrolledWindow::new();
    navigation_scroll.add_css_class("reference-navigation-viewport");
    navigation_scroll.set_policy(gtk4::PolicyType::External, gtk4::PolicyType::External);
    navigation_scroll.set_min_content_width(0);
    navigation_scroll.set_max_content_width(1);
    navigation_scroll.set_propagate_natural_width(false);
    navigation_scroll.set_min_content_height(0);
    navigation_scroll.set_propagate_natural_height(false);
    navigation_scroll.set_vexpand(true);
    navigation_scroll.set_child(Some(&navigation_document));
    ui.sidebar.prepend(&navigation_scroll);

    let rail = gtk4::Box::new(Orientation::Vertical, 8);
    rail.add_css_class("reference-rail");
    rail.set_width_request(token::RAIL_WIDTH as i32);
    let mut gated_agent_button = None;
    for (index, (key, title)) in jterm_core::reference_conversation::RAIL_NAVIGATION
        .iter()
        .enumerate()
    {
        let button = if index == 0 {
            sidebar_toggle.clone()
        } else {
            gtk4::Button::new()
        };
        button.add_css_class("flat");
        button.set_width_request(token::SELECTED_RAIL_SIZE as i32);
        button.set_height_request(token::SELECTED_RAIL_SIZE as i32);
        button.set_focus_on_click(false);
        button.set_tooltip_text(Some(title));
        button.update_property(&[gtk4::accessible::Property::Label(title)]);
        button.set_child(Some(&vector_icon(
            if *key == "workspace" {
                jterm_core::reference_icons::Icon::Grid
            } else {
                jterm_core::reference_icons::Icon::from_key(key)
            },
            18,
        )));
        if index == 0 {
            button.add_css_class("reference-rail-selected");
        } else {
            let ui = Rc::downgrade(ui);
            let palette = palette.clone();
            button.connect_clicked(move |_| {
                if let Some(ui) = ui.upgrade() {
                    match index {
                        1 => ui.apply_sidebar_view(SidebarView::Files, false),
                        2 => {
                            if let Some(terminal) = ui.current_terminal() {
                                terminal.grab_focus();
                            }
                        }
                        3 => ui.execute_action(crate::keybindings::Action::ToggleTasksPanel),
                        _ => palette.emit_clicked(),
                    }
                }
            });
        }
        if index == 3 {
            button.set_sensitive(ui.config.borrow().agent_tasks_enabled);
            button.set_tooltip_text(Some("Agent Tasks — enable in Settings when unavailable"));
            gated_agent_button = Some(button.clone());
        }
        rail.append(&button);
    }
    // Shell and Vim remain capabilities, with their original wired controls.
    // They are never introduced as additional presentation modes.
    let capabilities = gtk4::MenuButton::new();
    capabilities.set_child(Some(&vector_icon(
        jterm_core::reference_icons::Icon::More,
        16,
    )));
    capabilities.set_always_show_arrow(false);
    capabilities.add_css_class("flat");
    capabilities.set_tooltip_text(Some("Terminal capabilities"));
    let popover = gtk4::Popover::new();
    popover.add_css_class("forge-popup");
    let capability_list = gtk4::Box::new(Orientation::Vertical, 6);
    vim.set_child(Some(&icon_label("code", "Vim workspace")));
    ui.agent_toggle
        .set_child(Some(&icon_label("agent", "Shell Agent")));
    let show_tools = gtk4::CheckButton::with_label("Show terminal tools");
    show_tools.add_css_class("reference-native-tools-toggle");
    show_tools.set_tooltip_text(Some(
        "Optional compact tools for native terminal input; no second composer",
    ));
    let footer_for_toggle = legacy_tools_footer.downgrade();
    let visible_for_toggle = legacy_tools_visible.clone();
    let ui_for_toggle = Rc::downgrade(ui);
    show_tools.connect_toggled(move |toggle| {
        visible_for_toggle.set(toggle.is_active());
        if let (Some(ui), Some(footer)) = (ui_for_toggle.upgrade(), footer_for_toggle.upgrade()) {
            let native_grid = native_grid_input(&ui);
            footer.set_visible(toggle.is_active() && native_grid);
        }
    });
    show_tools.connect_map(|toggle| {
        let native_grid =
            ui_for_widget(toggle.upcast_ref()).is_some_and(|ui| native_grid_input(&ui));
        toggle.set_sensitive(native_grid);
    });
    capability_list.append(&show_tools);
    capability_list.append(vim);
    capability_list.append(&ui.agent_toggle);
    popover.set_child(Some(&capability_list));
    capabilities.set_popover(Some(&popover));
    guard_tools_focus(&capabilities, &popover);
    capabilities.add_css_class("reference-header-action");
    capabilities.set_size_request(
        token::HEADER_ACTION_SIZE as i32,
        token::HEADER_ACTION_SIZE as i32,
    );
    capabilities.set_valign(gtk4::Align::Center);
    if let Some(placeholder) = widget_with_class_any(&header_root, "reference-header-more") {
        if let Some(actions) = placeholder.parent().and_downcast::<gtk4::Box>() {
            actions.insert_child_after(&capabilities, placeholder.prev_sibling().as_ref());
            actions.remove(&placeholder);
        }
    }
    let spacer = gtk4::Box::new(Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    rail.append(&spacer);
    let settings = icon("emblem-system-symbolic", "Settings");
    settings.set_child(Some(&vector_icon(
        jterm_core::reference_icons::Icon::Settings,
        18,
    )));
    let ui_for_settings = Rc::downgrade(ui);
    settings.connect_clicked(move |_| {
        if let Some(ui) = ui_for_settings.upgrade() {
            ui.execute_action(crate::keybindings::Action::ToggleSettings);
        }
    });
    rail.append(&settings);
    let initial = std::env::var("USER")
        .ok()
        .and_then(|name| name.chars().next())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "U".into());
    let profile = label(&initial, "reference-profile");
    profile.set_width_request(token::PROFILE_DIAMETER as i32);
    profile.set_height_request(token::PROFILE_DIAMETER as i32);
    profile.set_xalign(0.5);
    rail.append(&profile);
    main.remove(content);
    let body = gtk4::Box::new(Orientation::Horizontal, 0);
    body.set_vexpand(true);
    body.append(&rail);
    body.append(content);
    let frame = gtk4::Overlay::new();
    frame.set_child(Some(&body));
    frame.set_vexpand(true);
    let changed_ui = Rc::downgrade(ui);
    let reflow_ui = Rc::downgrade(ui);
    let sizing = super::sidebar_resize::SidebarResize::new(
        content,
        &ui.ai_paned,
        ui.config.clone(),
        move || {
            if let Some(ui) = changed_ui.upgrade() {
                ui.persist_config();
            }
        },
        move || {
            if let Some(ui) = reflow_ui.upgrade() {
                sync_inspector(&ui, false);
            }
        },
    );
    let resize_for_deactivate = Rc::downgrade(&sizing);
    ui.window.connect_is_active_notify(move |window| {
        if !window.is_active() {
            if let Some(sizing) = resize_for_deactivate.upgrade() {
                sizing.cancel_deferred();
            }
        }
    });
    let drawer_grip = sizing.drawer_grip(&ui.reference_inspector);
    frame.add_overlay(&drawer_grip);
    frame.set_measure_overlay(&drawer_grip, false);
    let inspector_chrome = Rc::new(InspectorChrome {
        requested: std::cell::Cell::new(
            (!jterm_core::reference_conversation::fixture_enabled()).then_some(false),
        ),
        actual: std::cell::Cell::new(false),
        button: inspector_toggle.downgrade(),
        frame: frame.downgrade(),
        sizing,
        drawer_grip: drawer_grip.downgrade(),
    });
    unsafe {
        ui.window
            .set_data(INSPECTOR_CHROME_KEY, Rc::downgrade(&inspector_chrome));
    }
    let chrome_for_click = inspector_chrome.clone();
    let ui_for_inspector = Rc::downgrade(ui);
    inspector_toggle.connect_clicked(move |_| {
        if let (Some(ui), Some(frame)) =
            (ui_for_inspector.upgrade(), chrome_for_click.frame.upgrade())
        {
            chrome_for_click
                .requested
                .set(Some(!chrome_for_click.actual.get()));
            chrome_for_click.sync(&ui, frame.width(), frame.height());
        }
    });
    let observer = gtk4::DrawingArea::new();
    observer.set_can_target(false);
    observer.set_focusable(false);
    observer.set_hexpand(true);
    observer.set_vexpand(true);
    frame.add_overlay(&observer);
    frame.set_measure_overlay(&observer, false);
    let ui_for_resize = Rc::downgrade(ui);
    let palette_for_resize = palette.clone();
    let path_for_resize = path.clone();
    let top_for_geometry = top.clone();
    let rail_for_geometry = rail.clone();
    let chrome_for_resize = inspector_chrome.clone();
    observer.connect_resize(move |_, width, height| {
        let Some(ui) = ui_for_resize.upgrade() else {
            return;
        };
        let (navigation_width, inspector_width) = chrome_for_resize.sizing.preferences();
        let layout = token::Layout::for_size_with_sidebars(width as f32, height as f32 + token::TITLEBAR_HEIGHT,
            if ui.config.borrow().sidebar_visible { navigation_width as f32 } else { 0.0 },
            if chrome_for_resize.requested.get() == Some(false) { 0.0 } else { inspector_width as f32 });
        path_for_resize.set_visible(layout.inspector.is_some());
        palette_for_resize.set_width_request(if layout.inspector.is_some() { token::SEARCH_WIDTH as i32 } else { 180 });
        ui.sidebar
            .set_visible(layout.navigation.is_some() && ui.config.borrow().sidebar_visible);
        chrome_for_resize.sync(&ui, width, height);
        if let Some(destination) = std::env::var_os("FORGE_REFERENCE_GEOMETRY") {
            let ui = Rc::downgrade(&ui);
            let top_for_geometry = top_for_geometry.clone();
            let rail_for_geometry = rail_for_geometry.clone();
            glib::idle_add_local_once(move || {
            let Some(ui) = ui.upgrade() else { return; };
            let bounds = |widget: &gtk4::Widget| widget.compute_bounds(&ui.window).map(|r| serde_json::json!({"x":r.x(),"y":r.y(),"width":r.width(),"height":r.height()}));
            let child_bounds = |class: &str| {
                let mut measured = Vec::new();
                let mut child = widget_with_class(ui.window.upcast_ref(), class).and_then(|widget|widget.first_child());
                while let Some(widget) = child {
                    let button = widget.downcast_ref::<gtk4::Button>();
                    let text = widget.downcast_ref::<gtk4::Label>().map(|label|label.text().to_string())
                        .or_else(||button.and_then(|button|button.label()).map(|value|value.to_string()));
                    let content = button.and_then(|button|button.child());
                    measured.push(serde_json::json!({"bounds":bounds(&widget),"label":text,"visible":widget.is_visible(),"mapped":widget.is_mapped(),"opacity":widget.opacity(),"color":widget.color().to_string(),"content_bounds":content.as_ref().and_then(bounds),"content_color":content.as_ref().map(|widget|widget.color().to_string()),"classes":widget.css_classes().iter().map(|value|value.as_str()).collect::<Vec<_>>()}));
                    child = widget.next_sibling();
                }
                measured
            };
            let composer_buttons = || {
                let mut measured = Vec::new();
                let mut pending = widget_with_class(ui.window.upcast_ref(), "reference-composer-controls").into_iter().collect::<Vec<_>>();
                while let Some(widget) = pending.pop() {
                    let mut child = widget.first_child();
                    while let Some(next) = child {
                        child = next.next_sibling();
                        pending.push(next);
                    }
                    let Some(button) = widget.downcast_ref::<gtk4::Button>() else { continue; };
                    let mut captions = Vec::new();
                    let mut content = button.first_child().into_iter().collect::<Vec<_>>();
                    while let Some(widget) = content.pop() {
                        if let Some(label) = widget.downcast_ref::<gtk4::Label>() { captions.push(label.text().to_string()); }
                        let mut child = widget.first_child();
                        while let Some(next) = child { child = next.next_sibling(); content.push(next); }
                    }
                    let capsule = widget.parent().filter(|parent| parent.has_css_class("reference-sized-control")).unwrap_or_else(|| widget.clone());
                    measured.push(serde_json::json!({"capsule_bounds":bounds(&capsule),"bounds":bounds(&widget),"captions":captions,"tooltip":button.tooltip_text().map(|value|value.to_string()),"classes":button.css_classes().iter().map(|value|value.as_str()).collect::<Vec<_>>(),"mapped":button.is_mapped(),"sensitive":button.is_sensitive(),"focused":button.has_focus()}));
                }
                measured
            };
            let mut snapshot = serde_json::json!({
                "composer_buttons":composer_buttons(),
                "logical_window": {"width":ui.window.width(),"height":ui.window.height()},
                "native_surface":native_surface_geometry(&ui),
                "dpr":ui.window.scale_factor(),
                "window_active":ui.window.is_active(),
                "terminal_has_focus":ui.current_terminal().map(|terminal| terminal.has_focus()),
                "prompt_status":ui.current_term_view().map(|view| format!("{:?}", view.command_prompt_status())),
                "presentation":active_mode(&ui).map(|mode| mode.presentation_label()),
                "titlebar":bounds(top_for_geometry.upcast_ref()),
                "center_header":widget_with_class(ui.window.upcast_ref(), "reference-canvas-header").and_then(|w| bounds(&w)),
                "search":widget_with_class(ui.window.upcast_ref(), "reference-search").and_then(|w| bounds(&w)),
                "mode_controls":widget_with_class(ui.window.upcast_ref(), "reference-mode-switch").and_then(|w| bounds(&w)),
                "mode_buttons":child_bounds("reference-mode-switch"),
                "rail":bounds(rail_for_geometry.upcast_ref()),
                "navigation":bounds(ui.sidebar.upcast_ref()),
                "inspector":bounds(ui.reference_inspector.upcast_ref()),
                "inspector_tabs":widget_with_class(ui.window.upcast_ref(), "reference-inspector-tabs").and_then(|w| bounds(&w)),
                "inspector_tab_children":child_bounds("reference-inspector-tabs"),
                "inspector_stack":{"visible_name":ui.side_stack.visible_child_name().map(|value|value.to_string()),"transition_running":ui.side_stack.is_transition_running(),"bounds":bounds(ui.side_stack.upcast_ref())},
                "inspector_card":widget_with_class(ui.window.upcast_ref(), "reference-agent-card").and_then(|w| bounds(&w)),
                "center":ui.notebook.parent().and_then(|p| bounds(&p)),
                "navigation_status":widget_with_class(ui.window.upcast_ref(), "reference-navigation-status").and_then(|w| bounds(&w)),
                "navigation_viewport":widget_with_class(ui.window.upcast_ref(), "reference-navigation-viewport").and_then(|w| bounds(&w)),
                "navigation_document":widget_with_class(ui.window.upcast_ref(), "reference-navigation-document").and_then(|w| bounds(&w)),
                "spaces":widget_with_class(ui.window.upcast_ref(), "reference-spaces").and_then(|w| bounds(&w)),
                "inspector_progress":widget_with_class(ui.window.upcast_ref(), "reference-inspector-progress").and_then(|w| bounds(&w)),
                "inspector_agent_heading":widget_with_class(ui.window.upcast_ref(), "reference-inspector-agent-heading").and_then(|w| bounds(&w)),
                "inspector_metadata":widget_with_class(ui.window.upcast_ref(), "reference-inspector-metadata").and_then(|w| bounds(&w)),
                "inspector_metadata_cells":child_bounds("reference-inspector-metadata"),
                "inspector_current_heading":widget_with_class(ui.window.upcast_ref(), "reference-inspector-current-heading").and_then(|w| bounds(&w)),
                "inspector_current":widget_with_class(ui.window.upcast_ref(), "reference-inspector-current").and_then(|w| bounds(&w)),
                "inspector_queue_heading":widget_with_class(ui.window.upcast_ref(), "reference-inspector-queue-heading").and_then(|w| bounds(&w)),
                "inspector_queue":widget_with_class(ui.window.upcast_ref(), "reference-inspector-queue").and_then(|w| bounds(&w)),
                "inspector_queue_rows":child_bounds("reference-inspector-queue"),
                "inspector_output_heading":widget_with_class(ui.window.upcast_ref(), "reference-inspector-output-heading").and_then(|w| bounds(&w)),
                "inspector_terminal_section":widget_with_class(ui.window.upcast_ref(), "reference-inspector-terminal-section").and_then(|w| bounds(&w)),
                "inspector_output":widget_with_class(ui.window.upcast_ref(), "reference-inspector-output").and_then(|w| bounds(&w)),
                "tools":widget_with_class(ui.window.upcast_ref(), "reference-tools").and_then(|w| bounds(&w)),
                "composer":widget_with_class(ui.window.upcast_ref(), "chat-dock").or_else(|| widget_with_class(ui.window.upcast_ref(), "reference-block-composer")).and_then(|w| bounds(&w)),
            });
            snapshot["active_backend"] = serde_json::json!(active_mode(&ui).map(|mode| mode.as_str()));
            snapshot["inspector_visible"] = serde_json::json!(ui.reference_inspector.is_mapped());
            snapshot["inspector_drawer"] = serde_json::json!(ui.reference_inspector.has_css_class("reference-inspector-drawer"));
            snapshot["inspector_toggle"] = widget_with_class_any(ui.window.upcast_ref(), "reference-inspector-toggle").map(|w| serde_json::json!({"bounds":bounds(&w),"tooltip":w.tooltip_text().map(|v|v.to_string()),"mapped":w.is_mapped(),"active":w.downcast_ref::<gtk4::ToggleButton>().map(|b|b.is_active())})).unwrap_or(serde_json::Value::Null);
            snapshot["native_grid_input"] = serde_json::json!(native_grid_input(&ui));
            snapshot["legacy_tools_visible"] = serde_json::json!(widget_with_class_any(ui.window.upcast_ref(), "reference-native-tools").is_some_and(|w| w.is_mapped()));
            if let Ok(bytes) = serde_json::to_vec_pretty(&snapshot) { let _ = std::fs::write(destination, bytes); }
            });
        }
    });
    main.append(&frame);
    // Keep the reference navigation arrangement while retaining the legacy
    // persisted enum for older sessions and restoration code.
    ui.tab_placement.set(TabPlacement::Sidebar);
    ui.apply_tab_placement();
    ui.apply_sidebar_view(SidebarView::Tabs, false);
    // The selected presentation belongs to the active pane, not to the
    // preference used by a later tab. Resolve it after Notebook's switch
    // handler and after split-pane focus changes; never re-create a live PTY.
    let ui_for_presentation = Rc::downgrade(ui);
    let footer_for_presentation = legacy_tools_footer.downgrade();
    let refresh_presentation: Rc<dyn Fn()> = Rc::new(move || {
        let Some(ui) = ui_for_presentation.upgrade() else {
            return;
        };
        let Some(mode) = active_mode(&ui) else { return };
        let chat = mode == TerminalMode::Chat;
        for (class, selected) in [
            ("reference-mode-chat", chat),
            ("reference-mode-block", !chat),
        ] {
            if let Some(button) = widget_with_class_any(ui.window.upcast_ref(), class)
                .and_then(|w| w.downcast::<gtk4::ToggleButton>().ok())
            {
                button.set_active(selected);
            }
        }
        if let Some(button) =
            widget_with_class_any(ui.window.upcast_ref(), "reference-navigation-active")
        {
            if let Some(title) = widget_with_class_any(&button, "reference-icon-label")
                .and_then(|w| w.downcast::<gtk4::Label>().ok())
            {
                title.set_text(mode.presentation_label());
            }
        }
        if let Some(center) = ui.notebook.parent() {
            if mode == TerminalMode::Block {
                center.add_css_class("reference-developer-active");
            } else {
                center.remove_css_class("reference-developer-active");
            }
        }
        let native_grid = native_grid_input(&ui);
        if let Some(footer) = footer_for_presentation.upgrade() {
            footer.set_visible(native_grid && legacy_tools_visible.get());
        }
        ui.reference_inspector.queue_allocate();
    });
    let refresh = refresh_presentation.clone();
    ui.notebook.connect_switch_page(move |_, _, _| {
        let refresh = refresh.clone();
        glib::idle_add_local_once(move || refresh());
    });
    let refresh = refresh_presentation.clone();
    ui.window.connect_focus_widget_notify(move |_| {
        let refresh = refresh.clone();
        glib::idle_add_local_once(move || refresh());
    });
    glib::idle_add_local_once(move || refresh_presentation());
    let context_path = context_label(&ui.side_stack);
    let ui_for_context = Rc::downgrade(ui);
    let refresh_context: Rc<dyn Fn()> = Rc::new(move || {
        let Some(ui) = ui_for_context.upgrade() else {
            return;
        };
        if let Some(button) = &gated_agent_button {
            button.set_sensitive(ui.config.borrow().agent_tasks_enabled);
        }
        refresh_task_overview(&ui);
        if let Some(terminal) = ui.current_terminal() {
            let private = private_session(&ui);
            let remote = ui.current_pane_leaf().is_some_and(|leaf| leaf.is_remote());
            let directory = session_directory(&ui);
            let cwd = if private {
                "Private session".to_string()
            } else {
                directory
                    .clone()
                    .unwrap_or_else(|| "Directory not reported".into())
            };
            let safe = jterm_core::review_input::safe_inline_display(&cwd, 1024);
            let name = if private {
                "Private session".to_string()
            } else {
                std::path::Path::new(&cwd)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .map(|value| jterm_core::review_input::safe_inline_display(value, 128))
                    .unwrap_or_else(|| "Local workspace".into())
            };
            workspace_name.set_text(&name);
            workspace_name.set_tooltip_text(Some(&safe));
            let branch = if private || remote || directory.is_none() {
                None
            } else {
                crate::block_view::reference_workspace_branch(&cwd)
            };
            workspace_branch.set_text(branch.as_deref().unwrap_or("—"));
            workspace_branch.set_tooltip_text(branch.as_deref());
            path.set_text(&safe);
            path.set_tooltip_text(Some(&safe));
            if let Some(context_path) = &context_path {
                context_path.set_text(&safe);
                context_path.set_tooltip_text(Some(&safe));
            }
            let title = ui
                .notebook
                .current_page()
                .and_then(|index| ui.notebook.nth_page(Some(index)))
                .and_then(|page| super::tab_display_title(&ui.notebook, &page))
                .unwrap_or_else(|| "Terminal".into());
            set_context_value(&ui.side_stack, "session", &title);
            let connection = if private {
                "Hidden for private session".into()
            } else if remote {
                ui.current_pane_leaf()
                    .and_then(|leaf| leaf.managed_remote_name())
                    .unwrap_or_else(|| "Remote terminal".into())
            } else {
                "Local terminal".into()
            };
            set_context_value(&ui.side_stack, "connection", &connection);
            set_context_value(
                &ui.side_stack,
                "branch",
                if private {
                    "Hidden for private session"
                } else if remote {
                    "Available in the remote terminal"
                } else {
                    branch.as_deref().unwrap_or("Not a Git workspace")
                },
            );
            set_context_value(
                &ui.side_stack,
                "mode",
                active_mode(&ui)
                    .as_ref()
                    .map(TerminalMode::presentation_label)
                    .unwrap_or("Terminal"),
            );
            let process = if private {
                "Hidden for private session".into()
            } else {
                ui.current_pane_leaf()
                    .and_then(|leaf| leaf.foreground_process_name())
                    .unwrap_or_else(|| "Not reported".into())
            };
            set_context_value(&ui.side_stack, "process", &process);
            set_context_value(
                &ui.side_stack,
                "prompt",
                if private {
                    "Hidden for private session"
                } else {
                    ui.current_term_view()
                        .as_ref()
                        .map(|view| view.command_prompt_status().short_label())
                        .unwrap_or("Native terminal input")
                },
            );
            set_context_value(
                &ui.side_stack,
                "size",
                &format!(
                    "{} columns × {} rows",
                    terminal.column_count(),
                    terminal.row_count()
                ),
            );
            let config = ui.config.borrow();
            set_context_value(
                &ui.side_stack,
                "sharing",
                if !config.ai_enabled {
                    "AI disabled"
                } else if config.ai_share_command_context {
                    if config.ai_redact_secrets {
                        "Enabled · secret redaction on"
                    } else {
                        "Enabled · secret redaction off"
                    }
                } else {
                    "Command context sharing disabled"
                },
            );
            drop(config);
            if let Some(page) = ui.side_stack.child_by_name("context") {
                for action in ["copy", "browse"] {
                    if let Some(button) =
                        widget_with_class_any(&page, &format!("reference-context-{action}"))
                    {
                        button.set_sensitive(!private && directory.is_some());
                    }
                }
            }
        } else {
            workspace_name.set_text("No terminal selected");
            workspace_name.set_tooltip_text(None);
            workspace_branch.set_text("—");
            workspace_branch.set_tooltip_text(None);
            path.set_text("No terminal selected");
            if let Some(context_path) = &context_path {
                context_path.set_text("No terminal selected");
                context_path.set_tooltip_text(None);
            }
            for key in [
                "session",
                "connection",
                "branch",
                "mode",
                "process",
                "prompt",
                "size",
            ] {
                set_context_value(&ui.side_stack, key, "No terminal selected");
            }
            if let Some(page) = ui.side_stack.child_by_name("context") {
                for action in ["copy", "browse"] {
                    if let Some(button) =
                        widget_with_class_any(&page, &format!("reference-context-{action}"))
                    {
                        button.set_sensitive(false);
                    }
                }
            }
        }
    });
    let refresh_for_switch = refresh_context.clone();
    ui.notebook.connect_switch_page(move |_, _, _| {
        let refresh = refresh_for_switch.clone();
        glib::idle_add_local_once(move || refresh());
    });
    let refresh_for_title = refresh_context.clone();
    ui.window.connect_title_notify(move |_| refresh_for_title());
    let refresh_for_initial = refresh_context.clone();
    glib::idle_add_local_once(move || refresh_for_initial());
    let ui_for_poll = Rc::downgrade(ui);
    glib::timeout_add_seconds_local(1, move || {
        if ui_for_poll.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        refresh_context();
        glib::ControlFlow::Continue
    });
    if let Some(trace) = std::env::var_os("FORGE_REFERENCE_KEY_TRACE") {
        let capture = gtk4::EventControllerKey::new();
        capture.set_propagation_phase(gtk4::PropagationPhase::Capture);
        let weak = Rc::downgrade(ui);
        capture.connect_key_pressed(move |_, key, _, _| {
            if let Some(ui) = weak.upgrade() {
                use std::io::Write;
                if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&trace) {
                    let event = serde_json::json!({"key":key.to_unicode().map(|c| c.to_string()), "window_active":ui.window.is_active(), "terminal_has_focus":ui.current_terminal().map(|terminal| terminal.has_focus()), "prompt_status":ui.current_term_view().map(|view| format!("{:?}", view.command_prompt_status()))});
                    let _ = writeln!(file, "{event}");
                }
            }
            glib::Propagation::Proceed
        });
        ui.window.add_controller(capture);
    }
    if let Some(path) = std::env::var_os("FORGE_REFERENCE_STATE_TRACE") {
        let weak = Rc::downgrade(ui);
        let clocks: Rc<
            std::cell::RefCell<
                Option<(
                    gtk4::gdk::FrameClock,
                    glib::SignalHandlerId,
                    glib::SignalHandlerId,
                )>,
            >,
        > = Rc::new(std::cell::RefCell::new(None));
        let state = clocks.clone();
        ui.window.connect_map(move |window| {
            let Some(clock) = window.frame_clock() else {
                return;
            };
            let weak = weak.clone();
            let path = path.clone();
            let paint = clock.connect_after_paint(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if let Some(view) = ui.current_term_view() {
                        use std::io::Write;
                        if let Ok(mut file) = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&path)
                        {
                            let mut snapshot = view.reference_viewport_state();
                            let bounds = |class| widget_with_class(ui.window.upcast_ref(), class).and_then(|widget|widget.compute_bounds(&ui.window)).map(|r|serde_json::json!({"x":r.x(),"y":r.y(),"width":r.width(),"height":r.height()}));
                            snapshot["organism"] = bounds("reference-organism-surface").unwrap_or(serde_json::Value::Null);
                            snapshot["speech"] = bounds("reference-organism-speech").unwrap_or(serde_json::Value::Null);
                            snapshot["speech_dismiss"] = bounds("reference-organism-dismiss").unwrap_or(serde_json::Value::Null);
                            snapshot["tools_open"] = serde_json::json!(widget_with_class(ui.window.upcast_ref(), "reference-tools-popover").is_some());
                            snapshot["focused_widget"] = serde_json::json!(gtk4::prelude::RootExt::focus(&ui.window).map(|widget|widget.widget_name().to_string()));
                            snapshot["reference_role_fonts"] = reference_role_fonts(&ui);
                            let toggle = widget_with_class_any(ui.window.upcast_ref(), "reference-inspector-toggle")
                                .and_then(|widget| widget.downcast::<gtk4::ToggleButton>().ok());
                            snapshot["reference_inspector"] = serde_json::json!({
                                "visible":ui.reference_inspector.is_mapped(),
                                "drawer":ui.reference_inspector.has_css_class("reference-inspector-drawer"),
                                "bounds":ui.reference_inspector.compute_bounds(&ui.window).map(|r|[r.x(),r.y(),r.width(),r.height()]),
                                "selected_page":ui.side_stack.visible_child_name().map(|v|v.to_string()),
                                "focus_in_panel":gtk4::prelude::RootExt::focus(&ui.window).is_some_and(|w|w == *ui.reference_inspector.upcast_ref::<gtk4::Widget>() || w.is_ancestor(&ui.reference_inspector)),
                                "focused_object":gtk4::prelude::RootExt::focus(&ui.window).map(|w|format!("{:p}",w.as_ptr())),
                                "focused_type":gtk4::prelude::RootExt::focus(&ui.window).map(|w|w.type_().name().to_string()),
                                "center":ui.notebook.parent().and_then(|w|w.compute_bounds(&ui.window)).map(|r|[r.x(),r.y(),r.width(),r.height()]),
                                "toggle":toggle.map(|button|serde_json::json!({"bounds":button.compute_bounds(&ui.window).map(|r|[r.x(),r.y(),r.width(),r.height()]),"tooltip":button.tooltip_text().map(|v|v.to_string()),"active":button.is_active(),"sensitive":button.is_sensitive()})),
                            });
                            snapshot["native_surface"] = native_surface_geometry(&ui);
                            snapshot["navigation_scroll"] = widget_with_class(ui.window.upcast_ref(), "reference-navigation-viewport")
                                .and_then(|widget| widget.downcast::<gtk4::ScrolledWindow>().ok())
                                .map(|scroll| {
                                    let adjustment = scroll.vadjustment();
                                    serde_json::json!({"adjustment":[adjustment.lower(),adjustment.value(),adjustment.upper(),adjustment.page_size()],
                                        "viewport":bounds("reference-navigation-viewport"),"document":bounds("reference-navigation-document"),
                                        "spaces":bounds("reference-spaces"),"status":bounds("reference-navigation-status")})
                                }).unwrap_or(serde_json::Value::Null);
                            let _ = writeln!(file, "{snapshot}");
                        }
                    }
                }
            });
            let redraw = window.downgrade();
            let update = clock.connect_update(move |_| {
                if let Some(window) = redraw.upgrade() {
                    window.queue_draw();
                }
            });
            clock.begin_updating();
            *state.borrow_mut() = Some((clock, paint, update));
        });
        ui.window.connect_unmap(move |_| {
            if let Some((clock, paint, update)) = clocks.borrow_mut().take() {
                clock.end_updating();
                clock.disconnect(paint);
                clock.disconnect(update);
            }
        });
    }
    install_css();
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &ui.reference_font_css,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 21,
        );
    }
}

/// Opt in only application-owned workspace dialogs; native file choosers keep
/// their platform styling. Preserve the dialog's existing native text sizing.
pub(crate) fn style_workspace_dialog(dialog: &impl IsA<gtk4::Widget>) {
    dialog.add_css_class("forge-workspace-dialog");
}

fn shell_css() -> String {
    fn hex(color: [u8; 3]) -> String {
        format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
    }
    let css = format!(
        "{}\n{}\n{}\n{}",
        include_str!("reference_shell.css"),
        include_str!("developer_block.css"),
        include_str!("reference_popovers.css"),
        include_str!("reference_dialogs.css")
    )
    .replace("@TITLEBAR@", &hex(token::color::TITLEBAR))
    .replace("@RAIL@", &hex(token::color::RAIL))
    .replace("@NAVIGATION@", &hex(token::color::NAVIGATION))
    .replace("@INSPECTOR@", &hex(token::color::INSPECTOR))
    .replace("@INSPECTOR_CARD@", &hex(token::color::INSPECTOR_CARD))
    .replace("@CANVAS@", &hex(token::color::CANVAS))
    .replace("@CANVAS_COOL@", &hex(token::color::CANVAS_COOL))
    .replace("@ACCENT@", &hex(token::color::ACCENT))
    .replace(
        "@PROFILE_BACKGROUND@",
        &hex(token::color::PROFILE_BACKGROUND),
    )
    .replace("@PROFILE_BORDER@", &hex(token::color::PROFILE_BORDER))
    .replace("@DARK_BORDER@", &hex(token::color::DARK_BORDER))
    .replace("@POPUP_SURFACE@", &hex(token::color::NAVIGATION))
    .replace("@POPUP_BORDER@", &{
        let [r, g, b, _] = jterm_core::reference_popup::BORDER;
        hex([r, g, b])
    })
    .replace("@POPUP_FOREGROUND@", &hex(token::color::CHROME_TEXT))
    .replace("@POPUP_MUTED@", &hex(token::color::CHROME_MUTED))
    .replace("@POPUP_SELECTED@", &hex(token::color::NAVIGATION_SELECTED));
    // GTK CSS minima exclude the surrounding padding/border. Keep those
    // insets explicit while binding the total measured geometry to the same
    // native contract used by both Block and Chat widget requests.
    [
        ("@TITLEBAR_HEIGHT@", token::TITLEBAR_HEIGHT),
        ("@SEARCH_INNER_HEIGHT@", token::SEARCH_HEIGHT - 2.0),
        ("@MODE_HEIGHT@", token::MODE_CONTROL_HEIGHT),
        ("@MODE_MIN_WIDTH@", token::MODE_CONTROL_MIN_WIDTH),
        (
            "@RAIL_PADDING_X@",
            (token::RAIL_WIDTH - token::SELECTED_RAIL_SIZE - 1.0) / 2.0,
        ),
        (
            "@NAVIGATION_CONTENT_MIN@",
            token::MIN_NAVIGATION_WIDTH - 1.0 - 2.0 * token::NAVIGATION_PADDING,
        ),
        ("@HEADER_INNER_HEIGHT@", token::CANVAS_HEADER_HEIGHT - 1.0),
        ("@INSPECTOR_CONTENT_MIN@", token::MIN_INSPECTOR_WIDTH - 1.0),
        (
            "@INSPECTOR_TABS_INNER_HEIGHT@",
            token::INSPECTOR_TABS_HEIGHT - 1.0,
        ),
        ("@INSPECTOR_CARD_MARGIN@", token::INSPECTOR_INSET - 1.0),
        ("@COMPOSER_INSET@", token::COMPOSER_INSET),
        ("@COMPOSER_BOTTOM@", token::COMPOSER_BOTTOM),
        ("@COMPOSER_INNER_HEIGHT@", token::COMPOSER_HEIGHT - 2.0),
        (
            "@LEGACY_COMPOSER_INNER_HEIGHT@",
            token::COMPOSER_HEIGHT - token::COMPOSER_CONTROL_BOTTOM - 1.0,
        ),
        (
            "@LEGACY_CONTROL_BOTTOM_PADDING@",
            token::COMPOSER_CONTROL_BOTTOM - 1.0,
        ),
        ("@BLOCK_DOCK_INNER_HEIGHT@", token::COMPOSER_HEIGHT - 16.0),
        ("@CARD_INSET@", token::CARD_HORIZONTAL_INSET),
    ]
    .into_iter()
    .fold(css, |css, (name, value)| {
        css.replace(name, &value.to_string())
    })
}

pub(crate) fn install_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(&shell_css());
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION + 20,
        );
    }
}

#[cfg(test)]
mod review_target_tests {
    use super::{review_task_target, ObservedInspector};
    use crate::agent_task::{AgentProvider, NewTask, TaskManager, TaskStatus};

    fn tasks() -> Vec<crate::agent_task::AgentTask> {
        let mut manager = TaskManager::new();
        for name in ["displayed", "newer"] {
            manager
                .create(NewTask {
                    title: name.into(),
                    provider: AgentProvider::Codex,
                    repo_root: "/repo".into(),
                    worktree_path: format!("/tasks/{name}").into(),
                    branch: format!("task/{name}"),
                    base_commit: "a".repeat(40),
                    source_context: None,
                })
                .unwrap();
        }
        manager.tasks().to_vec()
    }

    #[test]
    fn review_keeps_displayed_identity_when_another_task_needs_attention() {
        let mut tasks = tasks();
        let displayed = tasks[0].id;
        tasks[1].status = TaskStatus::WaitingForApproval;
        assert_eq!(review_task_target(&tasks, Some(displayed)), Some(displayed));
        tasks.remove(0);
        assert_eq!(review_task_target(&tasks, Some(displayed)), None);
        assert_eq!(review_task_target(&tasks, None), None);
    }

    #[test]
    fn archived_displayed_task_cannot_redirect_review_to_another_task() {
        let mut tasks = tasks();
        let displayed = tasks[0].id;
        tasks[0].status = TaskStatus::Archived;
        assert_eq!(review_task_target(&tasks, Some(displayed)), None);
    }

    #[test]
    fn equal_visible_models_with_different_task_ids_refresh_the_action_target() {
        let tasks = tasks();
        let model = jterm_core::reference_conversation::Inspector::empty();
        let first = ObservedInspector {
            task: Some(tasks[0].id),
            model: model.clone(),
        };
        let second = ObservedInspector {
            task: Some(tasks[1].id),
            model,
        };
        assert!(first != second);
    }
}

#[cfg(test)]
mod context_directory_tests {
    use super::context_directory;

    #[test]
    fn private_context_never_reads_local_or_remote_directory() {
        for remote in [false, true] {
            assert_eq!(
                context_directory(true, remote, Some("file://remote/private"), || {
                    panic!("private context must not query the process directory")
                }),
                None
            );
        }
    }

    #[test]
    fn remote_context_requires_an_absolute_report_and_never_uses_local_process_cwd() {
        let never_local = || panic!("SSH process cwd belongs to this machine");
        assert_eq!(
            context_directory(
                false,
                true,
                Some("file://remote.example/srv/project%20one"),
                never_local
            ),
            Some("/srv/project one".into())
        );
        assert_eq!(context_directory(false, true, None, never_local), None);
        assert_eq!(
            context_directory(
                false,
                true,
                Some("https://remote.example/project"),
                never_local
            ),
            None
        );
        assert_eq!(
            context_directory(false, true, Some("file:relative"), never_local),
            None
        );
    }

    #[test]
    fn local_context_uses_local_namespace_even_when_remote_metadata_is_present() {
        assert_eq!(
            context_directory(
                false,
                false,
                Some("file://remote.example/srv/remote"),
                || Some("/home/local".into())
            ),
            Some("/home/local".into())
        );
    }
}

#[cfg(test)]
mod role_font_tests {
    use super::role_font_css;

    #[test]
    fn reference_role_fonts_keep_hierarchy_and_disjoint_groups() {
        let normal = role_font_css(1.0, 1.0);
        let ui = role_font_css(1.3, 1.0);
        let content = role_font_css(1.0, 1.3);
        for (selector, size, ui_size) in [
            (".reference-shell .reference-canvas-title", 12.0, 15.6),
            (
                "window.forge-window.reference-shell .reference-rich-inspector label.reference-card-title",
                11.0,
                14.3,
            ),
            (
                "window.forge-window.reference-shell .reference-rich-inspector label",
                9.0,
                11.7,
            ),
            (
                "window.forge-window.reference-shell .reference-toolbar label",
                10.0,
                13.0,
            ),
            (
                "window.forge-window.reference-shell .reference-composer-controls.reference-fixed-controls label",
                10.0,
                13.0,
            ),
        ] {
            let declaration = |size: f64| format!("{selector} {{ font-size: {size:.6}px; }}");
            assert!(normal.contains(&declaration(size)), "{selector}");
            assert!(ui.contains(&declaration(ui_size)), "UI: {selector}");
            assert!(content.contains(&declaration(size)), "Content leaves UI: {selector}");
        }
        for (selector, size) in [
            ("label.reference-message-heading", 20.0),
            ("label.reference-message-paragraph", 12.0),
            ("label.reference-message-author", 12.0),
            ("label.reference-chip-text", 9.0),
            (".reference-rich-step label", 10.0),
            (".reference-code-header label", 10.0),
            ("label.reference-code-line", 10.0),
            ("label.reference-line-number", 10.0),
        ] {
            let selector = format!(
                "window.forge-window.reference-shell .reference-rich-conversation {selector}"
            );
            let declaration =
                |scale: f64| format!("{selector} {{ font-size: {:.6}px; }}", size * scale);
            assert!(normal.contains(&declaration(1.0)), "{selector}");
            assert!(
                ui.contains(&declaration(1.0)),
                "UI leaves Content: {selector}"
            );
            assert!(content.contains(&declaration(1.3)), "Content: {selector}");
        }
        assert_eq!(role_font_css(f64::NAN, -1.0), normal);
        assert!(!ui.contains(".chat-controls"));
        assert!(!ui.contains("margin:"));
        assert!(!ui.contains("padding:"));
    }
}

#[cfg(test)]
mod popup_theme_tests {
    use gtk4 as gtk;
    use libadwaita as adw;
    use libadwaita::prelude::*;

    fn settle() {
        let context = gtk::glib::MainContext::default();
        let until = std::time::Instant::now() + std::time::Duration::from_millis(120);
        while std::time::Instant::now() < until {
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn assert_color(actual: gtk::gdk::RGBA, expected: [u8; 3]) {
        for (actual, expected) in [actual.red(), actual.green(), actual.blue()]
            .into_iter()
            .zip(expected)
        {
            assert!((actual * 255.0 - f32::from(expected)).abs() < 1.0);
        }
    }

    fn matching_pixels(widget: &impl IsA<gtk::Widget>, rgb: [u8; 3]) -> usize {
        let widget = widget.upcast_ref::<gtk::Widget>();
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(widget)).snapshot(
            &snapshot,
            f64::from(widget.width()),
            f64::from(widget.height()),
        );
        let node = snapshot.to_node().expect("mapped popup snapshot");
        let surface = widget.native().unwrap().surface().unwrap();
        let renderer = gtk::gsk::Renderer::for_surface(&surface).unwrap();
        let texture = renderer.render_texture(&node, None);
        let stride = texture.width() as usize * 4;
        let mut pixels = vec![0; stride * texture.height() as usize];
        texture.download(&mut pixels, stride);
        renderer.unrealize();
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| {
                let argb = u32::from_ne_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
                [
                    ((argb >> 16) & 255) as u8,
                    ((argb >> 8) & 255) as u8,
                    (argb & 255) as u8,
                ] == rgb
            })
            .count()
    }

    fn modal_corner_alpha(widget: &impl IsA<gtk::Widget>) -> u8 {
        let widget = widget.upcast_ref::<gtk::Widget>();
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(widget)).snapshot(
            &snapshot,
            f64::from(widget.width()),
            f64::from(widget.height()),
        );
        let node = snapshot.to_node().expect("mapped dialog snapshot");
        let surface = widget.native().unwrap().surface().unwrap();
        let renderer = gtk::gsk::Renderer::for_surface(&surface).unwrap();
        let texture = renderer.render_texture(&node, None);
        let stride = texture.width() as usize * 4;
        let mut pixels = vec![0; stride * texture.height() as usize];
        texture.download(&mut pixels, stride);
        renderer.unrealize();
        let argb = u32::from_ne_bytes([pixels[0], pixels[1], pixels[2], pixels[3]]);
        (argb >> 24) as u8
    }

    fn find_popover(widget: &gtk::Widget) -> Option<gtk::Popover> {
        if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
            return Some(popover.clone());
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(popover) = find_popover(&current) {
                return Some(popover);
            }
            child = current.next_sibling();
        }
        None
    }

    #[test]
    #[ignore = "requires DISPLAY; run on its own GTK main thread"]
    fn reference_popups_follow_shell_chrome_across_terminal_themes() {
        gtk::init().expect("GTK display");
        struct AnimationPreference(gtk::Settings, bool);
        impl Drop for AnimationPreference {
            fn drop(&mut self) {
                self.0.set_gtk_enable_animations(self.1);
            }
        }
        let settings = gtk::Settings::default().unwrap();
        let _animations =
            AnimationPreference(settings.clone(), settings.is_gtk_enable_animations());
        settings.set_gtk_enable_animations(false);
        let display = gtk::gdk::Display::default().unwrap();
        let shell = gtk::CssProvider::new();
        let parsing_errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let errors = parsing_errors.clone();
        shell.connect_parsing_error(move |_, _, error| {
            errors.borrow_mut().push(error.to_string());
        });
        shell.load_from_string(&super::shell_css());
        gtk::style_context_add_provider_for_display(
            &display,
            &shell,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 20,
        );
        let theme = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &theme,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        let fonts = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &fonts,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 21,
        );
        let window = gtk::Window::new();
        window.add_css_class("forge-window");
        window.add_css_class("reference-shell");
        window.set_default_size(480, 360);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.add_css_class("reference-navigation");
        let anchor = gtk::MenuButton::new();
        anchor.set_label("Menu");
        let popup = gtk::Popover::new();
        popup.add_css_class("forge-popup");
        popup.add_css_class("terminal-context-menu");
        popup.set_has_arrow(false);
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
        menu.set_width_request(220);
        let action = gtk::Button::with_label("Rename tab");
        let unavailable = gtk::Button::with_label("Unavailable action");
        unavailable.add_css_class("destructive-action");
        unavailable.set_sensitive(false);
        menu.append(&action);
        menu.append(&unavailable);
        popup.set_child(Some(&menu));
        anchor.set_popover(Some(&popup));
        content.append(&anchor);
        // This menu is created by GTK, not by one of our Popover builders.
        let dropdown = gtk::DropDown::from_strings(&["Current workspace", "Other workspace"]);
        content.append(&dropdown);
        window.set_child(Some(&content));
        window.present();
        anchor.popup();
        settle();
        let action_label = action.child().unwrap().downcast::<gtk::Label>().unwrap();
        let disabled_label = unavailable
            .child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        for (background, foreground, font, ui_scale) in [
            ("#16161e", "#c0caf5", "Monospace Bold Italic 18px", 1.0),
            ("#f6f8fa", "#24292f", "Sans Italic 22px", 1.3),
            ("#221122", "#ffddee", "Monospace Bold 16px", 1.0),
        ] {
            theme.load_from_string(&crate::font::menu_theme_css(
                font,
                &gtk::gdk::RGBA::parse(background).unwrap(),
                &gtk::gdk::RGBA::parse(foreground).unwrap(),
            ));
            fonts.load_from_string(&super::role_font_css(ui_scale, 1.0));
            settle();
            assert_color(action_label.color(), super::token::color::CHROME_TEXT);
            assert_color(disabled_label.color(), super::token::color::CHROME_MUTED);
            let actual_font = action_label.pango_context().font_description().unwrap();
            let families = actual_font.family().expect("popup font family stack");
            let families = families.split(',').map(str::trim).collect::<Vec<_>>();
            assert_eq!(families, ["DejaVu Sans", "sans-serif"]);
            assert_eq!(actual_font.weight(), gtk::pango::Weight::Normal);
            assert_eq!(actual_font.style(), gtk::pango::Style::Normal);
            assert_eq!(
                actual_font.size(),
                (10.0 * ui_scale * f64::from(gtk::pango::SCALE)).round() as i32
            );
            assert!(matching_pixels(&popup, super::token::color::NAVIGATION) > 100);
            action.set_state_flags(gtk::StateFlags::PRELIGHT, false);
            settle();
            assert!(matching_pixels(&popup, super::token::color::NAVIGATION_SELECTED) > 100);
            action.unset_state_flags(gtk::StateFlags::PRELIGHT);
        }
        popup.popdown();
        let native_popup = find_popover(dropdown.upcast_ref()).expect("native dropdown popup");
        native_popup.popup();
        settle();
        assert!(matching_pixels(&native_popup, super::token::color::NAVIGATION) > 100);
        assert!(
            parsing_errors.borrow().is_empty(),
            "{:?}",
            parsing_errors.borrow()
        );
        native_popup.popdown();
        window.close();
        for provider in [&shell, &theme, &fonts] {
            gtk::style_context_remove_provider_for_display(&display, provider);
        }
    }

    #[test]
    #[ignore = "requires DISPLAY; run on its own GTK main thread"]
    fn workspace_dialogs_share_readable_chrome_surfaces() {
        adw::init().expect("GTK/Adwaita display");
        struct DesktopPreferences(gtk::Settings, bool, adw::StyleManager, adw::ColorScheme);
        impl Drop for DesktopPreferences {
            fn drop(&mut self) {
                self.0.set_gtk_enable_animations(self.1);
                self.2.set_color_scheme(self.3);
            }
        }
        let settings = gtk::Settings::default().unwrap();
        let style = adw::StyleManager::default();
        let _desktop = DesktopPreferences(
            settings.clone(),
            settings.is_gtk_enable_animations(),
            style.clone(),
            style.color_scheme(),
        );
        settings.set_gtk_enable_animations(false);
        let display = gtk::gdk::Display::default().unwrap();
        let provider = gtk::CssProvider::new();
        let parsing_errors = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let errors = parsing_errors.clone();
        provider.connect_parsing_error(move |_, _, error| {
            errors.borrow_mut().push(error.to_string());
        });
        provider.load_from_string(&super::shell_css());
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 20,
        );
        // Deliberately omit the main-window classes: detached adaptive dialogs
        // still need the same surface contract through their explicit opt-in.
        let host = adw::Window::new();
        host.set_default_size(760, 640);
        host.set_content(Some(&gtk::Label::new(Some("Workspace"))));
        host.present();
        let dialog = adw::Dialog::builder()
            .title("Command Palette")
            .content_width(440)
            .content_height(440)
            .build();
        super::style_workspace_dialog(&dialog);
        let header = adw::HeaderBar::new();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
        body.set_margin_top(16);
        body.set_margin_bottom(16);
        body.set_margin_start(16);
        body.set_margin_end(16);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search commands"));
        body.append(&search);
        let input = gtk::Entry::new();
        input.set_text("Workspace name");
        body.append(&input);
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        let row = adw::ActionRow::builder()
            .title("Open workspace")
            .subtitle("Choose a project")
            .build();
        list.append(&row);
        body.append(&list);
        let disabled = gtk::Button::with_label("Unavailable action");
        disabled.add_css_class("suggested-action");
        disabled.set_sensitive(false);
        body.append(&disabled);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        dialog.set_child(Some(&toolbar));
        dialog.present(Some(&host));
        settle();
        let disabled_label = disabled.child().unwrap().downcast::<gtk::Label>().unwrap();
        let color = |[r, g, b]: [u8; 3]| {
            gtk::gdk::RGBA::new(
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b) / 255.0,
                1.0,
            )
        };
        for scheme in [adw::ColorScheme::ForceLight, adw::ColorScheme::ForceDark] {
            style.set_color_scheme(scheme);
            settle();
            assert_color(input.color(), super::token::color::CHROME_TEXT);
            assert_color(disabled_label.color(), super::token::color::CHROME_MUTED);
            assert!(
                crate::config::contrast_ratio(
                    &input.color(),
                    &color(super::token::color::INSPECTOR),
                ) >= 4.5,
                "dialog inputs must remain readable in {scheme:?}"
            );
            assert!(matching_pixels(&dialog, super::token::color::NAVIGATION) > 100);
            assert!(
                modal_corner_alpha(&dialog) < 255,
                "the workspace must remain visible through the dialog scrim"
            );
            assert!(matching_pixels(&header, super::token::color::TITLEBAR) > 100);
            assert!(matching_pixels(&list, super::token::color::INSPECTOR_CARD) > 100);
            assert!(matching_pixels(&input, super::token::color::INSPECTOR) > 100);
            let family = input
                .pango_context()
                .font_description()
                .unwrap()
                .family()
                .unwrap();
            assert_eq!(family.split(',').next().map(str::trim), Some("DejaVu Sans"));
            list.select_row(Some(&row));
            settle();
            assert!(matching_pixels(&list, super::token::color::NAVIGATION_SELECTED) > 100);
            list.unselect_all();
        }
        dialog.force_close();
        settle();
        let preferences = adw::PreferencesDialog::new();
        preferences.set_title("Settings");
        super::style_workspace_dialog(&preferences);
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::new();
        group.set_title("Appearance");
        let entry = adw::EntryRow::new();
        entry.set_title("Host");
        entry.set_text("example.test");
        group.add(&entry);
        page.add(&group);
        preferences.add(&page);
        preferences.present(Some(&host));
        for scheme in [adw::ColorScheme::ForceLight, adw::ColorScheme::ForceDark] {
            style.set_color_scheme(scheme);
            settle();
            assert_color(entry.color(), super::token::color::CHROME_TEXT);
            assert!(matching_pixels(&preferences, super::token::color::NAVIGATION) > 100);
        }
        assert!(
            parsing_errors.borrow().is_empty(),
            "{:?}",
            parsing_errors.borrow()
        );
        preferences.force_close();
        host.close();
        gtk::style_context_remove_provider_for_display(&display, &provider);
    }
}
