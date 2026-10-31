//! A neutral, grid-external presentation for the traditional VTE backend.
//! VTE does not expose trustworthy prompt or alternate-screen state here.
//! Consequently this adapter renders no command reactions and admits no Hello.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::glib;
use gtk4::prelude::*;
use jterm_core::organism::{
    sprite_frame_with_context, sticky_glyph_with_context, Behavior, BodyLanguage, RenderContext,
};

mod presentation;
use presentation::{Envelope, Paint, Rect};
use vte4::TerminalExt;

use super::organism::{HUMAN_INPUT_RETREAT, SURFACE_FRAME_INTERVAL};
use crate::config::{Config, OrganismMotion};

pub(crate) const VTE_ORGANISM_CAPABILITY: &str = "Traditional VTE shows a neutral companion. Command and alternate-screen state are unavailable, so live greetings are unavailable. The offline settings preview remains available. Neutral display only; compact. Use the command palette to expand this companion.";
pub(crate) const VTE_ORGANISM_HELLO_UNAVAILABLE: &str =
    "Live Hello is unavailable in traditional VTE. Try the offline preview in Settings.";

fn neutral_frame(motion: OrganismMotion, elapsed: Duration) -> u64 {
    if motion == OrganismMotion::Full {
        (elapsed.as_millis() / SURFACE_FRAME_INTERVAL.as_millis()) as u64
    } else {
        0
    }
}

fn neutral_wake(
    eligible: bool,
    motion: OrganismMotion,
    now: Duration,
    last_input: Option<Duration>,
) -> Option<Duration> {
    if !eligible {
        return None;
    }
    let retreat = last_input
        .and_then(|at| HUMAN_INPUT_RETREAT.checked_sub(now.saturating_sub(at)))
        .filter(|left| !left.is_zero());
    retreat
        .or_else(|| (motion == OrganismMotion::Full).then_some(SURFACE_FRAME_INTERVAL))
        .map(|delay| delay.max(Duration::from_millis(1)))
}

fn finite_positive_geometry(x: f32, y: f32, width: f32, height: f32) -> bool {
    [x, y, width, height].into_iter().all(f32::is_finite) && width > 0.0 && height > 0.0
}

fn finite_positive_rect(rect: &gtk4::graphene::Rect) -> bool {
    finite_positive_geometry(rect.x(), rect.y(), rect.width(), rect.height())
}

struct NativeSource {
    ticket: Rc<()>,
    id: glib::SourceId,
}

impl NativeSource {
    #[cfg(test)]
    fn as_raw(&self) -> u32 {
        self.id.as_raw()
    }
}

fn consume_source(slot: &RefCell<Option<NativeSource>>, ticket: &Rc<()>) -> bool {
    let current = slot
        .borrow()
        .as_ref()
        .is_some_and(|source| Rc::ptr_eq(&source.ticket, ticket));
    if current {
        slot.borrow_mut().take();
    }
    current
}

/// All four candidates occupy one native Overlay and participate in its
/// measurement. The measured widgets are also the painted widgets: GTK owns
/// font/CSS invalidation, and opacity never changes the envelope.
struct ExpandedCompanion {
    slot: gtk4::Overlay,
    labels: [gtk4::Label; 4],
}

impl ExpandedCompanion {
    fn new() -> Self {
        let context = RenderContext::new(Behavior::Idle, BodyLanguage::default(), false);
        let texts = [
            sprite_frame_with_context(context, 0).into_owned(),
            sprite_frame_with_context(context, 35).into_owned(),
            sticky_glyph_with_context(context, 0).into_owned(),
            sticky_glyph_with_context(context, 55).into_owned(),
        ];
        let slot = gtk4::Overlay::new();
        slot.set_can_target(false);
        slot.set_focusable(false);
        let labels = texts.map(|text| {
            let label = gtk4::Label::new(Some(&text));
            label.add_css_class("monospace");
            label.set_wrap(false);
            label.set_halign(gtk4::Align::Center);
            label.set_valign(gtk4::Align::Center);
            label.set_can_target(false);
            label.set_focusable(false);
            label.set_accessible_role(gtk4::AccessibleRole::Presentation);
            // Presentation leaf nodes are excluded from GTK accessibility trees.
            // Hidden is managed by GTK map/unmap, not a persistent decoration flag.
            label.set_opacity(0.0);
            label
        });
        slot.set_child(Some(&labels[0]));
        for label in &labels[1..] {
            slot.add_overlay(label);
            slot.set_measure_overlay(label, true);
            slot.set_clip_overlay(label, true);
        }
        Self { slot, labels }
    }
}

/// Owned by VteTerminalView. Signal closures and the single pending source
/// hold only weak references; the native Window subscription is disconnected.
pub(crate) struct VteOrganismSurface {
    root: glib::WeakRef<gtk4::Box>,
    terminal: glib::WeakRef<vte4::Terminal>,
    row: gtk4::Box,
    glyph: gtk4::Label,
    capability: gtk4::Label,
    expanded: Cell<bool>,
    mounting: Cell<bool>,
    expanded_widgets: RefCell<Option<Rc<ExpandedCompanion>>>,
    geometry_stamp: RefCell<Vec<u64>>,
    enabled: Cell<bool>,
    local: Cell<bool>,
    motion: Cell<OrganismMotion>,
    alive: Cell<bool>,
    mapped: Cell<bool>,
    epoch: Instant,
    last_input: Cell<Option<Duration>>,
    source: RefCell<Option<NativeSource>>,
    host: RefCell<Option<(glib::WeakRef<gtk4::Window>, glib::SignalHandlerId)>>,
    paint: RefCell<Option<(glib::WeakRef<gtk4::gdk::FrameClock>, glib::SignalHandlerId)>>,
    drawable: Cell<bool>,
    refresh_ticket: RefCell<Rc<()>>,
    #[cfg(test)]
    frame_ticks: Cell<u64>,
    geometry_idle: RefCell<Option<NativeSource>>,
}

impl VteOrganismSurface {
    pub(crate) fn new(root: &gtk4::Box, terminal: &vte4::Terminal, config: &Config) -> Rc<Self> {
        crate::eva_companion::tag_pane(root);
        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        row.set_can_target(false);
        row.set_focusable(false);
        row.set_hexpand(true);
        row.add_css_class("dim-label");
        // Establish real neutral content before the first geometry gate. The
        // row is still hidden until authoritative local admission, and the
        // glyph opacity remains subject to native eligibility below.
        let neutral = RenderContext::new(Behavior::Idle, BodyLanguage::default(), false);
        let initial_frame = sticky_glyph_with_context(neutral, 0);
        let glyph = gtk4::Label::new(Some(initial_frame.as_ref()));
        glyph.add_css_class("monospace");
        glyph.set_width_chars(5);
        glyph.set_max_width_chars(5);
        glyph.set_accessible_role(gtk4::AccessibleRole::Presentation);
        let capability = gtk4::Label::new(Some("VTE · neutral companion"));
        capability.set_xalign(0.0);
        capability.set_hexpand(true);
        capability.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        capability.update_property(&[gtk4::accessible::Property::Description(
            VTE_ORGANISM_CAPABILITY,
        )]);
        row.append(&glyph);
        row.append(&capability);
        root.append(&row);
        let motion = config.ascii_organism_motion.unwrap_or_else(|| {
            if gtk4::Settings::default().is_none_or(|s| s.is_gtk_enable_animations()) {
                OrganismMotion::Full
            } else {
                OrganismMotion::Calm
            }
        });
        let runtime = Rc::new(Self {
            root: root.downgrade(),
            terminal: terminal.downgrade(),
            row,
            glyph,
            capability,
            expanded: Cell::new(false),
            mounting: Cell::new(false),
            expanded_widgets: RefCell::new(None),
            geometry_stamp: RefCell::new(Vec::new()),
            enabled: Cell::new(
                config.ascii_organism_enabled && !crate::eva_companion::shared_mode_requested(),
            ),
            local: Cell::new(false),
            motion: Cell::new(motion),
            alive: Cell::new(true),
            mapped: Cell::new(root.is_mapped()),
            epoch: Instant::now(),
            last_input: Cell::new(None),
            source: RefCell::new(None),
            host: RefCell::new(None),
            paint: RefCell::new(None),
            drawable: Cell::new(false),
            refresh_ticket: RefCell::new(Rc::new(())),
            #[cfg(test)]
            frame_ticks: Cell::new(0),
            geometry_idle: RefCell::new(None),
        });
        let weak = Rc::downgrade(&runtime);
        terminal.connect_commit(move |_, _, _| {
            if let Some(runtime) = weak.upgrade() {
                // Prepared input is a conservative visual retreat only. It is
                // deliberately not a human-input or command event in the core.
                runtime.last_input.set(Some(runtime.epoch.elapsed()));
                runtime.refresh();
            }
        });
        let weak = Rc::downgrade(&runtime);
        terminal.connect_has_focus_notify(move |_| {
            if let Some(runtime) = weak.upgrade() {
                runtime.refresh();
            }
        });
        let weak = Rc::downgrade(&runtime);
        terminal.connect_child_exited(move |_, _| {
            if let Some(runtime) = weak.upgrade() {
                runtime.retire();
            }
        });
        let weak = Rc::downgrade(&runtime);
        root.connect_map(move |_| {
            if let Some(runtime) = weak.upgrade() {
                runtime.mapped.set(true);
                runtime.refresh();
            }
        });
        let weak = Rc::downgrade(&runtime);
        root.connect_unmap(move |_| {
            if let Some(runtime) = weak.upgrade() {
                runtime.invalidate_refresh();
                runtime.mapped.set(false);
                runtime.cancel_source();
                runtime.cancel_geometry_idle();
                runtime.disconnect_host();
                runtime.disconnect_paint();
                runtime.hide_paint();
            }
        });
        let weak = Rc::downgrade(&runtime);
        root.connect_root_notify(move |_| {
            if let Some(runtime) = weak.upgrade() {
                runtime.invalidate_refresh();
                runtime.mapped.set(false);
                runtime.cancel_source();
                runtime.cancel_geometry_idle();
                runtime.disconnect_host();
                runtime.disconnect_paint();
                runtime.refresh();
            }
        });
        runtime.refresh();
        runtime
    }

    fn expanded_widgets(&self) -> Option<Rc<ExpandedCompanion>> {
        self.expanded_widgets.borrow().clone()
    }

    fn hide_paint(&self) {
        let ticket = self.refresh_ticket.borrow().clone();
        self.glyph.set_opacity(0.0);
        if !self.current_refresh(&ticket) {
            return;
        }
        if let Some(widgets) = self.expanded_widgets() {
            for label in &widgets.labels {
                label.set_opacity(0.0);
                if !self.current_refresh(&ticket) {
                    return;
                }
            }
        }
    }

    fn set_expanded(self: &Rc<Self>, requested: bool) -> bool {
        if !self.alive.get() || !self.local.get() || !self.enabled.get() {
            return false;
        }
        if presentation::requested_change(self.expanded.get(), requested, true).is_none() {
            return true;
        }
        self.expanded.set(requested);
        // Description follows the accepted desired state, not a render ticket.
        // A synchronous opposite request updates it again before we return.
        self.capability.update_property(&[gtk4::accessible::Property::Description(
            if requested {
                "Traditional VTE: neutral display only; expanded. Use the command palette to collapse. Command reactions and live greetings remain unavailable."
            } else {
                VTE_ORGANISM_CAPABILITY
            },
        )]);
        self.refresh();
        true
    }

    fn sync_presentation_layout(&self, ticket: &Rc<()>) {
        if !self.alive.get() {
            return;
        }
        struct Mounting<'a>(&'a Cell<bool>);
        impl Drop for Mounting<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        self.mounting.set(true);
        let _mounting = Mounting(&self.mounting);
        let widgets = if self.expanded.get() {
            Some(self.expanded_widgets().unwrap_or_else(|| {
                let widgets = Rc::new(ExpandedCompanion::new());
                *self.expanded_widgets.borrow_mut() = Some(widgets.clone());
                widgets
            }))
        } else {
            self.expanded_widgets()
        };
        // Native parent notifications may request the opposite state. Reentrant
        // refresh retires this ticket and uses the existing geometry idle.
        if self.expanded.get() {
            if self.glyph.parent().as_ref() == Some(self.row.upcast_ref()) {
                self.row.remove(&self.glyph);
                if !self.current_refresh(ticket) {
                    return;
                }
            }
            if let Some(widgets) = widgets {
                if widgets.slot.parent().is_none() {
                    self.row.prepend(&widgets.slot);
                }
            }
        } else {
            if let Some(widgets) = widgets {
                if widgets.slot.parent().as_ref() == Some(self.row.upcast_ref()) {
                    self.row.remove(&widgets.slot);
                    if !self.current_refresh(ticket) {
                        return;
                    }
                }
            }
            if self.glyph.parent().is_none() {
                self.row.prepend(&self.glyph);
            }
        }
    }

    /// Palette's original weak target must still be the current attached pane.
    /// Presentation does not claim a behavioral owner or bypass terminal focus.
    pub(crate) fn present_for_target(
        target: &std::rc::Weak<Self>,
        current: Option<&Rc<Self>>,
        host: &gtk4::Window,
        requested: bool,
    ) -> bool {
        let Some(runtime) = presentation::resolve_current(target, current) else {
            return false;
        };
        if !host.is_active()
            || !runtime.alive.get()
            || !runtime.enabled.get()
            || !runtime.local.get()
        {
            return false;
        }
        let Some(root) = runtime.root.upgrade() else {
            return false;
        };
        if !root.is_mapped() || !root.is_ancestor(host) {
            return false;
        }
        if !runtime.set_expanded(requested) {
            return false;
        }
        runtime.refresh();
        true
    }

    fn invalidate_refresh(&self) -> Rc<()> {
        let ticket = Rc::new(());
        *self.refresh_ticket.borrow_mut() = ticket.clone();
        ticket
    }

    fn current_refresh(&self, ticket: &Rc<()>) -> bool {
        Rc::ptr_eq(&self.refresh_ticket.borrow(), ticket)
    }

    fn cancel_source(&self) {
        let source = self.source.borrow_mut().take();
        if let Some(source) = source {
            source.id.remove();
        }
    }

    fn disconnect_host(&self) {
        let observer = self.host.borrow_mut().take();
        if let Some((host, handler)) = observer {
            if let Some(host) = host.upgrade() {
                host.disconnect(handler);
            }
        }
    }

    fn cancel_geometry_idle(&self) {
        let source = self.geometry_idle.borrow_mut().take();
        if let Some(source) = source {
            source.id.remove();
        }
    }

    fn defer_geometry_refresh(self: &Rc<Self>) {
        if self.geometry_idle.borrow().is_some() {
            return;
        }
        let weak = Rc::downgrade(self);
        let ticket = Rc::new(());
        let callback_ticket = ticket.clone();
        let id = glib::idle_add_local_once(move || {
            if let Some(runtime) = weak.upgrade() {
                if !consume_source(&runtime.geometry_idle, &callback_ticket) {
                    return;
                }
                runtime.refresh();
            }
        });
        *self.geometry_idle.borrow_mut() = Some(NativeSource { ticket, id });
    }

    fn disconnect_paint(&self) {
        let observer = self.paint.borrow_mut().take();
        if let Some((clock, handler)) = observer {
            if let Some(clock) = clock.upgrade() {
                clock.disconnect(handler);
            }
        }
    }

    fn bind_paint(self: &Rc<Self>, eligible: bool) {
        let clock = eligible
            .then(|| self.root.upgrade().and_then(|root| root.frame_clock()))
            .flatten();
        let old = self
            .paint
            .borrow()
            .as_ref()
            .and_then(|(clock, _)| clock.upgrade());
        if clock == old {
            return;
        }
        self.disconnect_paint();
        if let Some(clock) = clock {
            let weak = Rc::downgrade(self);
            // Passive observation only: this connection never requests a
            // frame. It can recover a clipped pane on the next real paint,
            // including Paned transforms that do not reallocate the child.
            let handler = clock.connect_after_paint(move |clock| {
                if let Some(runtime) = weak.upgrade() {
                    let current = runtime.root.upgrade().and_then(|root| root.frame_clock());
                    if current.as_ref() != Some(clock) {
                        return;
                    }
                    let drawable = runtime.drawable_geometry();
                    let signature = runtime.geometry_signature();
                    let geometry_changed = *runtime.geometry_stamp.borrow() != signature;
                    if geometry_changed {
                        *runtime.geometry_stamp.borrow_mut() = signature;
                    }
                    if runtime.drawable.replace(drawable) != drawable || geometry_changed {
                        runtime.defer_geometry_refresh();
                    }
                }
            });
            *self.paint.borrow_mut() = Some((clock.downgrade(), handler));
        }
    }

    fn clipped_geometry(&self, widget: &gtk4::Widget) -> Option<gtk4::graphene::Rect> {
        let root = self.root.upgrade()?;
        if !self.mapped.get() || !self.row.is_mapped() || !widget.is_mapped() {
            return None;
        }
        let mut area = widget.compute_bounds(&root)?;
        if !finite_positive_rect(&area) {
            return None;
        }
        let mut ancestor = widget.parent();
        while let Some(widget) = ancestor {
            if widget.width() <= 0 || widget.height() <= 0 {
                return None;
            }
            let clip = widget.compute_bounds(&root)?;
            if !finite_positive_rect(&clip) {
                return None;
            }
            area = area.intersection(&clip)?;
            ancestor = widget.parent();
        }
        finite_positive_rect(&area).then_some(area)
    }

    fn presentation_widget(&self) -> gtk4::Widget {
        if self.expanded.get() {
            if let Some(widgets) = self.expanded_widgets() {
                return widgets.slot.clone().upcast();
            }
        }
        self.glyph.clone().upcast()
    }

    fn drawable_geometry(&self) -> bool {
        self.clipped_geometry(&self.presentation_widget()).is_some()
    }

    fn rect(rect: &gtk4::graphene::Rect) -> Rect {
        Rect {
            x: f64::from(rect.x()),
            y: f64::from(rect.y()),
            width: f64::from(rect.width()),
            height: f64::from(rect.height()),
        }
    }

    fn candidate_bounds(&self, label: &gtk4::Label) -> Option<Rect> {
        let root = self.root.upgrade()?;
        if !label.is_mapped() {
            return None;
        }
        let (ink, logical) = label.layout().pixel_extents();
        let (offset_x, offset_y) = label.layout_offsets();
        let left = f64::from(ink.x().min(logical.x())) + f64::from(offset_x);
        let top = f64::from(ink.y().min(logical.y())) + f64::from(offset_y);
        let right = (f64::from(ink.x()) + f64::from(ink.width()))
            .max(f64::from(logical.x()) + f64::from(logical.width()))
            + f64::from(offset_x);
        let bottom = (f64::from(ink.y()) + f64::from(ink.height()))
            .max(f64::from(logical.y()) + f64::from(logical.height()))
            + f64::from(offset_y);
        let points = [(left, top), (right, top), (left, bottom), (right, bottom)]
            .into_iter()
            .map(|(x, y)| {
                label.compute_point(&root, &gtk4::graphene::Point::new(x as f32, y as f32))
            })
            .collect::<Option<Vec<_>>>()?;
        if points
            .iter()
            .any(|p| !p.x().is_finite() || !p.y().is_finite())
        {
            return None;
        }
        let left = points.iter().map(|p| p.x()).fold(f32::INFINITY, f32::min);
        let top = points.iter().map(|p| p.y()).fold(f32::INFINITY, f32::min);
        let right = points
            .iter()
            .map(|p| p.x())
            .fold(f32::NEG_INFINITY, f32::max);
        let bottom = points
            .iter()
            .map(|p| p.y())
            .fold(f32::NEG_INFINITY, f32::max);
        let rect = Rect {
            x: f64::from(left),
            y: f64::from(top),
            width: f64::from(right - left),
            height: f64::from(bottom - top),
        };
        rect.valid().then_some(rect)
    }

    fn candidate_union(&self, labels: &[gtk4::Label]) -> Option<Rect> {
        let mut bounds = labels.iter().map(|label| self.candidate_bounds(label));
        let mut result = bounds.next()??;
        for bounds in bounds {
            result = result.union(bounds?)?;
        }
        Some(result)
    }

    /// Qualification only, no Pango extents or requisition measurement in the
    /// after-paint observer. GTK owns candidate font/CSS measurement itself.
    fn geometry_signature(&self) -> Vec<u64> {
        let mut signature = Vec::new();
        if let Some(clip) = self.clipped_geometry(&self.presentation_widget()) {
            for value in [clip.x(), clip.y(), clip.width(), clip.height()] {
                signature.push(u64::from(value.to_bits()));
            }
        }
        if let Some(root) = self.root.upgrade() {
            let widgets = self.expanded_widgets();
            let labels: &[gtk4::Label] = if self.expanded.get() {
                widgets.as_ref().map_or(&[], |widgets| &widgets.labels)
            } else {
                std::slice::from_ref(&self.glyph)
            };
            for label in labels {
                signature.push(u64::from(label.pango_context().serial()));
                signature.push(u64::from(label.layout().serial()));
                let (x, y) = label.layout_offsets();
                signature.extend([x as u64, y as u64]);
                Self::append_geometry_stamp(label.upcast_ref(), &root, &mut signature);
            }
            // Include every ancestor, even when its changed clip has the same
            // final intersection. No opacity or paint state enters this stamp.
            let mut ancestor = Some(self.presentation_widget());
            while let Some(widget) = ancestor {
                Self::append_geometry_stamp(&widget, &root, &mut signature);
                ancestor = widget.parent();
            }
        }
        signature
    }

    fn append_geometry_stamp(widget: &gtk4::Widget, root: &gtk4::Box, signature: &mut Vec<u64>) {
        signature.extend([widget.width() as u64, widget.height() as u64]);
        if let Some(bounds) = widget.compute_bounds(root) {
            signature.push(1);
            for value in [bounds.x(), bounds.y(), bounds.width(), bounds.height()] {
                signature.push(u64::from(value.to_bits()));
            }
        } else {
            signature.push(0);
        }
        if let Some(transform) = widget.compute_transform(root) {
            signature.push(1);
            signature.extend(transform.to_float().map(|v| u64::from(v.to_bits())));
        } else {
            signature.push(0);
        }
    }

    fn paint_plan(&self, admitted: bool, retreat: bool) -> presentation::Plan {
        let widgets = self.expanded_widgets();
        let envelope = widgets.as_ref().map(|widgets| Envelope {
            width: widgets.slot.width(),
            height: widgets.slot.height(),
        });
        let clip = self
            .clipped_geometry(&self.presentation_widget())
            .as_ref()
            .map(Self::rect);
        let full = widgets
            .as_ref()
            .and_then(|widgets| self.candidate_union(&widgets.labels[..2]));
        let fallback = if self.expanded.get() {
            widgets
                .as_ref()
                .and_then(|widgets| self.candidate_union(&widgets.labels[2..]))
        } else {
            self.candidate_bounds(&self.glyph)
        };
        presentation::plan(
            self.expanded.get(),
            admitted,
            retreat,
            envelope,
            clip,
            full,
            fallback,
        )
    }

    fn paint_expanded(&self, paint: Paint, frame: u64, ticket: &Rc<()>) {
        let Some(widgets) = self.expanded_widgets() else {
            return;
        };
        let context = RenderContext::new(Behavior::Idle, BodyLanguage::default(), false);
        let wanted = match paint {
            Paint::ExpandedSprite => Some(sprite_frame_with_context(context, frame)),
            Paint::ExpandedCompactFallback => Some(sticky_glyph_with_context(context, frame)),
            _ => None,
        };
        for label in &widgets.labels {
            let opacity = if wanted
                .as_ref()
                .is_some_and(|text| label.text().as_str() == text.as_ref())
            {
                1.0
            } else {
                0.0
            };
            if label.opacity() != opacity {
                label.set_opacity(opacity);
            }
            if !self.current_refresh(ticket) {
                return;
            }
        }
    }

    fn bind_host(self: &Rc<Self>) -> Option<gtk4::Window> {
        let root = self.root.upgrade()?;
        let current = (self.mapped.get() && root.is_mapped())
            .then(|| root.root().and_downcast::<gtk4::Window>())
            .flatten();
        let old = self
            .host
            .borrow()
            .as_ref()
            .and_then(|(host, _)| host.upgrade());
        if old == current {
            return current;
        }
        self.cancel_source();
        self.disconnect_host();
        if let Some(host) = &current {
            let weak = Rc::downgrade(self);
            let handler = host.connect_is_active_notify(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    runtime.refresh();
                }
            });
            *self.host.borrow_mut() = Some((host.downgrade(), handler));
        }
        current
    }

    pub(crate) fn set_local(self: &Rc<Self>, local: bool) {
        if let Some(root) = self.root.upgrade() {
            if local {
                root.remove_css_class("eva-remote-surface");
            } else {
                root.add_css_class("eva-remote-surface");
            }
        }
        self.local.set(local);
        self.refresh();
    }

    pub(crate) fn apply_settings(self: &Rc<Self>, enabled: bool, motion: OrganismMotion) {
        self.enabled
            .set(enabled && !crate::eva_companion::shared_mode_requested());
        self.motion.set(motion);
        self.refresh();
    }

    pub(crate) fn retire(&self) {
        self.invalidate_refresh();
        self.alive.set(false);
        self.cancel_source();
        self.cancel_geometry_idle();
        self.disconnect_host();
        self.disconnect_paint();
        self.hide_paint();
    }

    fn refresh(self: &Rc<Self>) {
        let refresh_ticket = self.invalidate_refresh();
        self.cancel_source();
        self.cancel_geometry_idle();
        if !self.alive.get() {
            self.disconnect_host();
            self.disconnect_paint();
            self.hide_paint();
            return;
        }
        if self.mounting.get() {
            if self.alive.get() {
                self.defer_geometry_refresh();
            }
            return;
        }
        self.sync_presentation_layout(&refresh_ticket);
        if !self.current_refresh(&refresh_ticket) {
            if self.alive.get() {
                self.defer_geometry_refresh();
            }
            return;
        }
        let host = if self.enabled.get() && self.local.get() && self.alive.get() {
            self.bind_host()
        } else {
            self.disconnect_host();
            None
        };
        let eligible = self.local.get()
            && self.enabled.get()
            && self.alive.get()
            && self.mapped.get()
            && self.root.upgrade().is_some_and(|root| root.is_mapped())
            && host.is_some_and(|host| host.is_active())
            && self
                .terminal
                .upgrade()
                .is_some_and(|terminal| terminal.is_mapped() && terminal.has_focus());
        // Preserve the existing geometry envelope when focus moves between
        // panes. The complete non-owner row, including its title, is unpainted
        // without changing terminal allocation or reflowing its grid.
        self.row
            .set_visible(self.enabled.get() && self.local.get() && self.alive.get());
        if !self.current_refresh(&refresh_ticket) {
            return;
        }
        self.row.set_opacity(if eligible { 1.0 } else { 0.0 });
        if !self.current_refresh(&refresh_ticket) {
            return;
        }
        self.bind_paint(eligible);
        if !self.current_refresh(&refresh_ticket) {
            return;
        }
        let drawable = self.drawable_geometry();
        self.drawable.set(drawable);
        let eligible = eligible && drawable;
        let now = self.epoch.elapsed();
        let retreat = self
            .last_input
            .get()
            .is_some_and(|at| now.saturating_sub(at) < HUMAN_INPUT_RETREAT);
        // Measure the exact next compact pose before permitting any paint.
        if !self.expanded.get() {
            // Idle is only the renderer's neutral pose here, never evidence
            // about the shell. The semantic label states the capability limit.
            let context = RenderContext::new(Behavior::Idle, BodyLanguage::default(), false);
            let frame = sticky_glyph_with_context(context, neutral_frame(self.motion.get(), now));
            if self.glyph.text().as_str() != frame.as_ref() {
                self.glyph.set_text(frame.as_ref());
            }
        }
        if !self.current_refresh(&refresh_ticket) {
            return;
        }
        let plan = self.paint_plan(eligible, retreat);
        if self.expanded.get() {
            self.paint_expanded(
                plan.paint,
                neutral_frame(self.motion.get(), now),
                &refresh_ticket,
            );
        } else {
            self.glyph.set_opacity(if plan.paint == Paint::Compact {
                1.0
            } else {
                0.0
            });
        }
        if !self.current_refresh(&refresh_ticket) {
            return;
        }
        let signature = self.geometry_signature();
        *self.geometry_stamp.borrow_mut() = signature;
        let wake_eligible = eligible && self.paint_plan(eligible, false).paint != Paint::Hidden;
        if let Some(delay) =
            neutral_wake(wake_eligible, self.motion.get(), now, self.last_input.get())
        {
            let weak = Rc::downgrade(self);
            let ticket = Rc::new(());
            let callback_ticket = ticket.clone();
            let id = glib::timeout_add_local_once(delay, move || {
                if let Some(runtime) = weak.upgrade() {
                    if !consume_source(&runtime.source, &callback_ticket) {
                        return;
                    }
                    #[cfg(test)]
                    runtime
                        .frame_ticks
                        .set(runtime.frame_ticks.get().saturating_add(1));
                    runtime.refresh();
                }
            });
            if self.current_refresh(&refresh_ticket) {
                *self.source.borrow_mut() = Some(NativeSource { ticket, id });
            } else {
                id.remove();
            }
        }
    }
}

impl super::UiState {
    pub(super) fn vte_companion_target(&self) -> Option<std::rc::Weak<VteOrganismSurface>> {
        match self.current_pane_leaf() {
            Some(super::PaneLeaf::Vte(view)) => {
                Some(Rc::downgrade(&view.organism_presentation_surface()))
            }
            _ => None,
        }
    }

    pub(super) fn set_vte_companion_for(
        &self,
        target: &Option<std::rc::Weak<VteOrganismSurface>>,
        expanded: bool,
    ) {
        let Some(target) = target else {
            return;
        };
        let Some(super::PaneLeaf::Vte(view)) = self.current_pane_leaf() else {
            return;
        };
        if super::PaneLeaf::Vte(view.clone()).is_remote() {
            return;
        }
        let current = view.organism_presentation_surface();
        if !VteOrganismSurface::present_for_target(
            target,
            Some(&current),
            &self.window.clone().upcast(),
            expanded,
        ) {
            return;
        }
        // Native property updates can reenter navigation. Resolve current pane
        // again before restoring focus, never use the previous pane snapshot.
        let Some(super::PaneLeaf::Vte(current_view)) = self.current_pane_leaf() else {
            return;
        };
        if !Rc::ptr_eq(&view, &current_view)
            || super::PaneLeaf::Vte(current_view.clone()).is_remote()
        {
            return;
        }
        let root = current_view.widget();
        if self.window.is_active()
            && root.is_mapped()
            && root.is_ancestor(&self.window)
            && current.alive.get()
            && current.local.get()
            && current.enabled.get()
        {
            current_view.vte().grab_focus();
            current.refresh();
        }
    }
}

impl Drop for VteOrganismSurface {
    fn drop(&mut self) {
        self.cancel_source();
        self.cancel_geometry_idle();
        self.disconnect_host();
        self.disconnect_paint();
    }
}

#[cfg(test)]
pub(super) fn exercise_existing_native_fixture(window: &gtk4::Window) {
    use libadwaita::prelude::*;
    let fixture_host = window.downcast_ref::<libadwaita::Window>().unwrap();
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    root.set_vexpand(true);
    let other = gtk4::Entry::new();
    let terminal = vte4::Terminal::new();
    terminal.set_vexpand(true);
    terminal.set_hexpand(true);
    root.append(&other);
    root.append(&terminal);
    let mut config = Config::safe_defaults();
    config.ascii_organism_enabled = true;
    config.ascii_organism_motion = Some(OrganismMotion::Full);
    let runtime = VteOrganismSurface::new(&root, &terminal, &config);
    assert!(
        !runtime.row.is_visible(),
        "local identity starts unavailable"
    );
    assert!(runtime.source.borrow().is_none());
    let initial_context = RenderContext::new(Behavior::Idle, BodyLanguage::default(), false);
    assert_eq!(
        runtime.glyph.text().as_str(),
        sticky_glyph_with_context(initial_context, 0).as_ref()
    );
    assert_eq!(runtime.glyph.opacity(), 0.0);
    runtime.set_local(true);
    assert!(
        terminal.pty().is_none(),
        "this fixture never spawns a child"
    );
    fixture_host.set_content(Some(&root));
    terminal.grab_focus();
    let main = glib::MainContext::default();
    let diagnostic_runtime = Rc::downgrade(&runtime);
    let wait = |stage: &str, ready: &dyn Fn() -> bool| {
        let start = Instant::now();
        loop {
            while main.iteration(false) {}
            if ready() {
                break;
            }
            if start.elapsed() >= Duration::from_secs(3) {
                if let Some(runtime) = diagnostic_runtime.upgrade() {
                    eprintln!(
                        "VTE_STAGE={stage} enabled={} local={} alive={} mapped_cache={} root_mapped={} terminal_mapped={} terminal_focus={} row_mapped={} glyph_mapped={} drawable={} source={} paint={} idle={} glyph_text={:?}",
                        runtime.enabled.get(),
                        runtime.local.get(),
                        runtime.alive.get(),
                        runtime.mapped.get(),
                        root.is_mapped(),
                        terminal.is_mapped(),
                        terminal.has_focus(),
                        runtime.row.is_mapped(),
                        runtime.glyph.is_mapped(),
                        runtime.drawable_geometry(),
                        runtime.source.borrow().is_some(),
                        runtime.paint.borrow().is_some(),
                        runtime.geometry_idle.borrow().is_some(),
                        runtime.glyph.text(),
                    );
                    let actual_host = root.root().and_downcast::<gtk4::Window>();
                    eprintln!(
                        "VTE_HOST present={} active={}",
                        actual_host.is_some(),
                        actual_host.as_ref().is_some_and(|host| host.is_active())
                    );
                    let focused = actual_host.as_ref().and_then(gtk4::prelude::RootExt::focus);
                    eprintln!(
                        "VTE_FOCUS type={:?} in_entry={} is_terminal={}",
                        focused.as_ref().map(|widget| widget.type_().name()),
                        focused
                            .as_ref()
                            .is_some_and(|widget| widget.is_ancestor(&other)),
                        focused.as_ref() == Some(terminal.upcast_ref()),
                    );
                    let mut ancestor = Some(runtime.glyph.clone().upcast::<gtk4::Widget>());
                    while let Some(widget) = ancestor {
                        let bounds = widget
                            .compute_bounds(&root)
                            .map(|b| (b.x(), b.y(), b.width(), b.height()));
                        eprintln!(
                            "VTE_GEOMETRY type={} mapped={} allocation={}x{} bounds={bounds:?}",
                            widget.type_().name(),
                            widget.is_mapped(),
                            widget.width(),
                            widget.height()
                        );
                        if let Some(paned) = widget.downcast_ref::<gtk4::Paned>() {
                            eprintln!(
                                "VTE_PANED position={} min={} max={}",
                                paned.position(),
                                paned.min_position(),
                                paned.max_position()
                            );
                        }
                        ancestor = widget.parent();
                    }
                }
                panic!("native VTE presentation did not settle at {stage}");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    wait("initial local mapping", &|| {
        window.is_active()
            && terminal.has_focus()
            && runtime.row.is_mapped()
            && terminal.height() > 0
            && runtime.source.borrow().is_some()
    });
    assert!(!runtime.row.can_target());
    assert!(!runtime.row.is_focusable());
    assert_eq!(
        runtime.glyph.accessible_role(),
        gtk4::AccessibleRole::Presentation
    );
    assert!(runtime.source.borrow().is_some());
    let retired_timer_ticket = runtime.source.borrow().as_ref().unwrap().ticket.clone();
    runtime.refresh();
    let timer_after_refresh = runtime.source.borrow().as_ref().unwrap().as_raw();
    assert!(!consume_source(&runtime.source, &retired_timer_ticket));
    assert_eq!(
        runtime.source.borrow().as_ref().unwrap().as_raw(),
        timer_after_refresh
    );
    runtime.defer_geometry_refresh();
    let retired_idle_ticket = runtime
        .geometry_idle
        .borrow()
        .as_ref()
        .unwrap()
        .ticket
        .clone();
    let one_idle = runtime.geometry_idle.borrow().as_ref().unwrap().as_raw();
    runtime.defer_geometry_refresh();
    assert_eq!(
        runtime.geometry_idle.borrow().as_ref().unwrap().as_raw(),
        one_idle
    );
    runtime.cancel_geometry_idle();
    runtime.defer_geometry_refresh();
    let current_idle = runtime.geometry_idle.borrow().as_ref().unwrap().as_raw();
    assert!(!consume_source(
        &runtime.geometry_idle,
        &retired_idle_ticket
    ));
    assert_eq!(
        runtime.geometry_idle.borrow().as_ref().unwrap().as_raw(),
        current_idle
    );
    runtime.cancel_geometry_idle();
    let previous_frame_ticks = runtime.frame_ticks.get();
    wait("first Full frame", &|| {
        runtime.frame_ticks.get() > previous_frame_ticks && runtime.source.borrow().is_some()
    });
    assert!(runtime.epoch.elapsed() >= SURFACE_FRAME_INTERVAL);
    exercise_expanded_fixture(&runtime, &terminal, &other, window, &main, &wait);
    let allocation = (terminal.width(), terminal.height(), runtime.row.height());
    let minimum = runtime.row.measure(gtk4::Orientation::Vertical, -1).0;
    assert!(minimum > 0);
    let observed = Rc::new(Cell::new(0));
    let observed_callback = observed.clone();
    terminal.connect_commit(move |_, _, _| observed_callback.set(observed_callback.get() + 1));
    terminal.emit_by_name::<()>("commit", &[&"x", &1_u32]);
    terminal.emit_by_name::<()>("commit", &[&"y", &1_u32]);
    assert_eq!(observed.get(), 2, "input observers are not intercepted");
    assert_eq!(runtime.glyph.opacity(), 0.0);
    assert!(runtime.row.is_visible());
    assert!(runtime.source.borrow().is_some());
    assert!(terminal.has_focus());
    while main.iteration(false) {}
    assert_eq!(
        allocation,
        (terminal.width(), terminal.height(), runtime.row.height())
    );
    assert_eq!(
        minimum,
        runtime.row.measure(gtk4::Orientation::Vertical, -1).0
    );
    assert!(other.grab_focus());
    // GtkEntry delegates native focus to its internal GtkText child.
    wait("focus moved to entry", &|| {
        gtk4::prelude::RootExt::focus(window).is_some_and(|focused| focused.is_ancestor(&other))
    });
    assert!(!terminal.has_focus());
    assert_eq!(
        runtime.row.opacity(),
        0.0,
        "non-owner title also disappears"
    );
    assert!(runtime.paint.borrow().is_none());
    assert!(runtime.geometry_idle.borrow().is_none());
    assert!(runtime.source.borrow().is_none());
    assert_eq!(runtime.glyph.opacity(), 0.0);
    assert_eq!(
        allocation,
        (terminal.width(), terminal.height(), runtime.row.height())
    );
    // Absolute input age remains authoritative after losing/reacquiring focus.
    runtime.last_input.set(Some(Duration::ZERO));
    wait("input retreat elapsed", &|| {
        runtime.epoch.elapsed() >= HUMAN_INPUT_RETREAT
    });
    terminal.grab_focus();
    wait("terminal refocused", &|| terminal.has_focus());
    runtime.apply_settings(true, OrganismMotion::Calm);
    assert!(runtime.source.borrow().is_none());
    assert_eq!(runtime.glyph.opacity(), 1.0);
    let context = RenderContext::new(Behavior::Idle, BodyLanguage::default(), false);
    assert_eq!(
        runtime.glyph.text().as_str(),
        sticky_glyph_with_context(context, 0).as_ref()
    );
    runtime.apply_settings(true, OrganismMotion::Static);
    assert!(runtime.source.borrow().is_none());
    assert_eq!(
        allocation,
        (terminal.width(), terminal.height(), runtime.row.height())
    );
    runtime.apply_settings(true, OrganismMotion::Full);
    assert!(runtime.source.borrow().is_some());
    root.set_visible(false);
    assert!(runtime.source.borrow().is_none());
    assert!(runtime.host.borrow().is_none());
    root.set_visible(true);
    wait("row remapped", &|| root.is_mapped());
    terminal.grab_focus();
    wait("source after remap", &|| {
        terminal.has_focus() && runtime.source.borrow().is_some()
    });
    runtime.apply_settings(false, OrganismMotion::Full);
    assert!(!runtime.row.is_visible());
    assert!(runtime.source.borrow().is_none());
    runtime.apply_settings(true, OrganismMotion::Full);
    // The same pane can detach and return without retaining its old host hook.
    fixture_host.set_content(gtk4::Widget::NONE);
    assert!(runtime.source.borrow().is_none());
    assert!(runtime.host.borrow().is_none());
    fixture_host.set_content(Some(&root));
    wait("root reattached", &|| root.is_mapped());
    terminal.grab_focus();
    wait("source after reattach", &|| {
        terminal.has_focus() && runtime.source.borrow().is_some()
    });
    // A nonzero Paned slot can clip a minimum-sized child without unmapping
    // it. Passive paint observation must stop and recover the animation.
    fixture_host.set_content(gtk4::Widget::NONE);
    let paned = gtk4::Paned::new(gtk4::Orientation::Vertical);
    paned.set_shrink_start_child(true);
    paned.set_shrink_end_child(true);
    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    root.set_size_request(-1, 240);
    paned.set_start_child(Some(&spacer));
    paned.set_end_child(Some(&root));
    fixture_host.set_content(Some(&paned));
    paned.set_position(10);
    terminal.grab_focus();
    wait("Paned initially visible", &|| {
        paned.height() > 0
            && paned.max_position() > 1
            && paned.max_position() <= paned.height()
            && root.compute_bounds(&paned).is_some_and(|bounds| {
                finite_positive_rect(&bounds)
                    && bounds.y() >= -0.01
                    && bounds.y() + bounds.height() <= paned.height() as f32 + 0.01
            })
            && terminal.has_focus()
            && runtime.drawable_geometry()
            && runtime.source.borrow().is_some()
    });
    // max-position is the allocated extent after subtracting the handle.
    // With both shrink flags true, max-position itself collapses the end
    // child. Leave one real pixel of its slot so it stays mapped instead.
    let clipped_position = paned.max_position() - 1;
    paned.set_position(clipped_position);
    wait("Paned clipped", &|| {
        paned.position() == clipped_position
            && paned.position() < paned.max_position()
            && root.is_mapped()
            && terminal.has_focus()
            && root.compute_bounds(&paned).is_some_and(|bounds| {
                finite_positive_rect(&bounds)
                    && bounds.y() >= 0.0
                    && bounds.y() < paned.height() as f32
            })
            && !runtime.drawable_geometry()
            && runtime.source.borrow().is_none()
    });
    assert!(
        root.is_mapped(),
        "the clipped child still has native mapping"
    );
    assert!(
        terminal.has_focus(),
        "only drawable geometry suppresses this animation"
    );
    assert!(runtime.geometry_idle.borrow().is_none());
    paned.set_position(10);
    wait("Paned restored", &|| {
        runtime.drawable_geometry() && runtime.source.borrow().is_some()
    });
    assert!(runtime.set_expanded(true));
    wait("expanded Paned visible", &|| {
        runtime.paint_plan(true, false).paint == Paint::ExpandedSprite
    });
    let expanded_height = runtime.row.measure(gtk4::Orientation::Vertical, -1);
    paned.set_position(paned.max_position() - 1);
    wait("expanded ancestor-only clip", &|| {
        runtime.paint_plan(true, false).paint == Paint::Hidden && runtime.source.borrow().is_none()
    });
    assert!(runtime.expanded.get());
    assert_eq!(
        expanded_height,
        runtime.row.measure(gtk4::Orientation::Vertical, -1)
    );
    paned.set_position(10);
    wait("expanded ancestor clip recovery", &|| {
        runtime.paint_plan(true, false).paint == Paint::ExpandedSprite
            && runtime.source.borrow().is_some()
    });
    runtime.apply_settings(true, OrganismMotion::Static);
    paned.set_position(paned.max_position() - 1);
    wait("static expanded clipped", &|| {
        runtime.paint_plan(true, false).paint == Paint::Hidden
            && runtime
                .expanded_widgets()
                .unwrap()
                .labels
                .iter()
                .all(|label| label.opacity() == 0.0)
            && runtime.geometry_idle.borrow().is_none()
    });
    assert!(runtime.source.borrow().is_none());
    paned.set_position(10);
    wait(
        "static ancestor translation recovers without timer",
        &|| {
            runtime.paint_plan(true, false).paint == Paint::ExpandedSprite
                && runtime
                    .expanded_widgets()
                    .unwrap()
                    .labels
                    .iter()
                    .any(|label| label.opacity() == 1.0)
                && runtime.geometry_idle.borrow().is_none()
        },
    );
    assert!(runtime.source.borrow().is_none());
    // Reuse the same native Paned and host. Frame time advances before
    // allocation/paint; wait for actual after-paint and stable geometry before
    // reading the orientation-dependent scan bound.
    let clock = paned.frame_clock().unwrap();
    let painted = Rc::new(Cell::new(0_u64));
    let observed_paints = painted.clone();
    struct PaintObserver(gtk4::gdk::FrameClock, Option<glib::SignalHandlerId>);
    impl Drop for PaintObserver {
        fn drop(&mut self) {
            if let Some(handler) = self.1.take() {
                self.0.disconnect(handler);
            }
        }
    }
    let paint_observer = PaintObserver(
        clock.clone(),
        Some(clock.connect_after_paint(move |_| {
            observed_paints.set(observed_paints.get().wrapping_add(1));
        })),
    );
    let geometry = || {
        let bounds = root.compute_bounds(&paned).unwrap();
        (
            paned.width(),
            paned.height(),
            paned.position(),
            paned.min_position(),
            paned.max_position(),
            bounds.x(),
            bounds.y(),
            bounds.width(),
            bounds.height(),
        )
    };
    let settle_native = |stage: &str| {
        let last_paint = Cell::new(painted.get());
        let previous_geometry = Cell::new(None);
        window.queue_draw();
        wait(stage, &|| {
            if painted.get() == last_paint.get() || runtime.geometry_idle.borrow().is_some() {
                return false;
            }
            last_paint.set(painted.get());
            let current = geometry();
            if previous_geometry.replace(Some(current)) == Some(current) {
                return true;
            }
            window.queue_draw();
            false
        });
    };
    let diagnostic = |stage: &str, requested: Option<i32>| {
        let widgets = runtime.expanded_widgets().unwrap();
        eprintln!(
            "VTE_HORIZONTAL stage={stage} requested={requested:?} orientation={:?} geometry={:?} paint={:?} clip={:?} full={:?} fallback={:?}",
            paned.orientation(), geometry(), runtime.paint_plan(true, false).paint,
            runtime.clipped_geometry(&runtime.presentation_widget()).as_ref().map(VteOrganismSurface::rect),
            runtime.candidate_union(&widgets.labels[..2]),
            runtime.candidate_union(&widgets.labels[2..]),
        );
    };
    diagnostic("before orientation", None);
    paned.set_orientation(gtk4::Orientation::Horizontal);
    paned.set_position(10);
    settle_native("horizontal orientation allocation settled");
    assert_eq!(paned.orientation(), gtk4::Orientation::Horizontal);
    assert!(paned.max_position() > 11 && paned.max_position() <= paned.width());
    assert!(root.compute_bounds(&paned).is_some_and(|bounds| {
        bounds.x() > 0.0 && bounds.y() == 0.0 && bounds.height() == paned.height() as f32
    }));
    wait("horizontal expanded full body", &|| {
        runtime.paint_plan(true, false).paint == Paint::ExpandedSprite
            && runtime.geometry_idle.borrow().is_none()
    });
    diagnostic("horizontal start", Some(10));
    let mut saw_fallback = false;
    let mut saw_hidden_after_fallback = false;
    let horizontal_height = runtime.row.measure(gtk4::Orientation::Vertical, -1);
    let assert_stable_paint = |expected: Paint| {
        for _ in 0..3 {
            settle_native("three-paint presentation stability");
            assert_eq!(runtime.paint_plan(true, false).paint, expected);
            let widgets = runtime.expanded_widgets().unwrap();
            match expected {
                Paint::ExpandedCompactFallback => {
                    assert!(widgets.labels[..2]
                        .iter()
                        .all(|label| label.opacity() == 0.0));
                    assert_eq!(
                        widgets.labels[2..]
                            .iter()
                            .filter(|label| label.opacity() == 1.0)
                            .count(),
                        1
                    );
                }
                Paint::Hidden => assert!(widgets.labels.iter().all(|label| label.opacity() == 0.0)),
                _ => unreachable!(),
            }
            assert!(runtime.source.borrow().is_none());
            assert_eq!(
                horizontal_height,
                runtime.row.measure(gtk4::Orientation::Vertical, -1)
            );
        }
    };
    let horizontal_max = paned.max_position();
    let last_plan = Cell::new(Paint::ExpandedSprite);
    for position in 11..horizontal_max {
        paned.set_position(position);
        settle_native("horizontal native allocation settled");
        let current_plan = runtime.paint_plan(true, false).paint;
        if last_plan.replace(current_plan) != current_plan || position == horizontal_max - 1 {
            diagnostic("scan transition or final position", Some(position));
        }
        assert_eq!(
            horizontal_height,
            runtime.row.measure(gtk4::Orientation::Vertical, -1)
        );
        assert!(
            runtime.source.borrow().is_none(),
            "Static never acquires a timer"
        );
        match runtime.paint_plan(true, false).paint {
            Paint::ExpandedCompactFallback => {
                if !saw_fallback {
                    assert_stable_paint(Paint::ExpandedCompactFallback);
                }
                saw_fallback = true;
            }
            Paint::Hidden if saw_fallback => {
                assert_stable_paint(Paint::Hidden);
                saw_hidden_after_fallback = true;
                break;
            }
            _ => {}
        }
    }
    assert!(
        saw_fallback,
        "real narrow pane must admit the five-character fallback"
    );
    assert!(
        saw_hidden_after_fallback,
        "further clipping hides paint without collapsing its envelope"
    );
    paned.set_position(10);
    settle_native("horizontal recovery allocation settled");
    wait("horizontal Static full-body recovery", &|| {
        runtime.paint_plan(true, false).paint == Paint::ExpandedSprite
            && runtime
                .expanded_widgets()
                .unwrap()
                .labels
                .iter()
                .any(|label| label.opacity() == 1.0)
    });
    // The same native horizontal fixture also tests compact partial ink.
    assert!(runtime.set_expanded(false));
    settle_native("compact horizontal allocation settled");
    let compact_size = (
        runtime.row.measure(gtk4::Orientation::Horizontal, -1),
        runtime.row.measure(gtk4::Orientation::Vertical, -1),
    );
    let mut saw_partial_compact = false;
    for position in (11..paned.max_position()).rev() {
        paned.set_position(position);
        settle_native("compact partial allocation settled");
        if runtime.drawable_geometry() && runtime.paint_plan(true, false).paint == Paint::Hidden {
            saw_partial_compact = true;
            for motion in [
                OrganismMotion::Full,
                OrganismMotion::Calm,
                OrganismMotion::Static,
            ] {
                runtime.apply_settings(true, motion);
                settle_native("compact partial settings settled");
                assert_eq!(runtime.paint_plan(true, false).paint, Paint::Hidden);
                assert_eq!(runtime.glyph.opacity(), 0.0);
                assert!(runtime.source.borrow().is_none());
                assert!(runtime.row.is_mapped());
                assert!(!runtime.expanded.get());
                assert_eq!(
                    compact_size,
                    (
                        runtime.row.measure(gtk4::Orientation::Horizontal, -1),
                        runtime.row.measure(gtk4::Orientation::Vertical, -1)
                    )
                );
            }
            for _ in 0..3 {
                clock.emit_by_name::<()>("after-paint", &[]);
                assert!(runtime.geometry_idle.borrow().is_none());
                assert_eq!(runtime.glyph.opacity(), 0.0);
            }
            break;
        }
    }
    assert!(
        saw_partial_compact,
        "compact positive intersection must still require complete ink"
    );
    runtime.apply_settings(true, OrganismMotion::Full);
    assert!(runtime.source.borrow().is_none());
    paned.set_position(10);
    settle_native("compact horizontal recovery settled");
    wait("compact Full recovery without manual refresh", &|| {
        runtime.paint_plan(true, false).paint == Paint::Compact
            && runtime.glyph.opacity() == 1.0
            && runtime.source.borrow().is_some()
    });
    assert_eq!(
        compact_size,
        (
            runtime.row.measure(gtk4::Orientation::Horizontal, -1),
            runtime.row.measure(gtk4::Orientation::Vertical, -1)
        )
    );
    runtime.apply_settings(true, OrganismMotion::Static);
    assert!(runtime.set_expanded(true));
    settle_native("expanded state restored before original cleanup");
    drop(paint_observer);
    paned.set_orientation(gtk4::Orientation::Vertical);
    paned.set_position(10);
    runtime.apply_settings(true, OrganismMotion::Full);
    assert!(runtime.set_expanded(false));
    paned.set_end_child(gtk4::Widget::NONE);
    root.set_size_request(-1, -1);
    fixture_host.set_content(Some(&root));
    terminal.grab_focus();
    wait("Paned removed", &|| {
        terminal.has_focus() && runtime.source.borrow().is_some()
    });

    // Actual native host activation and reparenting, still with no PTY.
    let second = gtk4::Window::builder()
        .default_width(400)
        .default_height(300)
        .child(&gtk4::Label::new(Some("Native host lifecycle check")))
        .build();
    second.present();
    wait("second host active", &|| {
        second.is_active() && !window.is_active()
    });
    assert!(runtime.source.borrow().is_none());
    assert!(runtime.paint.borrow().is_none());
    // Retire the old Root's focus before unparenting. GTK otherwise defers
    // old-window focus cleanup until after paint, when it could clear the
    // same terminal's new-window has-focus state.
    gtk4::prelude::RootExt::set_focus(window, gtk4::Widget::NONE);
    assert!(gtk4::prelude::RootExt::focus(window).is_none());
    fixture_host.set_content(gtk4::Widget::NONE);
    second.set_child(Some(&root));
    wait("pane mapped in second host", &|| {
        root.is_mapped()
            && terminal.is_mapped()
            && root.root().as_ref() == Some(second.upcast_ref::<gtk4::Root>())
            && root.width() > 0
            && root.height() > 0
    });
    assert!(terminal.grab_focus());
    wait("pane moved to second host", &|| {
        terminal.has_focus() && runtime.source.borrow().is_some()
    });
    let current_source = runtime.source.borrow().as_ref().unwrap().as_raw();
    window.notify("is-active");
    assert_eq!(
        runtime.source.borrow().as_ref().unwrap().as_raw(),
        current_source,
        "retired host notifications cannot reschedule the current pane"
    );
    // This first host is now empty and inactive. Remap it normally with its
    // returning child instead of presenting an already-visible empty X11
    // window with a potentially stale last-user-interaction timestamp.
    assert!(fixture_host.content().is_none());
    assert!(!window.is_active());
    window.set_visible(false);
    assert!(second.is_active());
    assert!(terminal.has_focus());
    assert_eq!(
        runtime.source.borrow().as_ref().unwrap().as_raw(),
        current_source,
        "hiding the retired host cannot affect the active pane"
    );
    gtk4::prelude::RootExt::set_focus(&second, gtk4::Widget::NONE);
    assert!(gtk4::prelude::RootExt::focus(&second).is_none());
    second.set_child(gtk4::Widget::NONE);
    assert!(runtime.source.borrow().is_none());
    assert!(runtime.paint.borrow().is_none());
    assert!(runtime.geometry_idle.borrow().is_none());
    fixture_host.set_content(Some(&root));
    assert_eq!(
        root.root().as_ref(),
        Some(window.upcast_ref::<gtk4::Root>())
    );
    window.present();
    wait("pane mapped back in first host", &|| {
        window.is_active()
            && !second.is_active()
            && root.is_mapped()
            && terminal.is_mapped()
            && root.root().as_ref() == Some(window.upcast_ref::<gtk4::Root>())
            && root.width() > 0
            && root.height() > 0
    });
    assert!(terminal.grab_focus());
    wait("pane returned to first host", &|| {
        terminal.has_focus() && runtime.source.borrow().is_some()
    });
    let current_source = runtime.source.borrow().as_ref().unwrap().as_raw();
    second.notify("is-active");
    assert_eq!(
        runtime.source.borrow().as_ref().unwrap().as_raw(),
        current_source,
        "the second retired host cannot reschedule the returned pane"
    );
    second.close();
    runtime.set_local(false);
    assert!(!runtime.row.is_visible());
    assert!(runtime.source.borrow().is_none());
    runtime.apply_settings(true, OrganismMotion::Full);
    assert!(
        !runtime.row.is_visible(),
        "config cannot override remote identity"
    );
    runtime.set_local(true);
    wait("local identity restored", &|| {
        runtime.row.is_mapped() && runtime.source.borrow().is_some()
    });
    runtime.apply_settings(false, OrganismMotion::Full);
    let weak_reentrant = Rc::downgrade(&runtime);
    let handler = runtime.row.connect_visible_notify(move |row| {
        if row.is_visible() {
            if let Some(runtime) = weak_reentrant.upgrade() {
                runtime.apply_settings(false, OrganismMotion::Full);
            }
        }
    });
    runtime.apply_settings(true, OrganismMotion::Full);
    assert!(!runtime.enabled.get());
    assert!(!runtime.row.is_visible());
    assert_eq!(
        runtime.row.opacity(),
        0.0,
        "outer refresh cannot repaint after settings reentry"
    );
    assert!(
        runtime.source.borrow().is_none(),
        "retired refresh cannot install a late frame source"
    );
    assert!(runtime.geometry_idle.borrow().is_none());
    runtime.row.disconnect(handler);
    runtime.apply_settings(true, OrganismMotion::Full);
    wait("active sources before Drop", &|| {
        runtime.source.borrow().is_some() && runtime.paint.borrow().is_some()
    });
    runtime.defer_geometry_refresh();
    let pending_frame = main
        .find_source_by_id(&runtime.source.borrow().as_ref().unwrap().id)
        .unwrap();
    let pending_geometry = main
        .find_source_by_id(&runtime.geometry_idle.borrow().as_ref().unwrap().id)
        .unwrap();
    assert!(!pending_frame.is_destroyed());
    assert!(!pending_geometry.is_destroyed());
    let old_row = runtime.row.clone();
    let weak = Rc::downgrade(&runtime);
    drop(runtime);
    assert!(
        weak.upgrade().is_none(),
        "signals and source do not own the adapter"
    );
    assert!(pending_frame.is_destroyed());
    assert!(pending_geometry.is_destroyed());
    assert!(terminal.pty().is_none());
    // Original Drop assertions above keep their live-source preconditions.
    // Reuse only the same existing widgets for the final retirement case.
    root.remove(&old_row);
    let retiring = VteOrganismSurface::new(&root, &terminal, &config);
    retiring.set_local(true);
    terminal.grab_focus();
    wait("retirement case admitted in existing host", &|| {
        retiring.row.is_mapped() && retiring.source.borrow().is_some()
    });
    let fired = Rc::new(Cell::new(false));
    let observed = fired.clone();
    let weak = Rc::downgrade(&retiring);
    let handler = retiring.glyph.connect_parent_notify(move |glyph| {
        if glyph.parent().is_none() && !observed.replace(true) {
            if let Some(runtime) = weak.upgrade() {
                runtime.retire();
            }
        }
    });
    assert!(retiring.set_expanded(true));
    assert!(fired.get());
    assert!(!retiring.alive.get());
    assert!(!retiring.mounting.get());
    assert!(retiring.expanded_widgets().unwrap().slot.parent().is_none());
    for enabled in [false, true] {
        retiring.apply_settings(enabled, OrganismMotion::Full);
        retiring.set_local(true);
        assert!(retiring.expanded_widgets().unwrap().slot.parent().is_none());
        assert!(retiring.source.borrow().is_none());
        assert!(retiring.geometry_idle.borrow().is_none());
        assert!(retiring.paint.borrow().is_none());
        assert!(!retiring.set_expanded(false));
    }
    retiring.glyph.disconnect(handler);
    let row = retiring.row.clone();
    let weak = Rc::downgrade(&retiring);
    drop(retiring);
    assert!(weak.upgrade().is_none());
    root.remove(&row);
    assert!(terminal.pty().is_none());
    lifecycle_tests::exercise(window, &main);
    exercise_unique_split_geometry(window, &main);
}

#[cfg(test)]
fn exercise_unique_split_geometry(window: &gtk4::Window, main: &glib::MainContext) {
    use libadwaita::prelude::*;
    let host = window.downcast_ref::<libadwaita::Window>().unwrap();
    let original = host.content();
    host.set_content(gtk4::Widget::NONE);
    let mut config = Config::safe_defaults();
    config.ascii_organism_enabled = true;
    config.ascii_organism_motion = Some(OrganismMotion::Static);
    let roots = [
        gtk4::Box::new(gtk4::Orientation::Vertical, 0),
        gtk4::Box::new(gtk4::Orientation::Vertical, 0),
    ];
    let terminals = [vte4::Terminal::new(), vte4::Terminal::new()];
    let surfaces: Vec<_> = roots
        .iter()
        .zip(&terminals)
        .map(|(root, terminal)| {
            terminal.set_hexpand(true);
            terminal.set_vexpand(true);
            root.append(terminal);
            let surface = VteOrganismSurface::new(root, terminal, &config);
            surface.set_local(true);
            surface
        })
        .collect();
    let split = gtk4::Paned::new(gtk4::Orientation::Horizontal);
    split.set_start_child(Some(&roots[0]));
    split.set_end_child(Some(&roots[1]));
    host.set_content(Some(&split));
    let settle = || {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(100) {
            while main.iteration(false) {}
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    window.present();
    settle();
    split.set_position(window.width() / 2);
    assert!(terminals[0].grab_focus());
    settle();
    let grids = || {
        terminals.each_ref().map(|terminal| {
            (
                terminal.width(),
                terminal.height(),
                terminal.column_count(),
                terminal.row_count(),
            )
        })
    };
    let initial_grids = grids();
    for owner in [1, 0, 1, 0] {
        assert!(terminals[owner].grab_focus());
        settle();
        assert_eq!(surfaces[owner].row.opacity(), 1.0);
        assert_eq!(
            surfaces[1 - owner].row.opacity(),
            0.0,
            "non-owner title and body are both absent"
        );
        assert_eq!(
            grids(),
            initial_grids,
            "focus cannot resize or reflow either VTE grid"
        );
        assert!(surfaces
            .iter()
            .all(|surface| surface.source.borrow().is_none()));
        assert!(terminals.iter().all(|terminal| terminal.pty().is_none()));
    }
    println!("VTE_UNIQUE_FOCUS swaps=4 complete_non_owner_row_hidden=true grid_pixels_and_rows_columns_unchanged=true no_pty=true");
    for surface in &surfaces {
        surface.retire();
    }
    drop(surfaces);
    host.set_content(original.as_ref());
}

#[cfg(test)]
fn exercise_expanded_fixture(
    runtime: &Rc<VteOrganismSurface>,
    terminal: &vte4::Terminal,
    other: &gtk4::Entry,
    window: &gtk4::Window,
    main: &glib::MainContext,
    wait: &dyn Fn(&str, &dyn Fn() -> bool),
) {
    fn assert_hidden(accessible: &impl IsA<gtk4::Accessible>, expected: bool) {
        use glib::translate::{from_glib_full, IntoGlib, ToGlibPtr};
        // GTK's public gtktestatcontext.h assertion macro treats null as a
        // match. Hidden is gboolean, promoted to C int in the variadic ABI.
        let mismatch: Option<glib::GString> = unsafe {
            from_glib_full(gtk4::ffi::gtk_test_accessible_check_state(
                accessible.as_ref().to_glib_none().0,
                gtk4::AccessibleState::Hidden.into_glib(),
                i32::from(expected),
            ))
        };
        assert!(mismatch.is_none(), "Hidden value mismatch: {mismatch:?}");
    }
    fn assert_presentation_leaves(widgets: &ExpandedCompanion) {
        for label in &widgets.labels {
            assert!(
                label.is_visible(),
                "all candidates still participate in measurement"
            );
            assert_eq!(label.accessible_role(), gtk4::AccessibleRole::Presentation);
            assert!(gtk4::test_accessible_has_role(
                label,
                gtk4::AccessibleRole::Presentation
            ));
            assert!(
                label.first_accessible_child().is_none(),
                "decorative leaves have no descendants to promote"
            );
        }
    }
    let compact = (terminal.width(), terminal.height(), runtime.row.height());
    let compact_req = (
        runtime.row.measure(gtk4::Orientation::Horizontal, -1),
        runtime.row.measure(gtk4::Orientation::Vertical, -1),
    );
    assert!(!runtime.expanded.get());
    assert_eq!(runtime.glyph.width_chars(), 5);
    assert_eq!(runtime.glyph.max_width_chars(), 5);
    assert_eq!(runtime.glyph.height_request(), -1);
    let reversed = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(runtime);
    let observed = reversed.clone();
    let handler = runtime.glyph.connect_parent_notify(move |glyph| {
        if glyph.parent().is_none() && !observed.replace(true) {
            if let Some(runtime) = weak.upgrade() {
                assert!(runtime.set_expanded(false));
            }
        }
    });
    assert!(runtime.set_expanded(true));
    wait("opposite parent-notify action reconciled", &|| {
        !runtime.expanded.get()
            && !runtime.mounting.get()
            && runtime.glyph.parent().as_ref() == Some(runtime.row.upcast_ref())
            && runtime.expanded_widgets().unwrap().slot.parent().is_none()
            && runtime.geometry_idle.borrow().is_none()
    });
    assert!(reversed.get());
    runtime.glyph.disconnect(handler);
    let changed = Rc::new(Cell::new(false));
    let observed = changed.clone();
    let weak = Rc::downgrade(runtime);
    let handler = runtime.glyph.connect_parent_notify(move |glyph| {
        if glyph.parent().is_none() && !observed.replace(true) {
            if let Some(runtime) = weak.upgrade() {
                runtime.apply_settings(false, OrganismMotion::Full);
            }
        }
    });
    assert!(runtime.set_expanded(true));
    wait("settings reentry settles hidden expanded topology", &|| {
        runtime.expanded.get()
            && !runtime.mounting.get()
            && runtime.glyph.parent().is_none()
            && runtime.expanded_widgets().unwrap().slot.parent().as_ref()
                == Some(runtime.row.upcast_ref())
            && !runtime.row.is_visible()
            && runtime.source.borrow().is_none()
            && runtime.geometry_idle.borrow().is_none()
    });
    assert!(changed.get());
    runtime.glyph.disconnect(handler);
    runtime.apply_settings(true, OrganismMotion::Full);
    assert!(runtime.set_expanded(false));
    terminal.grab_focus();
    wait("compact after reentrant setting", &|| {
        (terminal.width(), terminal.height(), runtime.row.height()) == compact
            && runtime.source.borrow().is_some()
    });
    // Controller + captured native target composition, not a whole-app UiState
    // launch. Production uses this exact controller and validates UiState's
    // current pane separately; source contracts cover that outer wiring.
    {
        use libadwaita::prelude::*;
        for (key, target_current, requested, expected) in [
            (gtk4::gdk::Key::Escape, true, true, false),
            (gtk4::gdk::Key::Return, false, true, false),
            (gtk4::gdk::Key::Return, true, true, true),
            (gtk4::gdk::Key::Return, true, false, false),
        ] {
            let dialog = libadwaita::Dialog::new();
            let list = gtk4::ListBox::new();
            list.append(&gtk4::Label::new(Some("neutral presentation action")));
            list.select_row(list.row_at_index(0).as_ref());
            let filter = gtk4::SearchEntry::new();
            let body = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            body.append(&filter);
            body.append(&list);
            dialog.set_child(Some(&body));
            let slot = Rc::new(RefCell::new(Some(dialog.clone())));
            let captured = Rc::downgrade(runtime);
            let current = target_current.then(|| runtime.clone());
            let host = window.clone();
            let closing = dialog.downgrade();
            let focus_terminal = terminal.clone();
            let accepted = Rc::new(Cell::new(false));
            let accepted_by_callback = accepted.clone();
            let activate = Rc::new(move |_index| {
                let Some(closing) = closing.upgrade() else {
                    return;
                };
                closing.force_close();
                let applied = VteOrganismSurface::present_for_target(
                    &captured,
                    current.as_ref(),
                    &host,
                    requested,
                );
                accepted_by_callback.set(applied);
                if applied {
                    focus_terminal.grab_focus();
                }
            });
            let queried = Rc::new(Cell::new(false));
            let query_callback = queried.clone();
            let keys = super::command_palette::palette_controller(
                crate::palette::PaletteMode::All,
                &list,
                &dialog,
                slot.clone(),
                &filter,
                Rc::new(move |_query| query_callback.set(true)),
                activate,
            );
            dialog.add_controller(keys.clone());
            let closed_slot = slot.clone();
            dialog.connect_closed(move |_| *closed_slot.borrow_mut() = None);
            let weak_dialog = dialog.downgrade();
            let weak_keys = keys.downgrade();
            let weak_list = list.downgrade();
            let weak_filter = filter.downgrade();
            let weak_row = list.row_at_index(0).unwrap().downgrade();
            dialog.present(Some(window));
            // Adw 1.5 maps before its second-frame open/focus handoff. A
            // synthetic key signal must wait for the same actual focus a real
            // keyboard event needs, rather than closing a not-yet-open sheet.
            wait("palette controller has native keyboard focus", &|| {
                dialog.is_mapped()
                    && window.is_active()
                    && gtk4::prelude::RootExt::focus(window)
                        .is_some_and(|focus| focus == filter || focus.is_ancestor(&filter))
            });
            assert!(keys.emit_by_name::<bool>(
                "key-pressed",
                &[&key, &0_u32, &gtk4::gdk::ModifierType::empty(),]
            ));
            assert_eq!(queried.get(), key == gtk4::gdk::Key::Return);
            assert_eq!(
                accepted.get(),
                target_current && key == gtk4::gdk::Key::Return
            );
            // Each case owns one opening/closing lifecycle. Do not overlap
            // dialogs or repair focus after close; production's immediate
            // focus restoration and the real host lifecycle must suffice.
            wait("palette original dialog fully removed", &|| {
                !dialog.is_mapped() && dialog.parent().is_none()
            });
            assert!(
                terminal.has_focus(),
                "palette close must restore the terminal"
            );
            wait("palette target action settled", &|| {
                runtime.expanded.get() == expected
                    && !runtime.mounting.get()
                    && runtime.geometry_idle.borrow().is_none()
            });
            assert!(
                slot.borrow().is_none(),
                "closed clears the production-style slot"
            );
            // Release only the fixture's local owners. Native ownership and
            // callback lifetimes must unwind without detaching the controller.
            drop(keys);
            drop(dialog);
            drop(list);
            drop(filter);
            drop(body);
            drop(slot);
            wait("palette closed widget graph released", &|| {
                weak_dialog.upgrade().is_none()
                    && weak_keys.upgrade().is_none()
                    && weak_list.upgrade().is_none()
                    && weak_filter.upgrade().is_none()
                    && weak_row.upgrade().is_none()
            });
        }
        wait("compact restored after palette controller cases", &|| {
            (terminal.width(), terminal.height(), runtime.row.height()) == compact
                && runtime.source.borrow().is_some()
        });
    }
    let target = Rc::downgrade(runtime);
    assert!(!VteOrganismSurface::present_for_target(
        &target, None, window, true
    ));
    assert!(
        !runtime.expanded.get(),
        "a switched current pane is not a target"
    );
    assert!(VteOrganismSurface::present_for_target(
        &target,
        Some(runtime),
        window,
        true
    ));
    wait("expanded measured slot", &|| {
        runtime.row.height() > compact.2
            && runtime.paint_plan(true, false).paint == Paint::ExpandedSprite
    });
    let widgets = runtime.expanded_widgets().unwrap();
    assert!(!widgets.slot.can_target());
    assert!(!widgets.slot.is_focusable());
    assert_eq!(terminal.width(), compact.0);
    let expanded = (terminal.width(), terminal.height(), runtime.row.height());
    assert_eq!(compact.1 - expanded.1, expanded.2 - compact.2);
    let requisition = (
        widgets.slot.measure(gtk4::Orientation::Horizontal, -1),
        widgets.slot.measure(gtk4::Orientation::Vertical, -1),
    );
    for label in &widgets.labels[1..] {
        assert!(widgets.slot.is_measure_overlay(label));
        assert!(widgets.slot.is_clip_overlay(label));
    }
    assert_presentation_leaves(&widgets);
    assert_eq!(
        requisition,
        (
            widgets.slot.measure(gtk4::Orientation::Horizontal, -1),
            widgets.slot.measure(gtk4::Orientation::Vertical, -1)
        )
    );
    assert!(gtk4::test_accessible_has_property(
        &runtime.capability,
        gtk4::AccessibleProperty::Description
    ));
    assert_ne!(
        runtime.capability.accessible_role(),
        gtk4::AccessibleRole::Presentation
    );
    assert_ne!(
        runtime.capability.accessible_role(),
        gtk4::AccessibleRole::None
    );
    assert!(runtime.capability.is_mapped());
    assert_hidden(&runtime.capability, false);
    assert!(VteOrganismSurface::present_for_target(
        &target,
        Some(runtime),
        window,
        true
    ));
    assert_eq!(
        requisition,
        (
            widgets.slot.measure(gtk4::Orientation::Horizontal, -1),
            widgets.slot.measure(gtk4::Orientation::Vertical, -1)
        )
    );
    for frame in 0..120 {
        let ticket = runtime.refresh_ticket.borrow().clone();
        runtime.paint_expanded(Paint::ExpandedSprite, frame, &ticket);
        assert_eq!(
            requisition,
            (
                widgets.slot.measure(gtk4::Orientation::Horizontal, -1),
                widgets.slot.measure(gtk4::Orientation::Vertical, -1)
            )
        );
        runtime.paint_expanded(Paint::ExpandedCompactFallback, frame, &ticket);
        assert_eq!(
            expanded,
            (terminal.width(), terminal.height(), runtime.row.height())
        );
    }
    terminal.emit_by_name::<()>("commit", &[&"x", &1_u32]);
    assert!(runtime.expanded.get());
    assert!(widgets.labels.iter().all(|label| label.opacity() == 0.0));
    while main.iteration(false) {}
    assert_eq!(
        expanded,
        (terminal.width(), terminal.height(), runtime.row.height())
    );
    assert!(other.grab_focus());
    wait("expanded focus retreat", &|| {
        !terminal.has_focus() && runtime.source.borrow().is_none()
    });
    assert_eq!(
        expanded,
        (terminal.width(), terminal.height(), runtime.row.height())
    );
    runtime.last_input.set(None);
    assert!(terminal.grab_focus());
    wait("expanded focus restored", &|| {
        terminal.has_focus() && runtime.source.borrow().is_some()
    });
    for motion in [
        OrganismMotion::Calm,
        OrganismMotion::Static,
        OrganismMotion::Full,
    ] {
        runtime.apply_settings(true, motion);
        assert!(runtime.expanded.get());
        assert_eq!(
            expanded,
            (terminal.width(), terminal.height(), runtime.row.height())
        );
        assert_eq!(
            runtime.source.borrow().is_some(),
            motion == OrganismMotion::Full
        );
    }
    runtime.apply_settings(true, OrganismMotion::Static);
    assert!(runtime.source.borrow().is_none());
    let attrs = gtk4::pango::AttrList::new();
    attrs.insert(gtk4::pango::AttrSize::new(24 * gtk4::pango::SCALE));
    attrs.insert(gtk4::pango::AttrInt::new_letter_spacing(
        2 * gtk4::pango::SCALE,
    ));
    for label in &widgets.labels {
        label.set_direction(gtk4::TextDirection::Rtl);
        label.set_attributes(Some(&attrs));
    }
    wait("expanded font automatically remeasured", &|| {
        runtime.row.height() > expanded.2
    });
    assert!(runtime.source.borrow().is_none());
    let font_height = runtime.row.height();
    for frame in 0..120 {
        let ticket = runtime.refresh_ticket.borrow().clone();
        runtime.paint_expanded(Paint::ExpandedSprite, frame, &ticket);
        assert_eq!(runtime.row.height(), font_height);
    }
    for label in &widgets.labels {
        label.set_direction(gtk4::TextDirection::Ltr);
        label.set_attributes(None);
    }
    wait("expanded original metrics restored", &|| {
        runtime.row.height() == expanded.2
    });
    wait("static fit restored without animation timer", &|| {
        runtime.paint_plan(true, false).paint == Paint::ExpandedSprite
            && widgets.labels.iter().any(|label| label.opacity() == 1.0)
            && runtime.geometry_idle.borrow().is_none()
    });
    assert!(runtime.source.borrow().is_none());
    runtime.set_local(false);
    assert!(runtime.expanded.get());
    assert!(!VteOrganismSurface::present_for_target(
        &target,
        Some(runtime),
        window,
        false
    ));
    runtime.set_local(true);
    runtime.apply_settings(false, OrganismMotion::Full);
    assert!(runtime.expanded.get());
    assert!(!VteOrganismSurface::present_for_target(
        &target,
        Some(runtime),
        window,
        false
    ));
    runtime.apply_settings(true, OrganismMotion::Full);
    terminal.grab_focus();
    wait("expanded admission restored", &|| {
        runtime.row.is_mapped() && runtime.source.borrow().is_some()
    });
    assert!(VteOrganismSurface::present_for_target(
        &target,
        Some(runtime),
        window,
        false
    ));
    wait("exact compact allocation restored", &|| {
        (terminal.width(), terminal.height(), runtime.row.height()) == compact
    });
    assert_eq!(
        compact_req,
        (
            runtime.row.measure(gtk4::Orientation::Horizontal, -1),
            runtime.row.measure(gtk4::Orientation::Vertical, -1)
        )
    );
    assert_presentation_leaves(&widgets);
    assert!(VteOrganismSurface::present_for_target(
        &target,
        Some(runtime),
        window,
        true
    ));
    wait("presentation role survives expanded remap", &|| {
        widgets.slot.is_mapped() && runtime.row.height() == expanded.2
    });
    assert_presentation_leaves(&widgets);
    assert_eq!(
        requisition,
        (
            widgets.slot.measure(gtk4::Orientation::Horizontal, -1),
            widgets.slot.measure(gtk4::Orientation::Vertical, -1)
        )
    );
    assert!(VteOrganismSurface::present_for_target(
        &target,
        Some(runtime),
        window,
        false
    ));
    wait("compact restored after accessibility remap", &|| {
        (terminal.width(), terminal.height(), runtime.row.height()) == compact
    });
    assert_eq!(runtime.glyph.height_request(), -1);
    assert_eq!(runtime.glyph.width_chars(), 5);
    assert_eq!(runtime.glyph.max_width_chars(), 5);
    assert!(VteOrganismSurface::present_for_target(
        &target,
        Some(runtime),
        window,
        false
    ));
    runtime.last_input.set(None);
    runtime.refresh();
}

#[cfg(test)]
mod lifecycle_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires DISPLAY"]
    fn one_companion_moves_between_vte_splits_without_resizing_grids() {
        libadwaita::init().expect("GTK/libadwaita display");
        let window = libadwaita::Window::builder()
            .default_width(1000)
            .default_height(500)
            .build();
        exercise_unique_split_geometry(window.upcast_ref(), &glib::MainContext::default());
        window.close();
    }

    #[test]
    fn compact_native_path_checks_exact_pose_and_observes_passive_recovery() {
        let source = include_str!("organism_vte.rs");
        let production = source
            .split("#[cfg(test)]\npub(super) fn exercise_existing_native_fixture")
            .next()
            .unwrap();
        let refresh = production.split("    fn refresh(").nth(1).unwrap();
        assert!(
            refresh.find("self.glyph.set_text").unwrap()
                < refresh.find("let plan = self.paint_plan").unwrap()
        );
        assert!(
            refresh.find("self.bind_paint(eligible)").unwrap()
                < refresh.find("let drawable =").unwrap()
        );
        assert!(refresh.contains("let wake_eligible = eligible && self.paint_plan(eligible, false).paint != Paint::Hidden"));
        let stamp = production
            .split("    fn geometry_signature(")
            .nth(1)
            .unwrap()
            .split("    fn append_geometry_stamp(")
            .next()
            .unwrap();
        assert!(stamp.contains("std::slice::from_ref(&self.glyph)"));
        assert!(!stamp.contains(".opacity("));
        assert!(!stamp.contains(".set_opacity("));
        assert!(production.contains("self.candidate_bounds(&self.glyph)"));
        assert!(source.contains("compact Full recovery without manual refresh"));
    }

    #[test]
    fn neutral_motion_preserves_full_frames_and_static_modes() {
        let elapsed = SURFACE_FRAME_INTERVAL * 17;
        assert_eq!(neutral_frame(OrganismMotion::Full, elapsed), 17);
        assert_eq!(neutral_frame(OrganismMotion::Calm, elapsed), 0);
        assert_eq!(neutral_frame(OrganismMotion::Static, elapsed), 0);
        assert_eq!(
            neutral_wake(true, OrganismMotion::Full, elapsed, None),
            Some(SURFACE_FRAME_INTERVAL)
        );
        for motion in [OrganismMotion::Calm, OrganismMotion::Static] {
            assert_eq!(neutral_wake(true, motion, elapsed, None), None);
        }
    }

    #[test]
    fn neutral_retreat_uses_absolute_deadline_and_hidden_has_no_wake() {
        let at = Duration::from_secs(5);
        for motion in [
            OrganismMotion::Full,
            OrganismMotion::Calm,
            OrganismMotion::Static,
        ] {
            assert_eq!(neutral_wake(false, motion, at, Some(at)), None);
            assert_eq!(
                neutral_wake(true, motion, at, Some(at)),
                Some(HUMAN_INPUT_RETREAT)
            );
            assert_eq!(
                neutral_wake(
                    true,
                    motion,
                    at + HUMAN_INPUT_RETREAT - Duration::from_nanos(1),
                    Some(at)
                ),
                Some(Duration::from_millis(1))
            );
            let after = neutral_wake(true, motion, at + HUMAN_INPUT_RETREAT, Some(at));
            assert_eq!(
                after,
                (motion == OrganismMotion::Full).then_some(SURFACE_FRAME_INTERVAL)
            );
            assert_eq!(
                neutral_wake(true, motion, at + Duration::from_secs(600), Some(at)),
                after
            );
        }
    }

    #[test]
    fn neutral_geometry_rejects_unknown_and_empty_allocations() {
        assert!(finite_positive_geometry(-4.0, 2.0, 5.0, 10.0));
        for (x, y, width, height) in [
            (0.0, 0.0, 0.0, 10.0),
            (0.0, 0.0, 10.0, -1.0),
            (f32::NAN, 0.0, 10.0, 10.0),
            (0.0, f32::INFINITY, 10.0, 10.0),
            (0.0, 0.0, f32::INFINITY, 10.0),
        ] {
            assert!(!finite_positive_geometry(x, y, width, height));
        }
    }

    #[test]
    fn neutral_glyph_frames_keep_the_reserved_five_character_slot() {
        let context = RenderContext::new(Behavior::Idle, BodyLanguage::default(), false);
        for frame in 0..240 {
            let glyph = sticky_glyph_with_context(context, frame);
            assert!(glyph.is_ascii());
            assert_eq!(glyph.len(), 5);
        }
    }

    #[test]
    fn neutral_hello_rejection_keeps_the_original_target_and_no_owner_mutation() {
        let source = include_str!("organism.rs");
        let target = source
            .split_once("    pub(super) fn organism_greeting_target(")
            .unwrap()
            .1
            .split_once("    pub(crate) fn greet_ascii_organism_for(")
            .unwrap()
            .0;
        assert!(target.contains("self.current_pane_leaf()"));
        assert!(target.contains("OrganismGreetingTarget::Vte(view.widget().downgrade())"));
        let request = source
            .split_once("    pub(crate) fn greet_ascii_organism_for(")
            .unwrap()
            .1
            // Only this rustfmt-indented method owns the greeting contract;
            // adjacent inspector methods legitimately resolve current panes.
            .split_once("\n    }\n")
            .unwrap()
            .0;
        assert!(request.contains("runtime.request_greeting(&view)"));
        assert!(!request.contains("pub(crate) fn "));
        assert!(!request.contains("current_pane"));
        assert!(!request.contains("current_term_view"));
        let rejection = request
            .split_once("// The palette has already closed.")
            .unwrap()
            .0;
        assert!(rejection.contains("root.is_ancestor(&self.window)"));
        assert!(rejection.contains("VTE_ORGANISM_HELLO_UNAVAILABLE"));
        assert!(!rejection.contains("sync_organism_presence"));
        assert!(!rejection.contains("request_greeting"));
        let settings = source
            .split_once("    pub(crate) fn sync_ascii_organism_settings(")
            .unwrap()
            .1
            .split_once("    pub(crate) fn attach_ascii_organism_to_view(")
            .unwrap()
            .0;
        assert!(settings.contains("self.configuration_leaves()"));
        assert!(settings.contains("view.sync_organism(enabled, motion)"));
    }

    #[test]
    fn neutral_local_admission_follows_authoritative_pane_identity() {
        let leaf = include_str!("pane_leaf.rs");
        let set_remote = leaf
            .split_once("    pub(crate) fn set_remote(")
            .unwrap()
            .1
            .split_once("    pub(crate) fn is_remote(")
            .unwrap()
            .0;
        assert!(
            set_remote.find("set_data::<bool>").unwrap()
                < set_remote.find("view.set_organism_local(!remote)").unwrap()
        );
        let source = include_str!("organism_vte.rs");
        let production = source
            .split_once("#[cfg(test)]\npub(super) fn exercise_existing_native_fixture")
            .unwrap()
            .0;
        assert!(production.contains("local: Cell::new(false)"));
        let settings = production
            .split_once("    pub(crate) fn apply_settings(")
            .unwrap()
            .1
            .split_once("    pub(crate) fn retire(")
            .unwrap()
            .0;
        assert!(!settings.contains("self.local.set"));
        assert!(production
            .contains(".set_visible(self.enabled.get() && self.local.get() && self.alive.get())"));
        assert!(production.contains("self.row.set_opacity(if eligible { 1.0 } else { 0.0 })"));
    }

    #[test]
    fn neutral_adapter_keeps_unknown_capabilities_and_weak_native_lifetime() {
        let source = include_str!("organism_vte.rs");
        let production = source
            .split_once("#[cfg(test)]\npub(super) fn exercise_existing_native_fixture")
            .unwrap()
            .0;
        assert!(!production.contains("contents_changed"));
        assert!(!production.contains("feed_child"));
        assert!(!production.contains("LiveAttention::new"));
        assert!(!production.contains("GentleInteraction"));
        assert!(production.contains("Rc::downgrade"));
        assert!(production.contains("root.connect_root_notify"));
        assert!(production.contains("root.connect_unmap"));
        assert!(production.contains("host.disconnect(handler)"));
        assert!(production.contains("consume_source(&runtime.source, &callback_ticket)"));
        assert!(production.contains("consume_source(&runtime.geometry_idle, &callback_ticket)"));
        assert!(production.contains("self.current_refresh(&refresh_ticket)"));
        assert_eq!(production.matches("timeout_add_local_once").count(), 1);
        assert_eq!(production.matches("idle_add_local_once").count(), 1);
        assert!(production.contains("if self.geometry_idle.borrow().is_some()"));
        assert!(!production.contains("add_tick_callback"));
        assert!(!production.contains("request_phase"));
        assert!(!production.contains("begin_updating"));
        assert!(!production.contains("queue_draw"));
        let paint = production
            .split_once("clock.connect_after_paint(")
            .unwrap()
            .1
            .split_once("*self.paint.borrow_mut()")
            .unwrap()
            .0;
        assert!(paint.contains("runtime.drawable.replace(drawable) != drawable"));
        assert!(paint.contains("runtime.defer_geometry_refresh()"));
        assert!(!paint.contains("runtime.refresh()"));
        assert!(production
            .contains(".set_visible(self.enabled.get() && self.local.get() && self.alive.get())"));
        assert!(production.contains("self.row.set_opacity(if eligible { 1.0 } else { 0.0 })"));
        assert!(production.contains("self.glyph.set_opacity"));
        assert!(production.contains("terminal.connect_commit"));
        assert!(production.contains("host.is_active()"));
        assert!(production.contains("terminal.has_focus()"));
    }
    #[test]
    fn neutral_expansion_is_palette_only_with_original_weak_target() {
        let keys = include_str!("../keybindings.rs");
        assert!(keys.contains("ExpandVteCompanion | Action::CollapseVteCompanion => None"));
        let palette = include_str!("command_palette.rs");
        assert!(palette.contains("let vte_presentation_target = self.vte_companion_target()"));
        assert!(palette.contains("&vte_presentation_target"));
        let source = include_str!("organism_vte.rs");
        let production = source
            .split_once("#[cfg(test)]\npub(super) fn exercise_existing_native_fixture")
            .unwrap()
            .0;
        assert!(production.contains("presentation::resolve_current(target, current)"));
        assert!(production.contains("terminal.has_focus()"));
        assert!(production.contains("Rc::ptr_eq(&view, &current_view)"));
    }

    #[test]
    fn neutral_expansion_uses_four_native_measured_painted_candidates() {
        let source = include_str!("organism_vte.rs");
        let production = source
            .split_once("#[cfg(test)]\npub(super) fn exercise_existing_native_fixture")
            .unwrap()
            .0;
        assert!(production.contains("labels: [gtk4::Label; 4]"));
        assert!(production.contains("gtk4::Overlay::new()"));
        assert!(production.contains("slot.set_measure_overlay(label, true)"));
        assert!(production.contains("slot.set_clip_overlay(label, true)"));
        assert!(!production.contains("gtk4::accessible::State::Hidden(true)"));
        assert!(production.contains("gtk4::AccessibleRole::Presentation"));
        assert!(!production.contains("gtk4::Grid"));
        assert!(!production.contains("set_height_request"));
        assert!(!production.contains("set_size_request"));
        assert!(production.contains("candidate_union(&widgets.labels[..2])"));
        assert!(production.contains("candidate_union(&widgets.labels[2..])"));
    }
}
