//! Native Block-pane body for the experimental ASCII organism.

use gtk4::prelude::*;
use gtk4::{Box as GBox, Label, Orientation};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use super::UiState;
use crate::block_view::TermView;
use crate::organism::{
    classify_command, Behavior, CommandKind, LifeState, NativeOrganism, Reaction, Tone,
};
use crate::organism_memory::{MemoryEvent, RepoContext};

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
    active_memory_kind: Cell<Option<CommandKind>>,
    active_context_key: RefCell<Option<String>>,
    active_repo_context: RefCell<Option<RepoContext>>,
    generation: Cell<u64>,
    settle_timer: RefCell<Option<gtk4::glib::SourceId>>,
    card: gtk4::Widget,
    sprite: Label,
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

        let runtime = Rc::new(Self {
            organism: RefCell::new(NativeOrganism::from_persisted_state(initial_state)),
            active_memory_kind: Cell::new(None),
            active_context_key: RefCell::new(None),
            active_repo_context: RefCell::new(None),
            generation: Cell::new(0),
            settle_timer: RefCell::new(None),
            card: outer.upcast(),
            sprite,
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

        {
            let runtime = runtime.clone();
            let view_weak = Rc::downgrade(view);
            let memory = self.organism_memory.clone();
            let shared_life = self.organism_life.clone();
            view.connect_command_started(move |event| {
                runtime.bump_generation();
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
                let context_key = repo_context
                    .as_ref()
                    .map(|context| context.repo.clone())
                    .or_else(|| event.cwd.clone());
                let context_changed = *runtime.active_context_key.borrow() != context_key;
                *runtime.active_context_key.borrow_mut() = context_key;
                let reaction = {
                    let mut organism = runtime.organism.borrow_mut();
                    organism.sync_state(shared_life.get());
                    if let Some(context) = repo_context {
                        organism.restore_repo_context(
                            context.open_failures,
                            context.recovered_pending_push,
                        );
                    } else if context_changed {
                        // Volatile/non-Git commands retain a streak while they
                        // stay in the same cwd, but never inherit one after a
                        // real context switch.
                        organism.restore_repo_context(0, false);
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
}
