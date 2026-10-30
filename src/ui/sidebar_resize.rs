//! Native workspace divider intent. Allocation clamps never replace saved widths.
use crate::config::Config;
use gtk::prelude::*;
use gtk4 as gtk;
use jterm_core::developer_workspace as token;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const NAV_MIN: i32 = token::MIN_NAVIGATION_WIDTH as i32;
const NAV_MAX: i32 = token::MAX_NAVIGATION_WIDTH as i32;
const INSPECTOR_MIN: i32 = token::MIN_INSPECTOR_WIDTH as i32;
const INSPECTOR_MAX: i32 = token::MAX_INSPECTOR_WIDTH as i32;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Navigation,
    Inspector,
    Drawer,
}

struct Edit {
    side: Side,
    origin_width: i32,
    preview: u32,
    changed: bool,
}

pub(super) struct SidebarResize {
    navigation: gtk::glib::WeakRef<gtk::Paned>,
    inspector: gtk::glib::WeakRef<gtk::Paned>,
    config: Rc<RefCell<Config>>,
    applying: Cell<bool>,
    navigation_width: Cell<Option<i32>>,
    inspector_width: Cell<Option<i32>>,
    allocation_pending: Cell<bool>,
    edit: RefCell<Option<Edit>>,
    drawer_pointer_x: Cell<f64>,
    changed: Rc<dyn Fn()>,
    reflow: Rc<dyn Fn()>,
}

impl SidebarResize {
    pub(super) fn new(
        navigation: &gtk::Paned,
        inspector: &gtk::Paned,
        config: Rc<RefCell<Config>>,
        changed: impl Fn() + 'static,
        reflow: impl Fn() + 'static,
    ) -> Rc<Self> {
        let this = Rc::new(Self {
            navigation: navigation.downgrade(),
            inspector: inspector.downgrade(),
            config,
            applying: Cell::new(false),
            navigation_width: Cell::new(None),
            inspector_width: Cell::new(None),
            allocation_pending: Cell::new(false),
            edit: RefCell::new(None),
            drawer_pointer_x: Cell::new(0.0),
            changed: Rc::new(changed),
            reflow: Rc::new(reflow),
        });
        this.bind_paned(navigation, Side::Navigation, "Resize navigation sidebar");
        this.bind_paned(inspector, Side::Inspector, "Resize right sidebar");
        this
    }

    pub(super) fn preferences(&self) -> (u32, u32) {
        let config = self.config.borrow();
        let mut widths = (config.sidebar_width, config.ai_panel_width);
        if let Some(edit) = self.edit.borrow().as_ref().filter(|edit| edit.changed) {
            match edit.side {
                Side::Navigation => widths.0 = edit.preview,
                Side::Inspector | Side::Drawer => widths.1 = edit.preview,
            }
        }
        widths
    }

    pub(super) fn apply(self: &Rc<Self>, navigation: Option<i32>, inspector_width: Option<i32>) {
        self.navigation_width.set(navigation);
        self.inspector_width.set(inspector_width);
        self.apply_positions();
        self.apply_after_allocation();
    }

    fn apply_after_allocation(self: &Rc<Self>) {
        if self.allocation_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        gtk::glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                this.allocation_pending.set(false);
                // The frame resize signal precedes nested Paned allocation.
                // Re-apply content widths after that allocation, including an
                // inspector just restored from a narrow-window drawer.
                this.apply_positions();
            }
        });
    }

    fn apply_positions(&self) {
        let old = self.applying.replace(true);
        if let (Some(paned), Some(width)) = (self.navigation.upgrade(), self.navigation_width.get())
        {
            if paned.position() != width {
                paned.set_position(width);
            }
        }
        if let (Some(paned), Some(width)) = (self.inspector.upgrade(), self.inspector_width.get()) {
            // Derive the start position from this Paned's actual allocation,
            // not the future center width of its ancestor. GtkPaned then keeps
            // the requested end width while the resizable center grows.
            let available = paned.width() - token::SIDEBAR_DIVIDER_WIDTH as i32;
            let position = (available - width).max(0);
            if available > 0 && paned.position() != position {
                paned.set_position(position);
            }
        }
        self.applying.set(old);
    }

    fn begin(&self, side: Side, width: i32) {
        if self.edit.borrow().is_some() {
            return;
        }
        let config = self.config.borrow();
        let preferred = if side == Side::Navigation {
            config.sidebar_width
        } else {
            config.ai_panel_width
        };
        *self.edit.borrow_mut() = Some(Edit {
            side,
            origin_width: width,
            preview: preferred,
            changed: false,
        });
    }

    fn preview(&self, side: Side, width: i32) {
        let changed = {
            let mut edit = self.edit.borrow_mut();
            let Some(edit) = edit.as_mut().filter(|edit| edit.side == side) else {
                return;
            };
            let different = edit.preview != width as u32;
            edit.preview = width as u32;
            edit.changed = width != edit.origin_width;
            different
        };
        if changed {
            (self.reflow)();
        }
    }

    pub(super) fn cancel_deferred(self: &Rc<Self>) {
        if self.edit.borrow_mut().take().is_none() {
            return;
        }
        // GTK can emit unmap in the middle of unparenting an inspector. Retire
        // intent now; reflow only after that native mutation has completed.
        let weak = Rc::downgrade(self);
        gtk::glib::idle_add_local_once(move || {
            if let Some(this) = weak.upgrade() {
                (this.reflow)();
            }
        });
    }

    fn finish_side(&self, side: Side) {
        if self
            .edit
            .borrow()
            .as_ref()
            .is_some_and(|edit| edit.side == side)
        {
            self.finish(true);
        }
    }

    pub(super) fn finish(&self, commit: bool) {
        let Some(edit) = self.edit.borrow_mut().take() else {
            return;
        };
        if commit && edit.changed {
            let mut config = self.config.borrow_mut();
            let target = match edit.side {
                Side::Navigation => &mut config.sidebar_width,
                Side::Inspector | Side::Drawer => &mut config.ai_panel_width,
            };
            let changed = *target != edit.preview;
            *target = edit.preview;
            drop(config);
            if changed {
                (self.changed)();
            }
        }
        (self.reflow)();
    }

    fn width(&self, paned: &gtk::Paned, side: Side) -> i32 {
        let divider = token::SIDEBAR_DIVIDER_WIDTH as i32;
        match side {
            Side::Navigation => paned.position(),
            Side::Inspector => paned.width() - paned.position() - divider,
            Side::Drawer => unreachable!(),
        }
    }

    fn observe_position(&self, paned: &gtk::Paned, side: Side) {
        if self.applying.get()
            || !self
                .edit
                .borrow()
                .as_ref()
                .is_some_and(|edit| edit.side == side)
        {
            return;
        }
        let divider = token::SIDEBAR_DIVIDER_WIDTH as i32;
        let maximum = if side == Side::Navigation {
            let inspector = self
                .inspector
                .upgrade()
                .and_then(|right| right.end_child())
                .filter(|child| child.is_visible())
                .map_or(0, |child| child.width() + divider);
            (paned.width() - divider - inspector - token::MIN_CENTER_WIDTH as i32)
                .clamp(NAV_MIN, NAV_MAX)
        } else {
            (paned.width() - divider - token::MIN_CENTER_WIDTH as i32)
                .clamp(INSPECTOR_MIN, INSPECTOR_MAX)
        };
        let minimum = if side == Side::Navigation {
            NAV_MIN
        } else {
            INSPECTOR_MIN
        };
        self.preview(side, self.width(paned, side).clamp(minimum, maximum));
    }

    fn bind_paned(self: &Rc<Self>, paned: &gtk::Paned, side: Side, title: &'static str) {
        paned.set_wide_handle(true);
        paned.set_focusable(true);
        // Pointer resizing must not take the native shell/IME's focus. F8 and
        // the existing GtkPaned keyboard actions remain available explicitly.
        paned.set_focus_on_click(false);
        paned.update_property(&[gtk::accessible::Property::Label(title)]);
        if let Some(handle) = handle(paned) {
            handle.set_cursor_from_name(Some("col-resize"));
            handle.set_tooltip_text(Some("Drag to resize. F8 focuses a divider; arrow keys resize, Enter keeps it, Escape cancels."));
        }
        let events = gtk::EventControllerLegacy::new();
        events.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        let divider = paned.downgrade();
        events.connect_event(move |_, event| {
            let (Some(this), Some(paned)) = (weak.upgrade(), divider.upgrade()) else {
                return gtk::glib::Propagation::Proceed;
            };
            if let Some(button) = event
                .downcast_ref::<gtk::gdk::ButtonEvent>()
                .filter(|event| event.button() == 1)
            {
                match button.event_type() {
                    gtk::gdk::EventType::ButtonPress if on_handle(&paned, event) => {
                        this.begin(side, this.width(&paned, side))
                    }
                    gtk::gdk::EventType::ButtonRelease => {
                        let weak = Rc::downgrade(&this);
                        gtk::glib::idle_add_local_once(move || {
                            if let Some(this) = weak.upgrade() {
                                this.finish(true);
                            }
                        });
                    }
                    _ => {}
                }
            }
            gtk::glib::Propagation::Proceed
        });
        paned.add_controller(events);
        let weak = Rc::downgrade(self);
        paned.connect_position_notify(move |paned| {
            if let Some(this) = weak.upgrade() {
                this.observe_position(paned, side);
            }
        });
        let weak = Rc::downgrade(self);
        paned.connect_max_position_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.apply_after_allocation();
            }
        });
        let weak = Rc::downgrade(self);
        paned.connect_min_position_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.apply_after_allocation();
            }
        });
        let weak = Rc::downgrade(self);
        paned.connect_move_handle(move |paned, _| {
            if let Some(this) = weak.upgrade() {
                this.begin(side, this.width(paned, side));
            }
            false
        });
        let weak = Rc::downgrade(self);
        paned.connect_accept_position(move |_| {
            if let Some(this) = weak.upgrade() {
                this.finish_side(side);
            }
            false
        });
        let weak = Rc::downgrade(self);
        paned.connect_cycle_handle_focus(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.finish_side(side);
            }
            false
        });
        let weak = Rc::downgrade(self);
        paned.connect_toggle_handle_focus(move |_| {
            if let Some(this) = weak.upgrade() {
                this.finish_side(side);
            }
            false
        });
        let weak = Rc::downgrade(self);
        paned.connect_has_focus_notify(move |paned| {
            if !paned.has_focus() {
                if let Some(this) = weak.upgrade() {
                    this.finish_side(side);
                }
            }
        });
        let weak = Rc::downgrade(self);
        paned.connect_unmap(move |_| {
            if let Some(this) = weak.upgrade() {
                this.cancel_deferred();
            }
        });
        if let Some(handle) = handle(paned) {
            let weak = Rc::downgrade(self);
            handle.connect_unmap(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.cancel_deferred();
                }
            });
        }
        let weak = Rc::downgrade(self);
        paned.connect_cancel_position(move |_| {
            // Cancel retires before GTK restores focus, so the focus-loss
            // acceptance path cannot accidentally commit an Escape action.
            if let Some(this) = weak.upgrade() {
                this.cancel_deferred();
            }
            false
        });
    }

    pub(super) fn drawer_grip(self: &Rc<Self>, inspector: &gtk::Box) -> gtk::DrawingArea {
        let grip = gtk::DrawingArea::new();
        grip.add_css_class("reference-drawer-grip");
        grip.set_width_request(token::SIDEBAR_DIVIDER_WIDTH as i32);
        grip.set_halign(gtk::Align::End);
        grip.set_valign(gtk::Align::Fill);
        grip.set_focusable(true);
        grip.set_focus_on_click(false);
        grip.set_accessible_role(gtk::AccessibleRole::Separator);
        grip.update_property(&[gtk::accessible::Property::Label("Resize right sidebar")]);
        grip.set_cursor_from_name(Some("col-resize"));
        grip.set_tooltip_text(Some("Drag the edge to resize the right sidebar. Focus it and use Left or Right for keyboard resizing."));
        grip.set_visible(false);
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let weak = Rc::downgrade(self);
        let panel = inspector.downgrade();
        drag.connect_drag_begin(move |gesture, _, _| {
            if let (Some(this), Some(panel)) = (weak.upgrade(), panel.upgrade()) {
                if let Some((x, _)) = gesture.current_event().and_then(|event| event.position()) {
                    this.drawer_pointer_x.set(x);
                }
                this.begin(Side::Drawer, outer_width(panel.upcast_ref()));
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        let weak = Rc::downgrade(self);
        let area = grip.downgrade();
        drag.connect_drag_update(move |gesture, offset, _| {
            if let (Some(this), Some(area)) = (weak.upgrade(), area.upgrade()) {
                let origin = this.edit.borrow().as_ref().map(|edit| edit.origin_width);
                let max = area
                    .parent()
                    .map_or(INSPECTOR_MAX, |parent| parent.width() - 32)
                    .clamp(INSPECTOR_MIN, INSPECTOR_MAX);
                if let Some(origin) = origin {
                    // The grip moves as the drawer grows. Measure against the
                    // stable native surface, not its changing local origin.
                    let offset = gesture
                        .current_event()
                        .and_then(|event| event.position())
                        .map_or(offset, |(x, _)| x - this.drawer_pointer_x.get());
                    this.preview(
                        Side::Drawer,
                        (origin - offset.round() as i32).clamp(INSPECTOR_MIN, max),
                    );
                }
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_drag_end(move |_, _, _| {
            if let Some(this) = weak.upgrade() {
                this.finish(true);
            }
        });
        let weak = Rc::downgrade(self);
        drag.connect_cancel(move |_, _| {
            if let Some(this) = weak.upgrade() {
                this.cancel_deferred();
            }
        });
        grip.add_controller(drag);
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        let panel = inspector.downgrade();
        let area = grip.downgrade();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let delta = match key {
                gtk::gdk::Key::Left => 10,
                gtk::gdk::Key::Right => -10,
                _ => return gtk::glib::Propagation::Proceed,
            };
            if let (Some(this), Some(panel), Some(area)) =
                (weak.upgrade(), panel.upgrade(), area.upgrade())
            {
                let max = area
                    .parent()
                    .map_or(INSPECTOR_MAX, |parent| parent.width() - 32)
                    .clamp(INSPECTOR_MIN, INSPECTOR_MAX);
                let step = if modifiers.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                    delta / 10
                } else {
                    delta
                };
                let width = outer_width(panel.upcast_ref());
                this.begin(Side::Drawer, width);
                this.preview(Side::Drawer, (width + step).clamp(INSPECTOR_MIN, max));
                this.finish(true);
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
        grip.add_controller(keys);
        grip
    }
}

fn outer_width(widget: &gtk::Widget) -> i32 {
    // Saved sidebar widths include their border. Widget::width() is the CSS
    // content box, so using it as the drag origin would lose that border on
    // every gesture (or keyboard step).
    widget
        .parent()
        .and_then(|parent| widget.compute_bounds(&parent))
        .map_or_else(|| widget.width(), |bounds| bounds.width().round() as i32)
}

fn handle(paned: &gtk::Paned) -> Option<gtk::Widget> {
    let mut child = paned.first_child();
    while let Some(widget) = child {
        if widget.css_name() == "separator" {
            return Some(widget);
        }
        child = widget.next_sibling();
    }
    None
}

fn on_handle(paned: &gtk::Paned, event: &gtk::gdk::Event) -> bool {
    let Some((x, y)) = event.position() else {
        return false;
    };
    let Some(root) = paned.root() else {
        return false;
    };
    let Some(native) = root.upcast_ref::<gtk::Widget>().native() else {
        return false;
    };
    if native.surface() != event.surface() {
        return false;
    }
    // GDK positions include the native CSD/shadow inset. Widget coordinates
    // start inside that inset, including when the root has no visible titlebar.
    let (surface_x, surface_y) = native.surface_transform();
    let Some(point) = root.upcast_ref::<gtk::Widget>().compute_point(
        paned,
        &gtk::graphene::Point::new((x - surface_x) as f32, (y - surface_y) as f32),
    ) else {
        return false;
    };
    handle(paned)
        .and_then(|handle| handle.compute_bounds(paned))
        .is_some_and(|bounds| bounds.contains_point(&point))
}

#[cfg(test)]
#[path = "sidebar_resize_tests.rs"]
mod tests;
