//! A neutral, grid-external presentation for the traditional VTE backend.
//! VTE does not expose trustworthy prompt or alternate-screen state here.
//! Consequently this adapter renders no command reactions and admits no Hello.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::glib;
use gtk4::prelude::*;
use jterm_core::organism::{sticky_glyph_with_context, Behavior, BodyLanguage, RenderContext};
use vte4::TerminalExt;

use super::organism::{HUMAN_INPUT_RETREAT, SURFACE_FRAME_INTERVAL};
use crate::config::{Config, OrganismMotion};

pub(crate) const VTE_ORGANISM_CAPABILITY: &str =
    "Traditional VTE shows a neutral companion. Command and alternate-screen state are unavailable, so live greetings are unavailable. The offline settings preview remains available.";
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

/// Owned by VteTerminalView. Signal closures and the single pending source
/// hold only weak references; the native Window subscription is disconnected.
pub(crate) struct VteOrganismSurface {
    root: glib::WeakRef<gtk4::Box>,
    terminal: glib::WeakRef<vte4::Terminal>,
    row: gtk4::Box,
    glyph: gtk4::Label,
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
            enabled: Cell::new(config.ascii_organism_enabled),
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
                runtime.glyph.set_opacity(0.0);
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
                    if runtime.drawable.replace(drawable) != drawable {
                        runtime.defer_geometry_refresh();
                    }
                }
            });
            *self.paint.borrow_mut() = Some((clock.downgrade(), handler));
        }
    }

    fn drawable_geometry(&self) -> bool {
        let Some(root) = self.root.upgrade() else {
            return false;
        };
        if !self.mapped.get() || !self.row.is_mapped() || !self.glyph.is_mapped() {
            return false;
        }
        let Some(mut area) = self.glyph.compute_bounds(&root) else {
            return false;
        };
        if !finite_positive_rect(&area) {
            return false;
        }
        let mut ancestor = self.glyph.parent();
        while let Some(widget) = ancestor {
            if widget.width() <= 0 || widget.height() <= 0 {
                return false;
            }
            let Some(clip) = widget.compute_bounds(&root) else {
                return false;
            };
            if !finite_positive_rect(&clip) {
                return false;
            }
            let Some(intersection) = area.intersection(&clip) else {
                return false;
            };
            area = intersection;
            ancestor = widget.parent();
        }
        finite_positive_rect(&area)
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
        self.local.set(local);
        self.refresh();
    }

    pub(crate) fn apply_settings(self: &Rc<Self>, enabled: bool, motion: OrganismMotion) {
        self.enabled.set(enabled);
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
        self.glyph.set_opacity(0.0);
    }

    fn refresh(self: &Rc<Self>) {
        let refresh_ticket = self.invalidate_refresh();
        self.cancel_source();
        self.cancel_geometry_idle();
        let host = if self.enabled.get() && self.local.get() && self.alive.get() {
            self.bind_host()
        } else {
            self.disconnect_host();
            None
        };
        self.row.set_visible(self.enabled.get() && self.local.get());
        if !self.current_refresh(&refresh_ticket) {
            return;
        }
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
        self.glyph
            .set_opacity(if eligible && !retreat { 1.0 } else { 0.0 });
        if !self.current_refresh(&refresh_ticket) {
            return;
        }
        if eligible && !retreat {
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
        if let Some(delay) = neutral_wake(eligible, self.motion.get(), now, self.last_input.get()) {
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
    window.set_child(Some(&root));
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
                        runtime.enabled.get(), runtime.local.get(), runtime.alive.get(),
                        runtime.mapped.get(), root.is_mapped(), terminal.is_mapped(),
                        terminal.has_focus(), runtime.row.is_mapped(), runtime.glyph.is_mapped(),
                        runtime.drawable_geometry(), runtime.source.borrow().is_some(),
                        runtime.paint.borrow().is_some(), runtime.geometry_idle.borrow().is_some(),
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
    window.set_child(gtk4::Widget::NONE);
    assert!(runtime.source.borrow().is_none());
    assert!(runtime.host.borrow().is_none());
    window.set_child(Some(&root));
    wait("root reattached", &|| root.is_mapped());
    terminal.grab_focus();
    wait("source after reattach", &|| {
        terminal.has_focus() && runtime.source.borrow().is_some()
    });
    // A nonzero Paned slot can clip a minimum-sized child without unmapping
    // it. Passive paint observation must stop and recover the animation.
    window.set_child(gtk4::Widget::NONE);
    let paned = gtk4::Paned::new(gtk4::Orientation::Vertical);
    paned.set_shrink_start_child(true);
    paned.set_shrink_end_child(true);
    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    root.set_size_request(-1, 240);
    paned.set_start_child(Some(&spacer));
    paned.set_end_child(Some(&root));
    window.set_child(Some(&paned));
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
    paned.set_end_child(gtk4::Widget::NONE);
    root.set_size_request(-1, -1);
    window.set_child(Some(&root));
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
    window.set_child(gtk4::Widget::NONE);
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
    window.present();
    wait("first host active again", &|| {
        window.is_active() && !second.is_active()
    });
    assert!(runtime.source.borrow().is_none());
    gtk4::prelude::RootExt::set_focus(&second, gtk4::Widget::NONE);
    assert!(gtk4::prelude::RootExt::focus(&second).is_none());
    second.set_child(gtk4::Widget::NONE);
    window.set_child(Some(&root));
    wait("pane mapped back in first host", &|| {
        root.is_mapped()
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
    let weak = Rc::downgrade(&runtime);
    drop(runtime);
    assert!(
        weak.upgrade().is_none(),
        "signals and source do not own the adapter"
    );
    assert!(pending_frame.is_destroyed());
    assert!(pending_geometry.is_destroyed());
    assert!(terminal.pty().is_none());
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .split_once("    /// Revoke synchronously")
            .unwrap()
            .0;
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
        assert!(production.contains("self.row.set_visible(self.enabled.get() && self.local.get())"));
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
        assert!(production.contains("self.row.set_visible(self.enabled.get() && self.local.get())"));
        assert!(production.contains("self.glyph.set_opacity"));
        assert!(production.contains("terminal.connect_commit"));
        assert!(production.contains("host.is_active()"));
        assert!(production.contains("terminal.has_focus()"));
    }
}
