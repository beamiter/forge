//! Native Block-pane body for the experimental ASCII organism.

use gtk4::prelude::*;
use gtk4::{Box as GBox, Label, Orientation};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use super::UiState;
use crate::block_view::TermView;
use crate::organism::{LifeState, NativeOrganism, Reaction, Tone};

const REACTION_HOLD: Duration = Duration::from_millis(8_000);
const TONE_CLASSES: [&str; 5] = [
    "organism-quiet",
    "organism-active",
    "organism-success",
    "organism-error",
    "organism-warning",
];

struct OrganismRuntime {
    organism: RefCell<NativeOrganism>,
    generation: Cell<u64>,
    settle_timer: RefCell<Option<gtk4::glib::SourceId>>,
    card: gtk4::Widget,
    sprite: Label,
    status: Label,
    state: Label,
}

impl OrganismRuntime {
    fn new() -> Rc<Self> {
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
        let badge = Label::new(Some("native Block events · no LLM"));
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

        let runtime = Rc::new(Self {
            organism: RefCell::new(NativeOrganism::default()),
            generation: Cell::new(0),
            settle_timer: RefCell::new(None),
            card: outer.upcast(),
            sprite,
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
        }
        self.card.add_css_class(match reaction.tone {
            Tone::Quiet => "organism-quiet",
            Tone::Active => "organism-active",
            Tone::Success => "organism-success",
            Tone::Error => "organism-error",
            Tone::Warning => "organism-warning",
        });
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
    pub(crate) fn attach_ascii_organism_to_view(&self, view: &Rc<TermView>) {
        if !self.config.borrow().ascii_organism_enabled {
            return;
        }

        let runtime = OrganismRuntime::new();
        view.insert_inline_notice(&runtime.card);

        {
            let runtime = runtime.clone();
            let view_weak = Rc::downgrade(view);
            view.connect_command_started(move |event| {
                runtime.bump_generation();
                let reaction = runtime
                    .organism
                    .borrow_mut()
                    .command_started(&event.command);
                runtime.render(&reaction);
                if let Some(view) = view_weak.upgrade() {
                    view.insert_inline_notice(&runtime.card);
                }
            });
        }

        {
            let runtime = runtime.clone();
            let view_weak = Rc::downgrade(view);
            view.connect_command_finished(move |event| {
                let generation = runtime.bump_generation();
                let reaction = runtime.organism.borrow_mut().command_finished(
                    &event.command,
                    event.exit_code,
                    event.duration_ms,
                );
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
}
