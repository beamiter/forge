//! Chat is presentation of the existing terminal, never another shell editor.
//!
//! The history anchor stays in the document. Only the live clip moves, so every
//! history insertion, selection, copy, search and restore keeps its original
//! identity. GTK allocation and clipping own both paint and pointer geometry.
use super::{ActiveBlock, BlockState};
use gtk4::prelude::*;
use gtk4::{glib, Orientation};
use jterm_core::chat_mode::{resolve_surface, ChatSessionState, ChatSurface};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use vte4::Terminal;

pub(super) const COMMAND_GAP_PX: i32 = 8;

/// Reserve a compact home for the companion, growing for everything the real
/// shell draws, including wrapped commands and completion menus.
pub(super) fn composer_rows(input: i64, drawn: i64, viewport: i64) -> i64 {
    input.max(drawn).clamp(4.min(viewport), viewport.max(1))
}

pub(super) fn surface(state: BlockState, integrated: bool) -> ChatSurface {
    resolve_surface(ChatSessionState {
        shell_integration: integrated && state != BlockState::RawFallback,
        prompt_ready: state == BlockState::AwaitingCommand,
        command_running: state == BlockState::CollectingOutput,
        alternate_screen: state == BlockState::AltScreen,
    })
}

/// A shell redraws its confirmed prompt after SIGWINCH, emitting a fresh
/// PromptStart/PromptEnd pair. Keep that existing presentation throughout the
/// pair: undocking it would change the row budget, send another SIGWINCH, and
/// restart the redraw. This does not grant input authority; `surface` remains
/// the source for submission guards.
fn presentation_surface(previous: ChatSurface, state: BlockState, integrated: bool) -> ChatSurface {
    if previous == ChatSurface::Prompt && integrated && state == BlockState::CollectingPrompt {
        ChatSurface::Prompt
    } else {
        surface(state, integrated)
    }
}

/// Extend prompt-redraw continuity across a completed command as well.
fn completion_surface(previous: ChatSurface, state: BlockState, integrated: bool) -> ChatSurface {
    // Prompt is sufficient even when a fast C/D pair never paints Running.
    // Fullscreen, fallback, Idle and lost integration retire the hold through
    // the existing real-state/prompt-redraw resolver below.
    if integrated && matches!(previous, ChatSurface::Prompt | ChatSurface::Running) {
        match state {
            BlockState::PostCommand => return ChatSurface::Running,
            BlockState::CollectingPrompt => return ChatSurface::Prompt,
            _ => {}
        }
    }
    presentation_surface(previous, state, integrated)
}

/// The command and real foreground represented at an action's origin.
#[derive(Clone, Copy)]
pub(super) struct StopTarget {
    generation: u64,
    pub(super) foreground: crate::pty::PtyForegroundToken,
}

/// GTK's five native GtkButton activation keys, shared by press and release.
fn control_activation_key(key: gtk4::gdk::Key) -> bool {
    matches!(
        key,
        gtk4::gdk::Key::space
            | gtk4::gdk::Key::KP_Space
            | gtk4::gdk::Key::Return
            | gtk4::gdk::Key::ISO_Enter
            | gtk4::gdk::Key::KP_Enter
    )
}

/// `Some(None)` is an invalidated origin, not permission to retarget. Native
/// activate can delay clicked by 250 ms; its first origin always wins until
/// clicked or GTK's matching activation retirement. Kept pure for regression
/// coverage without a display, but this is the production signal state.
struct ControlIntents<T: Copy> {
    press: Option<Option<T>>,
    native: Option<Option<T>>,
    key_down: bool,
    epoch: u64,
}

impl<T: Copy> Default for ControlIntents<T> {
    fn default() -> Self {
        Self {
            press: None,
            native: None,
            key_down: false,
            epoch: 0,
        }
    }
}

impl<T: Copy> ControlIntents<T> {
    fn press(&mut self, target: Option<T>, key: bool) {
        if key && self.key_down {
            return;
        }
        if key {
            self.key_down = true;
        }
        if self.native.is_none() {
            self.press = Some(target);
            self.epoch = self.epoch.wrapping_add(1);
        }
    }
    fn native_activate(&mut self, target: Option<T>) {
        if self.native.is_none() {
            self.native = Some(
                self.press
                    .unwrap_or(if self.key_down { None } else { target }),
            );
            self.epoch = self.epoch.wrapping_add(1);
        }
    }
    fn release_key(&mut self) {
        self.key_down = false;
    }
    fn cancel(&mut self) {
        if self.press.is_some() {
            self.press = Some(None);
        }
        if self.native.is_some() {
            self.native = Some(None);
        }
        self.epoch = self.epoch.wrapping_add(1);
    }
    fn clicked(&mut self) -> Option<T> {
        let target = self.native.take().or(self.press.take()).flatten();
        self.press = None;
        self.epoch = self.epoch.wrapping_add(1);
        // A timer can fire while a key is still held. Only key release, not
        // clicked, permits that key's next genuine press to capture a target.
        target
    }
    fn retire(&mut self, epoch: u64) {
        if self.epoch == epoch {
            self.press = None;
            self.native = None;
            self.epoch = self.epoch.wrapping_add(1);
        }
    }
}

fn retire_control_origin<T: Copy + 'static>(
    button: &gtk4::Button,
    intents: &Rc<RefCell<ControlIntents<T>>>,
) {
    let epoch = intents.borrow().epoch;
    let weak = button.downgrade();
    let intents = intents.clone();
    glib::idle_add_local_once(move || {
        // GTK removes this class BEFORE emitting clicked. Defer retirement
        // until that signal finishes; an older idle cannot erase a new origin.
        if weak
            .upgrade()
            .is_none_or(|button| !button.has_css_class("keyboard-activating"))
        {
            intents.borrow_mut().retire(epoch);
        }
    });
}

/// Bind a command control at its actual pointer/key/native-activate origin.
/// `clicked` is only completion of that frozen origin, never a fresh request.
pub(super) fn bind_control<T: Copy + 'static>(
    button: &gtk4::Button,
    origin: impl Fn() -> Option<T> + 'static,
    activate: impl Fn(Option<T>) + 'static,
) {
    let origin = Rc::new(origin);
    let intents = Rc::new(RefCell::new(ControlIntents::default()));
    let pointer = gtk4::GestureClick::new();
    pointer.set_name(Some("chat-control-press"));
    pointer.set_button(1);
    pointer.set_propagation_phase(gtk4::PropagationPhase::Capture);
    {
        let intents = intents.clone();
        let origin = origin.clone();
        pointer.connect_pressed(move |_, _, _, _| {
            intents.borrow_mut().press(origin(), false);
        });
    }
    {
        let intents = intents.clone();
        let button = button.downgrade();
        pointer.connect_released(move |_, _, _, _| {
            if let Some(button) = button.upgrade() {
                retire_control_origin(&button, &intents);
            }
        });
    }
    {
        let intents = intents.clone();
        let button = button.downgrade();
        pointer.connect_cancel(move |_, _| {
            intents.borrow_mut().cancel();
            if let Some(button) = button.upgrade() {
                retire_control_origin(&button, &intents);
            }
        });
    }
    button.add_controller(pointer);
    let key = gtk4::EventControllerKey::new();
    key.set_name(Some("chat-control-key"));
    key.set_propagation_phase(gtk4::PropagationPhase::Capture);
    {
        let intents = intents.clone();
        let origin = origin.clone();
        key.connect_key_pressed(move |_, key, _, _| {
            if control_activation_key(key) {
                intents.borrow_mut().press(origin(), true);
            }
            glib::Propagation::Proceed
        });
    }
    {
        let intents = intents.clone();
        let button = button.downgrade();
        key.connect_key_released(move |_, key, _, _| {
            if control_activation_key(key) {
                intents.borrow_mut().release_key();
                if let Some(button) = button.upgrade() {
                    retire_control_origin(&button, &intents);
                }
            }
        });
    }
    button.add_controller(key);
    {
        let intents = intents.clone();
        let origin = origin.clone();
        button.connect_activate(move |button| {
            // GTK's RUN_FIRST class closure has armed its animation, but
            // no main-loop/timer turn has happened yet. Never replace an
            // already pending pointer, key or accessibility origin.
            let target = (button.is_mapped() && button.is_sensitive())
                .then(|| origin())
                .flatten();
            intents.borrow_mut().native_activate(target);
            if !button.has_css_class("keyboard-activating") {
                retire_control_origin(button, &intents);
            }
        });
    }
    {
        let intents = intents.clone();
        button.connect_notify_local(Some("css-classes"), move |button, _| {
            if !button.has_css_class("keyboard-activating") {
                retire_control_origin(button, &intents);
            }
        });
    }
    {
        let intents = intents.clone();
        button.connect_sensitive_notify(move |button| {
            if !button.is_sensitive() {
                intents.borrow_mut().cancel();
                retire_control_origin(button, &intents);
            }
        });
    }
    {
        let intents = intents.clone();
        button.connect_unmap(move |button| {
            intents.borrow_mut().cancel();
            retire_control_origin(button, &intents);
        });
    }
    let focus = gtk4::EventControllerFocus::new();
    {
        let intents = intents.clone();
        let button = button.downgrade();
        focus.connect_leave(move |_| {
            // A timer activation hands focus back to the live terminal; its
            // physical key release will then belong to that VTE. A later fresh
            // focus must not inherit a permanently held key from this button.
            let mut state = intents.borrow_mut();
            state.release_key();
            state.cancel();
            drop(state);
            if let Some(button) = button.upgrade() {
                retire_control_origin(&button, &intents);
            }
        });
    }
    button.add_controller(focus);
    button.connect_clicked(move |_| {
        let target = intents.borrow_mut().clicked();
        activate(target);
    });
}

/// All widget references are weak: a VTE signal owns the layout closure that
/// consults this object, so retaining its clip here would create a widget cycle.
pub(super) struct ChatPresentation {
    dock: glib::WeakRef<gtk4::Box>,
    composer: glib::WeakRef<gtk4::Box>,
    composer_viewport: glib::WeakRef<gtk4::ScrolledWindow>,
    resize_observer: glib::WeakRef<gtk4::DrawingArea>,
    controls: glib::WeakRef<gtk4::Box>,
    status: glib::WeakRef<gtk4::Label>,
    submit: glib::WeakRef<gtk4::Button>,
    stop: glib::WeakRef<gtk4::Button>,
    current: Cell<ChatSurface>,
    reparenting: Cell<bool>,
    running_command: glib::WeakRef<gtk4::Label>,
    command_generation: Cell<u64>,
    displayed_stop_target: Cell<Option<StopTarget>>,
}

impl ChatPresentation {
    pub(super) fn new(
        root: &gtk4::Box,
        history: &gtk4::Overlay,
        active: &ActiveBlock,
        compact: bool,
    ) -> Self {
        let canvas = gtk4::Box::new(Orientation::Vertical, 0);
        canvas.add_css_class("chat-canvas");
        canvas.set_hexpand(true);
        canvas.set_vexpand(true);
        canvas.append(history);
        let dock = gtk4::Box::new(Orientation::Vertical, 0);
        dock.add_css_class("chat-dock");
        dock.set_hexpand(true);
        dock.set_vexpand(false);
        let composer = gtk4::Box::new(Orientation::Vertical, 0);
        composer.add_css_class("block-active");
        composer.add_css_class("chat-composer");
        if compact {
            composer.add_css_class("block-compact");
        }
        composer.set_hexpand(true);
        composer.set_focusable(false);
        // Natural height follows multiline input; minimum height does not.
        // An old tall editor must not prevent the window from getting smaller
        // before the canvas-driven PTY/layout pass can observe that resize.
        let composer_viewport = gtk4::ScrolledWindow::new();
        composer_viewport.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::External);
        composer_viewport.set_propagate_natural_height(true);
        composer_viewport.set_min_content_height(0);
        composer_viewport.set_hexpand(true);
        composer_viewport.set_vexpand(false);
        composer_viewport.set_focusable(false);
        composer_viewport.set_overflow(gtk4::Overflow::Hidden);
        composer_viewport.add_css_class("chat-composer-viewport");
        composer_viewport.set_child(Some(&composer));
        dock.append(&composer_viewport);
        let controls = gtk4::Box::new(Orientation::Horizontal, 8);
        controls.add_css_class("chat-controls");
        let shell_icon = gtk4::Image::from_icon_name("utilities-terminal-symbolic");
        shell_icon.add_css_class("chat-shell-icon");
        shell_icon.set_pixel_size(14);
        controls.append(&shell_icon);
        let status = gtk4::Label::new(Some("Shell · Tab completes · ↑ history"));
        status.add_css_class("chat-input-hint");
        status.set_hexpand(true);
        status.set_xalign(0.0);
        status.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        status.set_width_chars(1);
        status.set_tooltip_text(Some(status.text().as_str()));
        status.set_accessible_role(gtk4::AccessibleRole::Status);
        controls.append(&status);
        let stop = gtk4::Button::from_icon_name("process-stop-symbolic");
        stop.add_css_class("flat");
        stop.set_tooltip_text(Some(
            "Interrupt immediately when idle; if unavailable, use Ctrl+C on the keyboard",
        ));
        stop.update_property(&[gtk4::accessible::Property::Label(
            "Interrupt the running command",
        )]);
        stop.set_focus_on_click(false);
        stop.set_sensitive(false);
        stop.add_css_class("chat-stop");
        stop.add_css_class("chat-command-control");
        controls.append(&stop);
        let submit = gtk4::Button::from_icon_name("go-up-symbolic");
        submit.add_css_class("chat-submit");
        submit.add_css_class("chat-command-control");
        submit.set_tooltip_text(Some("Send Enter immediately when idle. Finish or cancel text composition before using Send; if unavailable or using enhanced keys, use the keyboard"));
        submit.update_property(&[gtk4::accessible::Property::Label("Submit shell input")]);
        submit.set_focus_on_click(false);
        controls.append(&submit);
        dock.append(&controls);
        canvas.append(&dock);
        // Observe the stable Chat canvas, not only history adjustments: the
        // independently docked live editor can be allocated after history has
        // already settled. This overlay has no size or input contribution.
        let frame = gtk4::Overlay::new();
        frame.set_child(Some(&canvas));
        frame.set_hexpand(true);
        frame.set_vexpand(true);
        let observer = gtk4::DrawingArea::new();
        observer.set_halign(gtk4::Align::Fill);
        observer.set_valign(gtk4::Align::Fill);
        observer.set_can_target(false);
        observer.set_focusable(false);
        frame.add_overlay(&observer);
        frame.set_measure_overlay(&observer, false);
        root.append(&frame);
        dock.set_visible(false);
        let running_command = gtk4::Label::new(None);
        running_command.add_css_class("chat-command");
        running_command.set_selectable(true);
        running_command.set_halign(gtk4::Align::End);
        running_command.set_wrap(true);
        running_command.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        running_command.set_max_width_chars(72);
        running_command.set_lines(4);
        running_command.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        running_command.set_margin_bottom(COMMAND_GAP_PX);
        running_command.set_visible(false);
        active.widget().prepend(&running_command);
        Self {
            running_command: running_command.downgrade(),
            dock: dock.downgrade(),
            composer: composer.downgrade(),
            composer_viewport: composer_viewport.downgrade(),
            resize_observer: observer.downgrade(),
            controls: controls.downgrade(),
            status: status.downgrade(),
            submit: submit.downgrade(),
            stop: stop.downgrade(),
            current: Cell::new(ChatSurface::Passthrough),
            reparenting: Cell::new(false),
            command_generation: Cell::new(0),
            displayed_stop_target: Cell::new(None),
        }
    }

    /// Re-arm the existing bounded settle; never install a permanent tick or
    /// retain the canvas through its own resize callback.
    pub(super) fn connect_resize(&self, arm: Rc<dyn Fn()>) {
        if let Some(observer) = self.resize_observer.upgrade() {
            observer.connect_resize(move |_, _, _| arm());
        }
    }

    pub(super) fn set_running_command(&self, command: &str) {
        // Called only by the accepted command-start fanout. Identical command
        // strings still receive different generations, including in one batch.
        self.command_generation
            .set(self.command_generation.get().wrapping_add(1));
        self.displayed_stop_target.set(None);
        if let Some(stop) = self.stop.upgrade() {
            stop.set_sensitive(false);
        }
        if let Some(label) = self.running_command.upgrade() {
            let display = jterm_core::review_input::safe_multiline_display(command, 4096);
            label.set_text(&display);
            label.set_tooltip_text(Some(&display));
        }
    }

    pub(super) fn stop_target_is_current(&self, target: Option<StopTarget>) -> bool {
        target.is_some_and(|target| target.generation == self.command_generation.get())
            && self.current.get() == ChatSurface::Running
    }

    pub(super) fn bind_stop(
        self: &Rc<Self>,
        button: &gtk4::Button,
        activate: impl Fn(Option<StopTarget>) + 'static,
    ) {
        let chat = self.clone();
        bind_control(button, move || chat.displayed_stop_target.get(), activate);
    }

    pub(super) fn submit_button(&self) -> Option<gtk4::Button> {
        self.submit.upgrade()
    }
    pub(super) fn stop_button(&self) -> Option<gtk4::Button> {
        self.stop.upgrade()
    }

    pub(super) fn set_compact(&self, compact: bool) {
        if let Some(dock) = self.dock.upgrade() {
            if compact {
                dock.add_css_class("chat-dock-compact");
            } else {
                dock.remove_css_class("chat-dock-compact");
            }
        }
        if let Some(composer) = self.composer.upgrade() {
            if compact {
                composer.add_css_class("block-compact");
            } else {
                composer.remove_css_class("block-compact");
            }
        }
    }

    pub(super) fn presentation_surface(&self, state: BlockState, integrated: bool) -> ChatSurface {
        // Controls retain their existing real-state/generation guards. Retire
        // a displayed Stop immediately even while its Running layout continues.
        if state != BlockState::CollectingOutput {
            self.displayed_stop_target.set(None);
            if let Some(stop) = self.stop.upgrade() {
                stop.set_sensitive(false);
            }
        }
        if !surface(state, integrated).allows_submit_affordance() {
            if let Some(submit) = self.submit.upgrade() {
                submit.set_sensitive(false);
            }
        }
        completion_surface(self.current.get(), state, integrated)
    }

    pub(super) fn reset_presentation(&self) {
        self.current.set(ChatSurface::Passthrough);
        self.displayed_stop_target.set(None);
    }

    /// Move the one live VTE without moving the stable history insertion anchor.
    /// Reparenting must neither copy terminal text nor feed/reset the terminal.
    pub(super) fn sync(&self, active: &ActiveBlock, surface: ChatSurface) -> gtk4::Box {
        let anchor = active.widget();
        let (Some(composer), Some(dock), Some(controls)) = (
            self.composer.upgrade(),
            self.dock.upgrade(),
            self.controls.upgrade(),
        ) else {
            return anchor.clone();
        };
        let clip = active.live_clip();
        let docked = surface.composer_docked();
        if docked {
            dock.add_css_class("chat-composer-card");
        } else {
            dock.remove_css_class("chat-composer-card");
        }
        let destination = if docked { &composer } else { anchor };
        let already_mounted = clip.parent().as_ref() == Some(destination.upcast_ref());
        let restore_focus = !already_mounted && active.active_vte.has_focus();
        if !already_mounted {
            if self.reparenting.replace(true) {
                return destination.clone();
            }
            if let Some(parent) = clip.parent().and_then(|p| p.downcast::<gtk4::Box>().ok()) {
                parent.remove(&clip);
            }
            destination.append(&clip);
            self.reparenting.set(false);
        }
        if docked {
            anchor.add_css_class("chat-live-anchor");
            anchor.set_height_request(0);
        } else {
            anchor.remove_css_class("chat-live-anchor");
        }
        composer.set_visible(docked);
        if let Some(viewport) = self.composer_viewport.upgrade() {
            viewport.set_visible(docked);
        }
        if surface != ChatSurface::Running {
            self.displayed_stop_target.set(None);
        }
        if let Some(command) = self.running_command.upgrade() {
            command.set_visible(surface == ChatSurface::Running);
            if surface == ChatSurface::Prompt {
                command.set_text("");
                command.set_tooltip_text(None);
            }
        }
        let decorated = matches!(surface, ChatSurface::Prompt | ChatSurface::Running);
        dock.set_visible(decorated);
        controls.set_visible(decorated);
        if let Some(submit) = self.submit.upgrade() {
            submit.set_visible(docked);
        }
        if let Some(stop) = self.stop.upgrade() {
            stop.set_visible(surface == ChatSurface::Running);
        }
        if self.current.replace(surface) != surface {
            if let Some(status) = self.status.upgrade() {
                status.set_text(if docked {
                    "Shell · Tab completes · ↑ history"
                } else {
                    "Running · input belongs to the process"
                });
                // Ellipsis keeps the controls compact, not the meaning hidden.
                status.set_tooltip_text(Some(status.text().as_str()));
            }
        }
        // Restore only after the destination is visible/mapped. Moving a
        // background pane must not turn mapping into permission to focus it.
        if restore_focus {
            active.active_vte.grab_focus();
        }
        destination.clone()
    }

    pub(super) fn update_status(
        &self,
        seconds: Option<u64>,
        can_submit: bool,
        foreground: Option<crate::pty::PtyForegroundToken>,
    ) {
        if let Some(submit) = self.submit.upgrade() {
            submit.set_sensitive(can_submit);
        }
        let running = self.current.get() == ChatSurface::Running && seconds.is_some();
        self.displayed_stop_target
            .set(
                running
                    .then_some(foreground)
                    .flatten()
                    .map(|foreground| StopTarget {
                        generation: self.command_generation.get(),
                        foreground,
                    }),
            );
        if let Some(stop) = self.stop.upgrade() {
            stop.set_sensitive(running && foreground.is_some());
        }
        if running {
            if let (Some(status), Some(seconds)) = (self.status.upgrade(), seconds) {
                status.set_text(&format!(
                    "Running · {seconds}s · input belongs to the process"
                ));
            }
        }
    }
}

/// The docked live anchor is a zero-height end-of-transcript marker. Its
/// bottom-edge point counts as visible; Block's nonempty-card inset does not.
pub(super) fn docked_anchor_visible(anchor_y: f64, viewport_height: f64) -> bool {
    anchor_y <= viewport_height + 1.0
}

/// Only short command bubbles shrink. Output always keeps the full log width.
/// `set_size` sets VTE's natural size; GTK still constrains it in narrow splits.
pub(super) fn command_columns(command: &str, recorded: i64) -> i64 {
    use unicode_width::UnicodeWidthStr;
    command
        .lines()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(1)
        .saturating_add(1)
        .clamp(4, recorded.max(4) as usize) as i64
}

/// Find Chat's real canvas through the current native hierarchy. The live VTE
/// may be docked or in the transcript; both converge on this same canvas.
pub(super) fn viewport_height_for(vte: &Terminal) -> Option<i32> {
    let mut parent = vte.parent();
    while let Some(widget) = parent {
        if widget.has_css_class("chat-canvas") {
            let mut height = widget.height();
            let dock = widget.last_child()?;
            if dock.is_visible() {
                // Reserve fixed natural chrome, never the current history page
                // size: a tall dock must not feed its own allocation back into
                // the PTY grid. GTK still owes the history scroller its minimum
                // (e.g. scrollbar minimum length), even with a min-zero editor.
                // Use the same budget while Running so docking alone cannot
                // resize readline or a completion menu.
                if let Some(controls) = dock.last_child() {
                    height -= controls.measure(Orientation::Vertical, widget.width()).1;
                }
                if let Some(history) = widget.first_child() {
                    height -= history.measure(Orientation::Vertical, widget.width()).0;
                }
            }
            return (height > 1).then_some(height);
        }
        parent = widget.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_presentation_survives_normal_and_coalesced_commands() {
        use BlockState::*;
        use ChatSurface::*;
        for states in [
            vec![
                AwaitingCommand,
                CollectingOutput,
                PostCommand,
                CollectingPrompt,
                AwaitingCommand,
            ],
            // A fast command can start and end between two layout callbacks.
            vec![
                AwaitingCommand,
                PostCommand,
                CollectingPrompt,
                AwaitingCommand,
            ],
            vec![AwaitingCommand, CollectingPrompt, AwaitingCommand],
        ] {
            let mut previous = Passthrough;
            for state in states {
                previous = completion_surface(previous, state, true);
                assert_eq!(
                    previous,
                    match state {
                        CollectingOutput | PostCommand => Running,
                        _ => Prompt,
                    },
                    "{state:?} must not hand the viewport to native VTE"
                );
                if matches!(state, PostCommand | CollectingPrompt) {
                    assert!(!surface(state, true).allows_submit_affordance());
                }
            }
        }
    }

    #[test]
    fn completion_presentation_starts_native_and_retires_on_takeover() {
        use BlockState::*;
        use ChatSurface::*;
        let mut previous = Passthrough;
        for state in [Idle, CollectingPrompt, CollectingPrompt, PostCommand] {
            previous = completion_surface(previous, state, true);
            assert_eq!(
                previous, Passthrough,
                "no confirmed Chat interaction: {state:?}"
            );
        }
        for held in [Prompt, Running] {
            for state in [Idle, RawFallback, AltScreen] {
                previous = held;
                previous = completion_surface(previous, state, true);
                assert_eq!(
                    previous,
                    if state == AltScreen {
                        Fullscreen
                    } else {
                        Passthrough
                    }
                );
                let state = CollectingPrompt;
                assert_eq!(
                    completion_surface(previous, state, true),
                    Passthrough,
                    "native takeover retires the presentation hold"
                );
            }
        }
    }
    #[test]
    fn completion_presentation_lost_integration_clears_hold() {
        for previous in [ChatSurface::Prompt, ChatSurface::Running] {
            for state in [BlockState::PostCommand, BlockState::CollectingPrompt] {
                assert_eq!(
                    completion_surface(previous, state, false),
                    ChatSurface::Passthrough
                );
                assert!(!surface(state, true).allows_submit_affordance());
            }
        }
    }

    #[test]
    fn chat_stop_captures_every_native_button_activation_key() {
        for key in [
            gtk4::gdk::Key::space,
            gtk4::gdk::Key::KP_Space,
            gtk4::gdk::Key::Return,
            gtk4::gdk::Key::ISO_Enter,
            gtk4::gdk::Key::KP_Enter,
        ] {
            assert!(control_activation_key(key));
        }
        assert!(!control_activation_key(gtk4::gdk::Key::Escape));
    }

    #[test]
    fn chat_stop_native_delay_and_repeat_cannot_rebind_an_origin() {
        let mut state = ControlIntents::<u64>::default();
        assert_eq!(
            state.clicked(),
            None,
            "unoriginated clicked cannot retarget"
        );
        state.native_activate(Some(1));
        state.native_activate(Some(2));
        state.press(Some(3), false);
        assert_eq!(state.clicked(), Some(1));
        assert_eq!(state.clicked(), None);
        state.press(Some(4), true);
        state.native_activate(Some(5));
        state.press(Some(6), true);
        state.press(Some(66), false); // pointer cannot release a held key
        assert_eq!(state.clicked(), Some(4));
        // A timer click while held must not make autorepeat a fresh press.
        state.press(Some(7), true);
        state.native_activate(Some(8));
        assert_eq!(state.clicked(), None);
        state.release_key();
        state.press(Some(9), true);
        state.native_activate(Some(10));
        state.release_key();
        assert_eq!(state.clicked(), Some(9));
    }

    #[test]
    fn chat_stop_cancelled_or_expired_origin_is_never_replaced_by_late_activation() {
        let mut state = ControlIntents::<u64>::default();
        state.press(Some(1), false);
        state.native_activate(Some(2));
        state.cancel();
        state.native_activate(Some(3));
        assert_eq!(state.clicked(), None);
        state.native_activate(None);
        state.native_activate(Some(4));
        assert_eq!(state.clicked(), None);
        state.native_activate(Some(5));
        let old_idle = state.epoch;
        assert_eq!(state.clicked(), Some(5));
        state.native_activate(Some(6));
        state.retire(old_idle);
        assert_eq!(
            state.clicked(),
            Some(6),
            "old cleanup must not erase new origin"
        );
        state.press(Some(7), false);
        state.cancel();
        state.retire(state.epoch);
        state.native_activate(Some(8));
        assert_eq!(
            state.clicked(),
            Some(8),
            "new action works after completed cancellation"
        );
        state.press(Some(9), false);
        state.retire(state.epoch);
        assert_eq!(
            state.clicked(),
            None,
            "released, abandoned press has expired"
        );
    }

    #[test]
    fn confirmed_prompt_redraw_keeps_presentation_but_not_submission_authority() {
        let mut previous = ChatSurface::Prompt;
        for _ in 0..8 {
            previous = presentation_surface(previous, BlockState::CollectingPrompt, true);
            assert_eq!(previous, ChatSurface::Prompt);
            assert!(!surface(BlockState::CollectingPrompt, true).allows_submit_affordance());
            previous = presentation_surface(previous, BlockState::AwaitingCommand, true);
            assert_eq!(previous, ChatSurface::Prompt);
        }
    }

    #[test]
    fn prompt_presentation_hold_requires_an_uninterrupted_confirmed_prompt() {
        assert_eq!(
            presentation_surface(ChatSurface::Passthrough, BlockState::CollectingPrompt, true),
            ChatSurface::Passthrough,
            "the first prompt has not been confirmed"
        );
        for (state, integrated, expected) in [
            (BlockState::CollectingOutput, true, ChatSurface::Running),
            (BlockState::AltScreen, true, ChatSurface::Fullscreen),
            (BlockState::RawFallback, true, ChatSurface::Passthrough),
            (BlockState::PostCommand, true, ChatSurface::Passthrough),
            (BlockState::Idle, true, ChatSurface::Passthrough),
            (
                BlockState::CollectingPrompt,
                false,
                ChatSurface::Passthrough,
            ),
            (BlockState::AwaitingCommand, false, ChatSurface::Passthrough),
        ] {
            let next = presentation_surface(ChatSurface::Prompt, state, integrated);
            assert_eq!(next, expected);
            assert_eq!(
                presentation_surface(next, BlockState::CollectingPrompt, true),
                ChatSurface::Passthrough,
                "execution, fullscreen, fallback or lost integration must clear the hold"
            );
        }
    }

    #[test]
    fn only_a_real_integrated_prompt_docks() {
        assert_eq!(
            surface(BlockState::AwaitingCommand, true),
            ChatSurface::Prompt
        );
        for state in [
            BlockState::Idle,
            BlockState::CollectingPrompt,
            BlockState::RawFallback,
            BlockState::PostCommand,
        ] {
            assert_eq!(surface(state, true), ChatSurface::Passthrough);
        }
        assert_eq!(
            surface(BlockState::AwaitingCommand, false),
            ChatSurface::Passthrough
        );
        assert_eq!(
            surface(BlockState::CollectingOutput, true),
            ChatSurface::Running
        );
        assert_eq!(
            surface(BlockState::AltScreen, false),
            ChatSurface::Fullscreen
        );
        assert_eq!(
            surface(BlockState::AwaitingCommand, true),
            ChatSurface::Prompt
        );
    }
    #[test]
    fn docked_transcript_end_is_visible_at_the_history_bottom() {
        assert!(docked_anchor_visible(480.0, 480.0));
        assert!(docked_anchor_visible(480.5, 480.0));
        assert!(docked_anchor_visible(120.0, 480.0));
        assert!(!docked_anchor_visible(500.0, 480.0));
    }

    #[test]
    fn command_capsules_measure_unicode_but_do_not_narrow_long_logs() {
        assert_eq!(command_columns("pwd", 100), 4);
        assert_eq!(command_columns("界界", 100), 5);
        assert_eq!(command_columns("e\u{301}", 100), 4);
        assert_eq!(command_columns("one\na longer line", 100), 14);
        assert_eq!(command_columns(&"x".repeat(1000), 80), 80);
        assert_eq!(command_columns("anything", 2), 4);
    }
    #[test]
    #[ignore = "requires DISPLAY; run explicitly under Xvfb"]
    fn chat_moves_only_the_native_clip_and_keeps_history_anchor() {
        use super::super::bounded_bytes::BoundedByteRing;
        use std::cell::RefCell;
        use std::rc::Rc;
        use vte4::TerminalExt;
        gtk4::init().expect("gtk init");
        let config = crate::config::Config::safe_defaults();
        let live = ActiveBlock::new(&config, Rc::new(RefCell::new(BoundedByteRing::new(1024))));
        let terminal = live.active_vte.clone();
        terminal.add_css_class("chat-live-terminal");
        super::super::install_block_css(&config);
        let root = gtk4::Box::new(Orientation::Vertical, 0);
        root.add_css_class("term-chat-root");
        let list = gtk4::Box::new(Orientation::Vertical, 0);
        list.append(live.widget());
        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_child(Some(&list));
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&scroll));
        let chat = ChatPresentation::new(&root, &overlay, &live, false);
        let anchor: gtk4::Widget = live.widget().clone().upcast();
        let original_parent = anchor.parent();
        let block = ActiveBlock::new(&config, Rc::new(RefCell::new(BoundedByteRing::new(1024))));
        let panes = gtk4::Box::new(Orientation::Horizontal, 0);
        panes.append(&root);
        panes.append(block.widget());
        let window = gtk4::Window::builder()
            .default_width(900)
            .default_height(500)
            .child(&panes)
            .build();
        window.present();
        let settle = || {
            for _ in 0..20 {
                while gtk4::glib::MainContext::default().iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        settle();
        chat.set_running_command("printf '<long & literal command>'\nnext line");
        let command = chat.running_command.upgrade().unwrap();
        assert_eq!(
            command.tooltip_text().as_deref(),
            Some(command.text().as_str())
        );
        live.set_live_geometry(terminal.char_height() as i32, 24, 3);
        block.set_live_geometry(block.active_vte.char_height() as i32, 24, 3);
        settle();
        assert!(block.active_vte.grab_focus());
        for surface in [
            ChatSurface::Prompt,
            ChatSurface::Running,
            ChatSurface::Fullscreen,
            ChatSurface::Running,
            ChatSurface::Prompt,
            ChatSurface::Passthrough,
            ChatSurface::Prompt,
        ] {
            let target = chat.sync(&live, surface);
            let status = chat.status.upgrade().unwrap();
            assert_eq!(
                status.tooltip_text().as_deref(),
                Some(status.text().as_str())
            );
            assert_eq!(status.width_chars(), 1);
            if surface == ChatSurface::Prompt {
                assert_eq!(command.tooltip_text(), None);
            }
            settle();
            assert!(
                block.active_vte.has_focus(),
                "mapped background Chat must not steal focus"
            );
            assert!(terminal.is_mapped());
            assert_eq!(anchor.parent(), original_parent);
            assert_eq!(live.active_vte, terminal);
            assert_eq!(
                live.live_clip().parent().as_ref(),
                Some(target.upcast_ref())
            );
            assert_eq!(
                target.has_css_class("chat-composer"),
                surface.composer_docked()
            );
            assert_eq!(
                chat.submit_button().unwrap().is_visible(),
                surface.composer_docked()
            );
            assert_eq!(
                chat.stop_button().unwrap().is_visible(),
                surface == ChatSurface::Running
            );
        }
        // The simultaneously mapped sibling keeps ordinary Block style.
        assert!(!block.widget().has_css_class("chat-live-anchor"));
        assert_eq!(
            block.live_clip().parent().as_ref(),
            Some(block.widget().upcast_ref())
        );
        window.close();
    }
}
