//! Native Block-pane body for the experimental ASCII organism.

use gtk4::prelude::*;
use gtk4::{Box as GBox, Label, Orientation};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::UiState;
use crate::block_view::TermView;
use crate::organism::{
    classify_command, sticky_glyph, Behavior, CommandKind, LifeState, NativeOrganism, Reaction,
    RepoArrival, Tone,
};
use crate::organism_memory::{local_day_at_ms, unix_ms, MemoryEvent, RepoContext};

const REACTION_HOLD: Duration = Duration::from_millis(8_000);
/// An accepted correction only vouches for a command that starts promptly.
const CORRECTION_ASSIST_WINDOW: Duration = Duration::from_secs(30);
const HUMAN_INPUT_RETREAT: Duration = Duration::from_millis(900);
const SURFACE_FRAME_INTERVAL: Duration = Duration::from_millis(100);
const SURFACE_MARGIN: i32 = 8;
const TONE_CLASSES: [&str; 5] = [
    "organism-quiet",
    "organism-active",
    "organism-success",
    "organism-error",
    "organism-warning",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceMode {
    Idle,
    Typing,
    Watching,
    Reacting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SurfaceBox {
    width: i32,
    height: i32,
    right_gutter: i32,
    cell_width: i32,
    cell_height: i32,
    body_width: i32,
    body_height: i32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SurfacePoint {
    x: f64,
    y: f64,
}

fn surface_mode(
    behavior: Behavior,
    command_running: bool,
    human_input_age: Option<Duration>,
) -> SurfaceMode {
    if human_input_age.is_some_and(|age| age < HUMAN_INPUT_RETREAT) {
        SurfaceMode::Typing
    } else if command_running {
        SurfaceMode::Watching
    } else if behavior == Behavior::Idle {
        SurfaceMode::Idle
    } else {
        SurfaceMode::Reacting
    }
}

/// Pick a point inside the live VTE without changing its allocation. The
/// right gutter is owned by the live scrollbar, so every pose stays left of
/// it. A surface that cannot fit the complete sprite fails closed.
fn surface_point(surface: SurfaceBox, mode: SurfaceMode, frame: u64) -> Option<SurfacePoint> {
    let cell_width = surface.cell_width.max(1);
    let cell_height = surface.cell_height.max(1);
    let margin_x = SURFACE_MARGIN.max(cell_width);
    let margin_y = SURFACE_MARGIN.max(cell_height);
    let min_width = surface
        .body_width
        .saturating_add(surface.right_gutter)
        .saturating_add(margin_x.saturating_mul(2));
    let min_height = surface
        .body_height
        .saturating_add(margin_y.saturating_mul(2));
    if surface.width < min_width
        || surface.height < min_height
        || surface.body_width <= 0
        || surface.body_height <= 0
    {
        return None;
    }

    let min_x = align_up(margin_x, cell_width);
    let max_x = align_down(
        surface
            .width
            .saturating_sub(surface.right_gutter)
            .saturating_sub(surface.body_width)
            .saturating_sub(margin_x),
        cell_width,
    );
    let min_y = align_up(margin_y, cell_height);
    let max_y = align_down(
        surface
            .height
            .saturating_sub(surface.body_height)
            .saturating_sub(margin_y),
        cell_height,
    );
    if max_x < min_x || max_y < min_y {
        return None;
    }

    let (x, y) = match mode {
        SurfaceMode::Idle => {
            // Spend 90% of the 80-second cycle quietly sitting at an edge and
            // only 10% walking between them. It feels alive without turning
            // ordinary terminal work into a perpetual desktop-pet animation.
            let span = max_x.saturating_sub(min_x);
            let phase = (frame % 800) as i32;
            let step = match phase {
                0..=359 => 0,
                360..=399 => phase - 360,
                400..=759 => 40,
                _ => 800 - phase,
            };
            let x = align_down(
                min_x.saturating_add(span.saturating_mul(step) / 40),
                cell_width,
            );
            (x, max_y)
        }
        // Accepted human input moves it away from the prompt edge. The window
        // slides on every accepted write, so sustained typing keeps it there.
        SurfaceMode::Typing => (max_x, min_y),
        SurfaceMode::Watching => {
            let bob = if (frame / 3).is_multiple_of(2) {
                0
            } else {
                cell_height
            };
            (
                max_x,
                align_down(min_y.saturating_add(bob).min(max_y), cell_height),
            )
        }
        SurfaceMode::Reacting => (
            align_down(
                min_x.saturating_add(max_x.saturating_sub(min_x) / 2),
                cell_width,
            ),
            align_down(
                min_y.saturating_add(max_y.saturating_sub(min_y) / 2),
                cell_height,
            ),
        ),
    };

    Some(SurfacePoint {
        x: f64::from(x.clamp(min_x, max_x)),
        y: f64::from(y.clamp(min_y, max_y)),
    })
}

fn align_down(value: i32, cell: i32) -> i32 {
    value.div_euclid(cell.max(1)).saturating_mul(cell.max(1))
}

fn align_up(value: i32, cell: i32) -> i32 {
    let cell = cell.max(1);
    value
        .saturating_add(cell.saturating_sub(1))
        .div_euclid(cell)
        .saturating_mul(cell)
}

/// Opaque identity of the Block pane hosting a correction card. Pointer
/// identity only — it carries no path, command, or content data.
pub(crate) fn pane_token(view: &Rc<TermView>) -> usize {
    Rc::as_ptr(view) as usize
}

/// Window-shared, content-free pulses from the command-correction card. The
/// organism learns only the accept/dismiss fact — never the failed command,
/// the proposed correction, or any output text.
pub(crate) struct OrganismCorrectionSignal {
    life: Rc<Cell<LifeState>>,
    accepted: Cell<Option<(usize, Instant)>>,
    dismiss_streak: Cell<u32>,
}

impl OrganismCorrectionSignal {
    pub(crate) fn new(life: Rc<Cell<LifeState>>) -> Rc<Self> {
        Rc::new(Self {
            life,
            accepted: Cell::new(None),
            dismiss_streak: Cell::new(0),
        })
    }

    /// `pane` scopes the acceptance to the Block pane hosting the card, so a
    /// command starting in any other pane can never claim the assist.
    pub(crate) fn note_accepted(&self, pane: usize) {
        self.dismiss_streak.set(0);
        self.accepted.set(Some((pane, Instant::now())));
        self.life
            .set(crate::organism::correction_accepted(self.life.get()));
    }

    /// Drop a pending acceptance whose command demonstrably did not run.
    pub(crate) fn revoke_accept(&self, pane: usize) {
        if self.accepted.get().is_some_and(|(id, _)| id == pane) {
            self.accepted.set(None);
        }
    }

    pub(crate) fn note_dismissed(&self) {
        let streak = self.dismiss_streak.get().saturating_add(1);
        self.dismiss_streak.set(streak);
        self.life
            .set(crate::organism::correction_dismissed(self.life.get(), streak));
    }

    /// Consume a fresh acceptance for the command that is about to start in
    /// the accepting pane, so one card vouches for at most one command there.
    fn take_recent_accept(&self, pane: usize, now: Instant) -> bool {
        match self.accepted.get() {
            Some((id, at)) if id == pane => {
                self.accepted.set(None);
                now.saturating_duration_since(at) < CORRECTION_ASSIST_WINDOW
            }
            _ => false,
        }
    }
}

/// True when `cwd` is `root` itself or a path strictly inside it.
fn cwd_within(cwd: &str, root: &str) -> bool {
    cwd.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Decide whether a volatile (non-build) command is still working inside the
/// last known checkout: either its cwd sits under the canonical repo root, or
/// it equals the exact raw cwd that last resolved to that root. A different
/// subdirectory of a symlinked checkout matches neither arm and conservatively
/// falls back to a context reset.
fn same_checkout(root: Option<&str>, repo_cwd: Option<&str>, cwd: Option<&str>) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    root.is_some_and(|root| cwd_within(cwd, root))
        || repo_cwd.is_some_and(|known| known == cwd)
}

struct OrganismRuntime {
    organism: RefCell<NativeOrganism>,
    active_memory_kind: Cell<Option<CommandKind>>,
    active_context_key: RefCell<Option<String>>,
    active_repo_context: RefCell<Option<RepoContext>>,
    /// Canonical root of the last resolved repository, plus the raw cwd that
    /// resolved to it. Greeting/shyness keys on this identity, never on the
    /// mixed root/cwd context key.
    last_repo_root: RefCell<Option<String>>,
    last_repo_cwd: RefCell<Option<String>>,
    last_local_day: Cell<Option<i64>>,
    generation: Cell<u64>,
    settle_timer: RefCell<Option<gtk4::glib::SourceId>>,
    surface_tick: RefCell<Option<gtk4::TickCallbackId>>,
    surface_last_frame: Cell<Option<Instant>>,
    surface_frame: Cell<u64>,
    surface_behavior: Cell<Behavior>,
    last_surface_mode: Cell<Option<SurfaceMode>>,
    command_running: Cell<bool>,
    last_human_input: Cell<Option<Instant>>,
    output_activity: Cell<bool>,
    card: gtk4::Widget,
    sprite: Label,
    live_body: Label,
    sticky_avatar: Label,
    badge: Label,
    status: Label,
    state: Label,
}

impl OrganismRuntime {
    fn new(initial_state: LifeState, persistent: bool) -> Rc<Self> {
        let outer = GBox::new(Orientation::Vertical, 0);
        outer.add_css_class("block-finished");
        outer.add_css_class("block-organism");
        outer.add_css_class("organism-quiet");
        outer.set_hexpand(true);
        outer.set_vexpand(false);
        outer.set_margin_top(3);
        outer.set_margin_bottom(3);
        outer.set_margin_start(8);
        outer.set_margin_end(8);
        outer.set_can_target(false);
        outer.set_focusable(false);

        let content = GBox::new(Orientation::Horizontal, 12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(8);
        content.set_margin_bottom(8);

        let sprite = Label::new(None);
        sprite.add_css_class("organism-sprite");
        sprite.set_xalign(0.0);
        sprite.set_yalign(0.5);
        sprite.set_selectable(false);
        content.append(&sprite);

        let detail = GBox::new(Orientation::Vertical, 3);
        detail.set_hexpand(true);
        let header = GBox::new(Orientation::Horizontal, 8);
        let title = Label::new(Some("ASCII organism"));
        title.add_css_class("organism-title");
        title.set_xalign(0.0);
        header.append(&title);
        let badge = Label::new(Some(if persistent {
            "repo memory · no LLM"
        } else {
            "volatile · no LLM"
        }));
        badge.add_css_class("organism-badge");
        badge.set_hexpand(true);
        badge.set_halign(gtk4::Align::End);
        badge.set_max_width_chars(32);
        badge.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        header.append(&badge);
        detail.append(&header);

        let status = Label::new(None);
        status.add_css_class("organism-status");
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_lines(2);
        status.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        status.set_accessible_role(gtk4::AccessibleRole::Status);
        detail.append(&status);

        let state = Label::new(None);
        state.add_css_class("organism-state");
        state.set_xalign(0.0);
        state.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        detail.append(&state);
        content.append(&detail);
        outer.append(&content);

        let live_body = Label::new(None);
        live_body.set_widget_name("ascii-organism-live-body");
        live_body.add_css_class("organism-live-body");
        live_body.add_css_class("organism-quiet");
        live_body.set_xalign(0.0);
        live_body.set_yalign(0.0);
        live_body.set_selectable(false);
        live_body.set_can_target(false);
        live_body.set_focusable(false);
        live_body.set_accessible_role(gtk4::AccessibleRole::Presentation);

        let sticky_avatar = Label::new(Some("/\\_/\\"));
        // Micro-poses are all five ASCII characters; pinning the width keeps
        // the sticky header steady even under a proportional fallback font.
        sticky_avatar.set_width_chars(5);
        sticky_avatar.set_widget_name("ascii-organism-sticky-avatar");
        sticky_avatar.add_css_class("organism-sticky-avatar");
        sticky_avatar.add_css_class("organism-active");
        sticky_avatar.set_selectable(false);
        sticky_avatar.set_can_target(false);
        sticky_avatar.set_focusable(false);
        sticky_avatar.set_accessible_role(gtk4::AccessibleRole::Presentation);

        let runtime = Rc::new(Self {
            organism: RefCell::new(NativeOrganism::from_persisted_state(initial_state)),
            active_memory_kind: Cell::new(None),
            active_context_key: RefCell::new(None),
            active_repo_context: RefCell::new(None),
            last_repo_root: RefCell::new(None),
            last_repo_cwd: RefCell::new(None),
            last_local_day: Cell::new(None),
            generation: Cell::new(0),
            settle_timer: RefCell::new(None),
            surface_tick: RefCell::new(None),
            surface_last_frame: Cell::new(None),
            surface_frame: Cell::new(0),
            surface_behavior: Cell::new(Behavior::Idle),
            last_surface_mode: Cell::new(None),
            command_running: Cell::new(false),
            last_human_input: Cell::new(None),
            output_activity: Cell::new(false),
            card: outer.upcast(),
            sprite,
            live_body,
            sticky_avatar,
            badge,
            status,
            state,
        });
        let idle = runtime.organism.borrow().idle_reaction();
        runtime.render(&idle);
        runtime
    }

    fn bump_generation(&self) -> u64 {
        if let Some(source) = self.settle_timer.borrow_mut().take() {
            source.remove();
        }
        let next = self.generation.get().wrapping_add(1);
        self.generation.set(next);
        next
    }

    fn render(&self, reaction: &Reaction) {
        self.sprite.set_text(reaction.behavior.sprite());
        self.surface_behavior.set(reaction.behavior);
        let status = match reaction.speech {
            Some(speech) => format!("{speech}  {}", reaction.description),
            None => reaction.description.clone(),
        };
        self.status.set_text(&status);
        self.status.set_tooltip_text(Some(&status));
        self.state
            .set_text(&state_summary(self.organism.borrow().state()));

        for class in TONE_CLASSES {
            self.card.remove_css_class(class);
            self.live_body.remove_css_class(class);
            self.sticky_avatar.remove_css_class(class);
        }
        let tone_class = match reaction.tone {
            Tone::Quiet => "organism-quiet",
            Tone::Active => "organism-active",
            Tone::Success => "organism-success",
            Tone::Error => "organism-error",
            Tone::Warning => "organism-warning",
        };
        self.card.add_css_class(tone_class);
        self.live_body.add_css_class(tone_class);
        self.sticky_avatar.add_css_class(tone_class);
    }

    fn human_input_age(&self, now: Instant) -> Option<Duration> {
        self.last_human_input
            .get()
            .map(|at| now.saturating_duration_since(at))
    }

    fn refresh_surface(&self, view: &TermView, now: Instant) {
        let behavior = self.surface_behavior.get();
        let mode = surface_mode(
            behavior,
            self.command_running.get(),
            self.human_input_age(now),
        );
        if self.last_surface_mode.get() != Some(mode) {
            log::trace!("ASCII organism live-surface mode: {mode:?}");
            self.last_surface_mode.set(Some(mode));
        }
        let display_behavior = match mode {
            SurfaceMode::Idle => Behavior::Idle,
            SurfaceMode::Typing | SurfaceMode::Watching => Behavior::WatchCommand,
            SurfaceMode::Reacting => behavior,
        };
        let sprite = display_behavior.sprite();
        if self.live_body.text().as_str() != sprite {
            self.live_body.set_text(sprite);
        }
        // The sticky one-line form mirrors the same displayed behavior with a
        // fixed-width micro-pose, so scrollback readers see the mood too.
        let glyph = sticky_glyph(display_behavior, self.surface_frame.get());
        if self.sticky_avatar.text().as_str() != glyph {
            self.sticky_avatar.set_text(glyph);
        }

        let metrics = view.live_organism_surface_metrics();
        if metrics.alt_screen {
            // ActiveBlock owns the temporary alt-screen override. Preserve the
            // geometry-derived desired visibility so rmcup restores it in the
            // same parser turn instead of waiting for another animation tick.
            self.sticky_avatar.set_visible(false);
            return;
        }
        // Keep the child measurable while the non-measuring overlay is hidden.
        // GTK reports zero requisition for an explicitly invisible Label,
        // which would otherwise make a tiny initial allocation self-locking.
        self.live_body.set_visible(true);
        let (_, body_width, _, _) = self.live_body.measure(Orientation::Horizontal, -1);
        let (_, body_height, _, _) = self.live_body.measure(Orientation::Vertical, body_width);
        let point = surface_point(
            SurfaceBox {
                width: metrics.width,
                height: metrics.height,
                right_gutter: metrics.right_gutter,
                cell_width: metrics.cell_width,
                cell_height: metrics.cell_height,
                body_width,
                body_height,
            },
            mode,
            self.surface_frame.get(),
        );

        if let Some(point) = point {
            view.set_live_organism_visible(true);
            view.move_live_organism_body(self.live_body.upcast_ref(), point.x, point.y);
        } else {
            view.set_live_organism_visible(false);
        }
        // The Block layer decides whether the sticky running header is active;
        // this child only follows the same accepted-input retreat window.
        self.sticky_avatar.set_visible(mode != SurfaceMode::Typing);
    }

    fn start_surface_tick(runtime: &Rc<Self>, view: &Rc<TermView>) {
        let runtime_weak = Rc::downgrade(runtime);
        let view_weak = Rc::downgrade(view);
        let tick = view.vte().add_tick_callback(move |_, _| {
            let (Some(runtime), Some(view)) = (runtime_weak.upgrade(), view_weak.upgrade()) else {
                return gtk4::glib::ControlFlow::Break;
            };
            let now = Instant::now();
            if runtime
                .surface_last_frame
                .get()
                .is_some_and(|last| now.saturating_duration_since(last) < SURFACE_FRAME_INTERVAL)
            {
                return gtk4::glib::ControlFlow::Continue;
            }
            runtime.surface_last_frame.set(Some(now));
            let pulse = u64::from(runtime.output_activity.replace(false));
            runtime
                .surface_frame
                .set(runtime.surface_frame.get().wrapping_add(1 + pulse));
            runtime.refresh_surface(&view, now);
            gtk4::glib::ControlFlow::Continue
        });
        *runtime.surface_tick.borrow_mut() = Some(tick);
    }

    fn mark_volatile(&self) {
        self.badge.set_text("volatile · save failed");
        self.badge.set_tooltip_text(Some(
            "Repository memory could not be queued for durable storage",
        ));
    }

    fn settle_later(runtime: &Rc<Self>, view: std::rc::Weak<TermView>, generation: u64) {
        let runtime_weak = Rc::downgrade(runtime);
        let source = gtk4::glib::timeout_add_local_once(REACTION_HOLD, move || {
            let Some(runtime) = runtime_weak.upgrade() else {
                return;
            };
            runtime.settle_timer.borrow_mut().take();
            if runtime.generation.get() != generation {
                return;
            }
            let idle = runtime.organism.borrow().idle_reaction();
            runtime.render(&idle);
            if let Some(view) = view.upgrade() {
                view.insert_inline_notice(&runtime.card);
            }
        });
        *runtime.settle_timer.borrow_mut() = Some(source);
    }
}

impl Drop for OrganismRuntime {
    fn drop(&mut self) {
        if let Some(source) = self.settle_timer.get_mut().take() {
            source.remove();
        }
        if let Some(tick) = self.surface_tick.get_mut().take() {
            tick.remove();
        }
    }
}

fn state_summary(state: LifeState) -> String {
    format!(
        "E{:02} M{:02} C{:02} B{:02} S{:02} N{:02} A{:02} F{:02}",
        percent(state.energy),
        percent(state.mood),
        percent(state.curiosity),
        percent(state.boredom),
        percent(state.stress),
        percent(state.social_need),
        percent(state.attachment),
        percent(state.confidence),
    )
}

fn percent(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 100.0).round() as u8
}

impl UiState {
    pub(crate) fn attach_ascii_organism_to_view(&self, view: &Rc<TermView>, remote: bool) {
        if remote || !self.config.borrow().ascii_organism_enabled {
            return;
        }

        let initial_state = self.organism_life.get();
        let persistent = self.organism_memory.borrow().is_some();
        let runtime = OrganismRuntime::new(initial_state, persistent);
        view.insert_inline_notice(&runtime.card);
        if !view.put_live_organism_body(runtime.live_body.upcast_ref(), 0.0, 0.0) {
            log::warn!("could not attach ASCII organism to the live terminal surface");
        }
        if !view.put_sticky_organism_avatar(runtime.sticky_avatar.upcast_ref()) {
            log::warn!("could not attach ASCII organism to the sticky running header");
        }
        runtime.refresh_surface(view, Instant::now());
        OrganismRuntime::start_surface_tick(&runtime, view);

        {
            let runtime = runtime.clone();
            let view_weak = Rc::downgrade(view);
            view.connect_human_input(move |_kind| {
                let now = Instant::now();
                let entering_retreat = runtime
                    .human_input_age(now)
                    .is_none_or(|age| age >= HUMAN_INPUT_RETREAT);
                runtime.last_human_input.set(Some(now));
                if entering_retreat {
                    // Keep the accepted-input hot path O(1): hide once, then
                    // let the single frame callback measure and move it to the
                    // upper cell-aligned pose. Repeated keys only extend time.
                    if let Some(view) = view_weak.upgrade() {
                        // AltScreen already owns an immediate hide override;
                        // do not replace the desired post-rmcup visibility.
                        if !view.live_organism_surface_metrics().alt_screen {
                            view.set_live_organism_visible(false);
                        }
                    }
                }
            });
        }

        {
            let runtime = runtime.clone();
            view.connect_activity(move || runtime.output_activity.set(true));
        }

        {
            let runtime = runtime.clone();
            let view_weak = Rc::downgrade(view);
            let memory = self.organism_memory.clone();
            let shared_life = self.organism_life.clone();
            let correction = self.organism_correction.clone();
            let pane = pane_token(view);
            view.connect_command_started(move |event| {
                runtime.bump_generation();
                runtime.command_running.set(true);
                // Enter's retreat protected the editable prompt; once OSC C
                // establishes a running command, the dedicated watching pose
                // is already anchored away from that line.
                runtime.last_human_input.set(None);
                let kind = classify_command(&event.command);
                runtime.active_memory_kind.set(Some(kind));
                let repo_context =
                    if matches!(kind, CommandKind::BuildOrTest | CommandKind::GitPush) {
                        let mut memory = memory.borrow_mut();
                        memory.as_mut().and_then(|memory| {
                            if let Err(error) = memory.refresh() {
                                log::error!("could not refresh ASCII organism memory: {error}");
                            }
                            memory.context_now(event.cwd.as_deref())
                        })
                    } else {
                        None
                    };
                *runtime.active_repo_context.borrow_mut() = repo_context.clone();
                // The context key is the canonical repo root for build/push
                // commands. Volatile commands still inside the same checkout
                // reuse that root instead of their raw cwd, so interleaved
                // `ls`/`git status` from a subdirectory can never flap the key
                // and fake a context switch.
                let context_key = repo_context
                    .as_ref()
                    .map(|context| context.repo.clone())
                    .or_else(|| {
                        if same_checkout(
                            runtime.last_repo_root.borrow().as_deref(),
                            runtime.last_repo_cwd.borrow().as_deref(),
                            event.cwd.as_deref(),
                        ) {
                            runtime.last_repo_root.borrow().clone()
                        } else {
                            event.cwd.clone()
                        }
                    });
                let context_changed = *runtime.active_context_key.borrow() != context_key;
                *runtime.active_context_key.borrow_mut() = context_key;
                let reaction = {
                    let mut organism = runtime.organism.borrow_mut();
                    organism.sync_state(shared_life.get());
                    let today = local_day_at_ms(unix_ms());
                    if runtime.last_local_day.get().is_some_and(|day| day != today) {
                        // Volatile contexts otherwise never notice midnight.
                        organism.roll_over_day();
                    }
                    runtime.last_local_day.set(Some(today));
                    if let Some(context) = repo_context {
                        // Greeting/shyness keys on the resolved repository
                        // identity; the mixed root/cwd key cannot re-fire it.
                        let entered_new_repo = runtime.last_repo_root.borrow().as_deref()
                            != Some(context.repo.as_str());
                        organism.restore_repo_context(
                            context.open_failures,
                            context.recovered_pending_push,
                            context.successes_today,
                            context.failures_today,
                        );
                        if entered_new_repo {
                            organism.note_repo_arrival(RepoArrival::from_familiarity(
                                context.familiarity_days,
                            ));
                        }
                        *runtime.last_repo_root.borrow_mut() = Some(context.repo.clone());
                        *runtime.last_repo_cwd.borrow_mut() = event.cwd.clone();
                    } else if context_changed {
                        // A volatile/non-Git command genuinely left the last
                        // known checkout; never inherit a streak across a real
                        // context switch.
                        organism.restore_repo_context(0, false, 0, 0);
                        *runtime.last_repo_root.borrow_mut() = None;
                        *runtime.last_repo_cwd.borrow_mut() = None;
                    }
                    if correction.take_recent_accept(pane, Instant::now()) {
                        organism.note_assisted_command();
                    }
                    let reaction = organism.command_started(&event.command);
                    shared_life.set(organism.state());
                    reaction
                };
                runtime.render(&reaction);
                if let Some(view) = view_weak.upgrade() {
                    view.insert_inline_notice(&runtime.card);
                }
            });
        }

        {
            let runtime = runtime.clone();
            let view_weak = Rc::downgrade(view);
            let memory = self.organism_memory.clone();
            let shared_life = self.organism_life.clone();
            view.connect_command_finished(move |event| {
                let generation = runtime.bump_generation();
                runtime.command_running.set(false);
                // Show the authoritative result for the complete hold window.
                // Any genuinely new prompt input will immediately replace this
                // with a fresh sliding retreat.
                runtime.last_human_input.set(None);
                let mut reaction = {
                    let mut organism = runtime.organism.borrow_mut();
                    organism.sync_state(shared_life.get());
                    let reaction = organism.command_finished(
                        &event.command,
                        event.exit_code,
                        event.duration_ms,
                    );
                    shared_life.set(organism.state());
                    reaction
                };
                let classified = classify_command(&event.command);
                let kind = if classified == CommandKind::Other {
                    runtime.active_memory_kind.take().unwrap_or(classified)
                } else {
                    runtime.active_memory_kind.take();
                    classified
                };
                let state = shared_life.get();
                let repo = runtime
                    .active_repo_context
                    .borrow_mut()
                    .take()
                    .map(|context| context.repo);
                let memory_event = MemoryEvent::now_for_repo(kind, event.exit_code, repo, state);
                if let Some(memory) = memory.borrow_mut().as_mut() {
                    // Merge transactions completed by other Forge windows as
                    // late as possible, so a recovery that lands while this
                    // command runs can still influence the visible reaction.
                    if let Err(error) = memory.refresh() {
                        log::error!("could not refresh ASCII organism memory: {error}");
                    }
                    let (insight, persist_result) = memory.apply_and_enqueue(memory_event);
                    if kind == CommandKind::BuildOrTest {
                        match event.exit_code {
                            Some(code) if code != 0 && insight.open_failures >= 2 => {
                                reaction.behavior = Behavior::SitNearError;
                                reaction.description.push_str(&format!(
                                    " · repo failure {}",
                                    insight.open_failures
                                ));
                            }
                            Some(0) if insight.recovered_failures > 0 => {
                                reaction.behavior = if insight.recovered_failures >= 3 {
                                    Behavior::CelebrateBig
                                } else {
                                    Behavior::Celebrate
                                };
                                reaction.speech = Some(if insight.recovered_failures >= 3 {
                                    "终于。"
                                } else {
                                    "好了。"
                                });
                                reaction.description.push_str(&format!(
                                    " · repo recovery after {} failure(s)",
                                    insight.recovered_failures
                                ));
                            }
                            _ => {}
                        }
                    }
                    if insight.faster_than_yesterday {
                        reaction.speech = Some("这次比昨天快。");
                        reaction.description.push_str(" · remembered this repo");
                    } else if insight.push_after_recovery && reaction.speech.is_none() {
                        // The build may have recovered before this window was
                        // restarted; repo memory still closes the loop.
                        reaction.speech = Some("收好了。");
                    }
                    if let Err(error) = persist_result {
                        log::error!("could not queue ASCII organism memory: {error}");
                        runtime.mark_volatile();
                    }
                }
                runtime.render(&reaction);
                if let Some(view) = view_weak.upgrade() {
                    view.insert_inline_notice(&runtime.card);
                }
                OrganismRuntime::settle_later(&runtime, view_weak.clone(), generation);
            });
        }

        {
            // CommandEnd arrives before the finished GTK block is committed at
            // the next PromptStart. Re-pin only; the reducer already consumed
            // the authoritative lifecycle event above.
            let runtime = runtime.clone();
            let view_weak = Rc::downgrade(view);
            view.connect_block_finished(
                move |_command, _exit_code, _output, _agent_generation, _duration_ms| {
                    if let Some(view) = view_weak.upgrade() {
                        view.insert_inline_notice(&runtime.card);
                    }
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_state_summary_reports_all_eight_bounded_dimensions() {
        let summary = state_summary(LifeState {
            energy: 0.0,
            mood: 0.1,
            curiosity: 0.2,
            boredom: 0.3,
            stress: 0.4,
            social_need: 0.5,
            attachment: 0.75,
            confidence: 1.0,
        });
        assert_eq!(summary, "E00 M10 C20 B30 S40 N50 A75 F100");
    }

    #[test]
    fn live_surface_fails_closed_when_the_complete_body_does_not_fit() {
        let tiny_width = SurfaceBox {
            width: 99,
            height: 80,
            right_gutter: 20,
            cell_width: 8,
            cell_height: 16,
            body_width: 64,
            body_height: 48,
        };
        let tiny_height = SurfaceBox {
            width: 300,
            height: 63,
            ..tiny_width
        };
        assert_eq!(surface_point(tiny_width, SurfaceMode::Idle, 0), None);
        assert_eq!(surface_point(tiny_height, SurfaceMode::Watching, 0), None);

        let unaligned_near_boundary = SurfaceBox {
            width: 64 + 20 + 16,
            height: 48 + 16 + 16,
            right_gutter: 20,
            cell_width: 7,
            cell_height: 16,
            body_width: 64,
            body_height: 48,
        };
        assert_eq!(
            surface_point(unaligned_near_boundary, SurfaceMode::Typing, 0),
            None
        );
    }

    #[test]
    fn every_surface_pose_is_clamped_left_of_the_scrollbar_gutter() {
        let surface = SurfaceBox {
            width: 320,
            height: 180,
            right_gutter: 20,
            cell_width: 8,
            cell_height: 16,
            body_width: 88,
            body_height: 48,
        };
        for mode in [
            SurfaceMode::Idle,
            SurfaceMode::Typing,
            SurfaceMode::Watching,
            SurfaceMode::Reacting,
        ] {
            for frame in [0, 1, 359, 360, 399, 400, 759, 799, u64::MAX] {
                let point = surface_point(surface, mode, frame).expect("body fits");
                assert!(point.x >= f64::from(SURFACE_MARGIN));
                assert!(point.y >= f64::from(SURFACE_MARGIN));
                assert_eq!(point.x as i32 % surface.cell_width, 0);
                assert_eq!(point.y as i32 % surface.cell_height, 0);
                assert!(
                    point.x + f64::from(surface.body_width)
                        <= f64::from(surface.width - surface.right_gutter - SURFACE_MARGIN)
                );
                assert!(
                    point.y + f64::from(surface.body_height)
                        <= f64::from(surface.height - SURFACE_MARGIN)
                );
            }
        }
    }

    #[test]
    fn accepted_typing_slides_the_body_away_then_expires() {
        assert_eq!(
            surface_mode(
                Behavior::Idle,
                false,
                Some(HUMAN_INPUT_RETREAT - Duration::from_millis(1))
            ),
            SurfaceMode::Typing
        );
        assert_eq!(
            surface_mode(Behavior::Idle, false, Some(HUMAN_INPUT_RETREAT)),
            SurfaceMode::Idle
        );
        assert_eq!(
            surface_mode(Behavior::WatchCommand, true, None),
            SurfaceMode::Watching
        );
        assert_eq!(
            surface_mode(Behavior::Celebrate, false, None),
            SurfaceMode::Reacting
        );
    }

    #[test]
    fn correction_accept_pulse_is_pane_scoped_single_use_and_expires() {
        let life = Rc::new(Cell::new(LifeState::default()));
        let signal = OrganismCorrectionSignal::new(life);
        let now = Instant::now();

        signal.note_accepted(7);
        assert!(!signal.take_recent_accept(9, now));
        assert!(signal.take_recent_accept(7, now));
        assert!(!signal.take_recent_accept(7, now));

        signal.note_accepted(7);
        assert!(!signal.take_recent_accept(7, now + CORRECTION_ASSIST_WINDOW * 2));

        signal.note_accepted(3);
        signal.revoke_accept(4);
        assert!(signal.take_recent_accept(3, Instant::now()));
        signal.note_accepted(3);
        signal.revoke_accept(3);
        assert!(!signal.take_recent_accept(3, Instant::now()));
    }

    #[test]
    fn dismissal_streak_resets_on_acceptance() {
        let life = Rc::new(Cell::new(LifeState::default()));
        let signal = OrganismCorrectionSignal::new(life);
        signal.note_dismissed();
        signal.note_dismissed();
        assert_eq!(signal.dismiss_streak.get(), 2);
        signal.note_accepted(1);
        assert_eq!(signal.dismiss_streak.get(), 0);
    }

    #[test]
    fn volatile_commands_inside_the_known_checkout_keep_their_context() {
        assert!(cwd_within("/repo", "/repo"));
        assert!(cwd_within("/repo/sub/dir", "/repo"));
        assert!(!cwd_within("/repository", "/repo"));
        assert!(!cwd_within("/tmp", "/repo"));

        assert!(same_checkout(Some("/repo"), None, Some("/repo/sub")));
        assert!(same_checkout(
            None,
            Some("/home/u/link"),
            Some("/home/u/link")
        ));
        assert!(!same_checkout(
            Some("/repo"),
            Some("/home/u/link"),
            Some("/tmp")
        ));
        assert!(!same_checkout(None, None, Some("/tmp")));
        assert!(!same_checkout(Some("/repo"), None, None));
    }

    #[test]
    fn idle_body_wanders_but_typing_uses_the_safe_upper_corner() {
        let surface = SurfaceBox {
            width: 360,
            height: 200,
            right_gutter: 20,
            cell_width: 8,
            cell_height: 16,
            body_width: 88,
            body_height: 48,
        };
        let idle_left = surface_point(surface, SurfaceMode::Idle, 0).unwrap();
        let idle_still = surface_point(surface, SurfaceMode::Idle, 359).unwrap();
        let idle_right = surface_point(surface, SurfaceMode::Idle, 400).unwrap();
        let typing = surface_point(surface, SurfaceMode::Typing, 0).unwrap();
        assert_eq!(idle_still, idle_left);
        assert!(idle_right.x > idle_left.x);
        assert_eq!(typing.x, idle_right.x);
        assert!(typing.y < idle_left.y);
    }
}
