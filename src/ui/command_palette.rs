//! Unified fuzzy command palette over actions, persistent history, workflows
//! and the review-first AI command entry.

use adw::prelude::*;
use gtk4::gdk::{Key, ModifierType};
use gtk4::{
    Box as GBox, Button, EventControllerKey, Image, Label, ListBox, Orientation, ScrolledWindow,
    SearchEntry, Spinner,
};
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use super::command_review::{
    set_review_feedback, CommandReviewCard, CommandReviewSpec, ReviewPresentation,
};
use super::UiState;
use crate::block_view::TermView;
use crate::palette::{Accept, Entry, HistoryEntry, PaletteMode, Query};

const MAX_PALETTE_QUERY_CHARS: i32 = 16 * 1024;

/// The live header search, action, history and workflow entry points all use
/// this constructor, so new palette modes retain the workspace surface scope.
fn unified_palette_dialog(mode: PaletteMode) -> adw::Dialog {
    let dialog = adw::Dialog::builder()
        .title(palette_title(mode))
        .content_width(620)
        .content_height(520)
        .build();
    super::reference_shell::style_workspace_dialog(&dialog);
    dialog
}

/// One live review-first natural-language command suggestion. It is kept
/// separate from the multi-turn Shell Agent state machine, but intentionally
/// uses the same Block-pinned review surface and interaction vocabulary.
pub(crate) struct CommandSuggestionHandle {
    runtime: Rc<CommandSuggestionRuntime>,
}

impl CommandSuggestionHandle {
    pub(crate) fn shutdown(&self) {
        self.runtime.shutdown();
    }
}

struct CommandSuggestionRuntime {
    target: Rc<TermView>,
    config: Rc<RefCell<crate::config::Config>>,
    slot: Weak<RefCell<Option<CommandSuggestionHandle>>>,
    card: gtk4::Widget,
    review_box: GBox,
    status: Label,
    spinner: Spinner,
    stop: Button,
    retry: Button,
    request: String,
    cwd: String,
    shell: String,
    block_context: Option<crate::ai::BlockContext>,
    cancellation: RefCell<Option<crate::ai::AiCancellationToken>>,
    busy: Cell<bool>,
    alive: Cell<bool>,
}

impl CommandSuggestionRuntime {
    fn set_status(&self, message: &str, active: bool, error: bool) {
        self.status
            .set_text(&jterm_core::review_input::safe_inline_display(
                message,
                16 * 1024,
            ));
        if error {
            self.status.add_css_class("error");
        } else {
            self.status.remove_css_class("error");
        }
        if active {
            self.spinner.start();
        } else {
            self.spinner.stop();
        }
        self.stop.set_visible(active);
        self.stop.set_sensitive(active);
        self.retry.set_visible(!active && error && self.alive.get());
        self.retry
            .set_sensitive(!active && error && self.alive.get());
    }

    fn clear_review(&self) {
        while let Some(child) = self.review_box.first_child() {
            self.review_box.remove(&child);
        }
        self.review_box.set_visible(false);
    }

    fn stop_current_request(&self) {
        if !self.alive.get() || !self.busy.get() {
            return;
        }
        if let Some(cancellation) = self.cancellation.borrow().as_ref() {
            cancellation.cancel();
            self.stop.set_sensitive(false);
            self.set_status("Stopping this suggestion request…", true, false);
        }
    }

    fn request_model(runtime: Rc<Self>) {
        if !runtime.alive.get() || runtime.busy.get() {
            return;
        }
        let client = match crate::ai::client_from_config(&runtime.config.borrow()) {
            Ok(client) => client,
            Err(error) => {
                runtime.set_status(&error.to_string(), false, true);
                return;
            }
        };
        runtime.clear_review();
        runtime.busy.set(true);
        let provider = client.display_name();
        runtime.set_status(
            &format!("Drafting with {provider} for this Block pane…"),
            true,
            false,
        );
        let cancellation = crate::ai::AiCancellationToken::new();
        *runtime.cancellation.borrow_mut() = Some(cancellation.clone());
        let request = runtime.request.clone();
        let cwd = runtime.cwd.clone();
        let shell = runtime.shell.clone();
        let block_context = runtime.block_context.clone();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        if let Err(error) = crate::ai::spawn_request_worker("forge-command-suggestion", move || {
            let result = crate::ai::nl_to_command_with_context_blocking_cancellable(
                &client,
                &request,
                &cwd,
                &shell,
                std::env::consts::OS,
                block_context.as_ref(),
                &cancellation,
            );
            let _ = tx.send(result);
        }) {
            log::warn!("forge-command-suggestion could not start: {error}");
        }

        let rx = RefCell::new(rx);
        gtk4::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            if !runtime.alive.get() {
                return gtk4::glib::ControlFlow::Break;
            }
            match rx.borrow().try_recv() {
                Ok(Ok(command)) => {
                    runtime.cancellation.borrow_mut().take();
                    runtime.busy.set(false);
                    runtime.set_status(
                        "Review the proposal below. Nothing has been inserted or run.",
                        false,
                        false,
                    );
                    Self::render_proposal(&runtime, command, &provider);
                    gtk4::glib::ControlFlow::Break
                }
                Ok(Err(error)) => {
                    runtime.cancellation.borrow_mut().take();
                    runtime.busy.set(false);
                    let message = if matches!(error, crate::ai::AiError::Cancelled) {
                        "Suggestion request stopped. Retry when ready.".to_string()
                    } else {
                        format!("Command suggestion failed: {error}")
                    };
                    runtime.set_status(&message, false, true);
                    gtk4::glib::ControlFlow::Break
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => gtk4::glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    runtime.cancellation.borrow_mut().take();
                    runtime.busy.set(false);
                    runtime.set_status(
                        "Command suggestion worker disconnected. Retry when ready.",
                        false,
                        true,
                    );
                    gtk4::glib::ControlFlow::Break
                }
            }
        });
    }

    fn render_proposal(runtime: &Rc<Self>, command: String, provider: &str) {
        runtime.clear_review();
        let command = match jterm_core::review_input::validate(&command) {
            Ok(command) => command.to_string(),
            Err(error) => {
                runtime.set_status(
                    &format!("AI returned an unsafe command: {error}. Regenerate to try again."),
                    false,
                    true,
                );
                return;
            }
        };
        runtime.review_box.set_visible(true);
        let review = CommandReviewCard::new(CommandReviewSpec {
            presentation: ReviewPresentation::Embedded,
            compact: runtime.config.borrow().block_compact,
            icon: "dialog-information-symbolic",
            title: "Command proposal".to_string(),
            badge: jterm_core::review_input::safe_inline_display(provider, 1024),
            description: format!("Generated for: {}", compact_one_line(&runtime.request, 140)),
            command,
            primary_label: "Insert for review".to_string(),
            primary_executes: false,
            auxiliary_label: None,
            secondary_label: Some("Regenerate".to_string()),
            close_button: false,
        });
        runtime.review_box.append(&review.root);

        if let Some(regenerate) = review.secondary.as_ref() {
            let weak = Rc::downgrade(runtime);
            regenerate.connect_clicked(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    Self::request_model(runtime);
                }
            });
        }
        {
            let weak = Rc::downgrade(runtime);
            let entry = review.entry.clone();
            let feedback = review.feedback.clone();
            review.primary.connect_clicked(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    Self::insert_for_review(runtime, &entry, &feedback);
                }
            });
        }
        {
            let weak = Rc::downgrade(runtime);
            let feedback = review.feedback.clone();
            review.entry.connect_activate(move |entry| {
                if let Some(runtime) = weak.upgrade() {
                    Self::insert_for_review(runtime, entry, &feedback);
                }
            });
        }
        review.focus();
    }

    fn insert_for_review(runtime: Rc<Self>, entry: &gtk4::Entry, feedback: &Label) {
        let command = match jterm_core::review_input::validate(&entry.text()) {
            Ok(command) => command.to_string(),
            Err(error) => {
                set_review_feedback(feedback, &format!("Cannot insert: {error}"), true);
                return;
            }
        };
        let prompt_status = runtime.target.command_prompt_status();
        if !prompt_status.is_ready() {
            set_review_feedback(feedback, prompt_status.blocked_message(), true);
            return;
        }
        runtime.target.grab_focus();
        if let Err(error) = runtime.target.write_input(command.as_bytes()) {
            set_review_feedback(
                feedback,
                &format!("Command was not inserted: {error}"),
                true,
            );
            return;
        }
        Self::dismiss(runtime);
    }

    fn dismiss(runtime: Rc<Self>) {
        if let Some(slot) = runtime.slot.upgrade() {
            let is_current = slot
                .borrow()
                .as_ref()
                .is_some_and(|handle| Rc::ptr_eq(&handle.runtime, &runtime));
            if is_current {
                slot.borrow_mut().take();
            }
        }
        runtime.shutdown();
    }

    fn shutdown(&self) {
        if !self.alive.replace(false) {
            return;
        }
        if let Some(cancellation) = self.cancellation.borrow_mut().take() {
            cancellation.cancel();
            if !cancellation.wait_for_inactive(std::time::Duration::from_millis(500)) {
                log::warn!("Timed out waiting for the command suggestion request to stop");
            }
        }
        self.busy.set(false);
        self.spinner.stop();
        self.target.remove_inline_notice(&self.card);
    }
}

fn compact_one_line(text: &str, max_chars: usize) -> String {
    let text = jterm_core::review_input::safe_inline_display(text, 16 * 1024);
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = collapsed.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn clear_rows(list: &ListBox) {
    while let Some(row) = list.row_at_index(0) {
        list.remove(&row);
    }
}

fn render_rows(list: &ListBox, entries: &[Entry]) {
    clear_rows(list);
    for entry in entries {
        let title = jterm_core::review_input::safe_inline_display(&entry.label, 16 * 1024);
        let row = adw::ActionRow::builder()
            .title(&title)
            .use_markup(false)
            .activatable(true)
            .build();
        if let Some(subtitle) = entry.sublabel.as_deref() {
            row.set_subtitle(&jterm_core::review_input::safe_inline_display(
                subtitle,
                16 * 1024,
            ));
        }
        if let Some(right) = entry.right.as_deref() {
            let right = jterm_core::review_input::safe_inline_display(right, 4 * 1024);
            let label = Label::new(Some(&right));
            label.add_css_class("dim-label");
            row.add_suffix(&label);
        }
        list.append(&row);
    }
    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }
}

struct PaletteSnapshot {
    history: Vec<HistoryEntry>,
    workflows: Vec<crate::workflows::Workflow>,
}

const MAX_PALETTE_HISTORY: usize = 2_000;

struct PaletteHistoryLoad {
    entries: Vec<HistoryEntry>,
    failed: bool,
}

fn start_palette_history_load(
    live: Vec<String>,
    load: impl FnOnce() -> Result<Vec<jterm_core::command_history::CommandHistoryRecord>, ()>
        + Send
        + 'static,
) -> jterm_core::workflows::BackgroundRefresh<PaletteHistoryLoad> {
    let state = jterm_core::workflows::BackgroundRefresh::new(PaletteHistoryLoad {
        entries: merge_palette_history(live.clone(), Vec::new(), MAX_PALETTE_HISTORY),
        failed: false,
    });
    state.refresh(move || match load() {
        Ok(persisted) => PaletteHistoryLoad {
            entries: merge_palette_history(live, persisted, MAX_PALETTE_HISTORY),
            failed: false,
        },
        Err(()) => PaletteHistoryLoad {
            entries: merge_palette_history(live, Vec::new(), MAX_PALETTE_HISTORY),
            failed: true,
        },
    });
    state
}

/// Merge newest-first live Block commands with the newest-first JSONL tail.
/// Live commands win duplicate identity because they are the UI's freshest
/// observation; persisted metadata remains available for older commands.
fn merge_palette_history(
    live: Vec<String>,
    persisted: Vec<jterm_core::command_history::CommandHistoryRecord>,
    limit: usize,
) -> Vec<HistoryEntry> {
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::with_capacity(limit.min(live.len().saturating_add(persisted.len())));
    for entry in live
        .into_iter()
        .map(|command| HistoryEntry {
            command,
            cwd: None,
            exit_code: None,
        })
        .chain(persisted.into_iter().map(HistoryEntry::from))
    {
        if merged.len() == limit {
            break;
        }
        if jterm_core::review_input::validate(&entry.command).is_ok()
            && seen.insert(entry.command.clone())
        {
            merged.push(entry);
        }
    }
    merged
}

fn palette_target_matches<T: PartialEq>(
    expected: Option<&T>,
    current: Option<&T>,
    mapped: bool,
) -> bool {
    mapped && expected.is_some() && expected == current
}

fn palette_title(mode: PaletteMode) -> &'static str {
    match mode {
        PaletteMode::History => "Command History",
        PaletteMode::Workflows => "Workflows",
        _ => "Command Palette",
    }
}

fn palette_placeholder(mode: PaletteMode) -> &'static str {
    match mode {
        PaletteMode::History => "Filter history · > actions · : workflows · ? ask AI",
        PaletteMode::Workflows => "Filter workflows · > actions · @ history · ? ask AI",
        _ => "Search all · > actions · @ history · : workflows · ? ask AI",
    }
}

fn palette_toggle_key(mode: PaletteMode, key: Key, state: ModifierType) -> bool {
    if !state.contains(ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK) {
        return false;
    }
    match mode {
        PaletteMode::History => matches!(key, Key::H | Key::h),
        PaletteMode::Workflows => matches!(key, Key::M | Key::m),
        _ => matches!(key, Key::P | Key::p),
    }
}

impl UiState {
    /// The single UI boundary for review-only shell insertion. Every caller
    /// gets the same control-character rejection and visible failure instead
    /// of silently turning a multi-line value into submitted shell input.
    pub(crate) fn insert_review_text(&self, pane: &crate::ui::PaneLeaf, text: &str) -> bool {
        match pane.write_review_input(text) {
            Ok(()) => {
                pane.grab_focus();
                true
            }
            Err(error) => {
                log::warn!("refusing unsafe review-only shell input: {error}");
                let dialog = adw::AlertDialog::new(
                    Some("Command was not inserted"),
                    Some(&format!(
                        "This review-only action cannot insert the command because {error}."
                    )),
                );
                crate::ui::reference_shell::style_workspace_dialog(&dialog);
                dialog.add_response("ok", "OK");
                dialog.set_default_response(Some("ok"));
                dialog.present(Some(&self.window));
                false
            }
        }
    }

    fn palette_snapshot(
        &self,
    ) -> (
        PaletteSnapshot,
        jterm_core::workflows::BackgroundRefresh<PaletteHistoryLoad>,
    ) {
        let live = self
            .current_term_view()
            .map(|view| view.command_history_bounded(MAX_PALETTE_HISTORY))
            .unwrap_or_default();
        let request = {
            let config = self.config.borrow();
            config
                .command_history_enabled
                .then(|| {
                    config.command_history_path.as_ref().map(|path| {
                        (
                            std::path::PathBuf::from(path),
                            (config.command_history_max_entries as usize).min(MAX_PALETTE_HISTORY),
                        )
                    })
                })
                .flatten()
        };
        let history = if let Some((path, limit)) = request {
            start_palette_history_load(live, move || {
                jterm_core::command_history::read_recent(&path, limit).map_err(|error| {
                    log::warn!("command palette history snapshot: {error}");
                })
            })
        } else {
            jterm_core::workflows::BackgroundRefresh::new(PaletteHistoryLoad {
                entries: merge_palette_history(live, Vec::new(), MAX_PALETTE_HISTORY),
                failed: false,
            })
        };
        (
            PaletteSnapshot {
                history: history.snapshot().value.entries.clone(),
                workflows: self.workflow_library.snapshot().value.as_ref().clone(),
            },
            history,
        )
    }

    fn gather_palette_entries(
        &self,
        raw_query: &str,
        default_mode: PaletteMode,
        snapshot: &PaletteSnapshot,
    ) -> Vec<Entry> {
        crate::palette::gather(
            &Query::parse(raw_query, default_mode),
            &self.keybinding_map.borrow(),
            &snapshot.history,
            &snapshot.workflows,
            100,
        )
    }

    /// Render a workflow and insert it for review.
    ///
    /// The single insertion path for every workflow, zero-argument ones
    /// included. forge used to write `workflow.command` straight to the pane
    /// whenever `args` was empty, so the `{{ }}` literal-brace escape its own
    /// module documents was not applied there — `-d '{{"env":"prod"}}'`
    /// reached the prompt with the braces still doubled — and the template
    /// never crossed validation on that path at all.
    pub(crate) fn insert_workflow(
        &self,
        pane: &crate::ui::PaneLeaf,
        workflow: &crate::workflows::Workflow,
    ) {
        match crate::workflows::render(workflow, &std::collections::HashMap::new()) {
            Ok(command) => {
                self.insert_review_text(pane, &command);
            }
            Err(error) => {
                log::warn!("refusing unsafe workflow render: {error}");
                let alert = adw::AlertDialog::new(Some("Command was not inserted"), Some(&error));
                crate::ui::reference_shell::style_workspace_dialog(&alert);
                alert.add_response("ok", "OK");
                alert.set_default_response(Some("ok"));
                alert.present(Some(&self.window));
            }
        }
    }

    pub(super) fn palette_input_target(
        &self,
        target: &Option<gtk4::glib::WeakRef<gtk4::Widget>>,
    ) -> Option<super::PaneLeaf> {
        let root = target.as_ref()?.upgrade()?;
        let current = self.current_pane_leaf().map(|pane| pane.root_widget());
        if !palette_target_matches(Some(&root), current.as_ref(), root.is_mapped()) {
            return None;
        }
        super::PaneLeaf::from_widget(&root)
    }

    /// Input belongs to the opening's pane; global actions retain their own
    /// dispatch policy. Never retarget a history/workflow/AI choice after exit.
    fn accept_palette_entry(
        &self,
        entry: Entry,
        workflows: &[crate::workflows::Workflow],
        target: &Option<gtk4::glib::WeakRef<gtk4::Widget>>,
    ) {
        match entry.accept {
            Accept::Action(action) => self.execute_action(action),
            accept => {
                if matches!(&accept, Accept::TypeCommand(text) | Accept::AskAi(text) if text.is_empty())
                {
                    return;
                }
                let Some(pane) = self.palette_input_target(target) else {
                    self.toast_overlay.add_toast(adw::Toast::new(
                        "Nothing was inserted: the palette's original pane is no longer active.",
                    ));
                    return;
                };
                match accept {
                    Accept::TypeCommand(command) => {
                        self.insert_review_text(&pane, &command);
                    }
                    Accept::AskAi(request) => self.generate_command_for_review(request, pane),
                    Accept::RunWorkflow(path) => {
                        let Some(workflow) = workflows.iter().find(|workflow| {
                            workflow.source_path.as_deref() == Some(path.as_path())
                        }) else {
                            log::warn!(
                                "workflow disappeared before activation: {}",
                                jterm_core::review_input::safe_inline_display(
                                    &path.to_string_lossy(),
                                    crate::workflows::MAX_LOGGED_PATH_BYTES,
                                )
                            );
                            return;
                        };
                        if workflow.args.is_empty() {
                            self.insert_workflow(&pane, workflow);
                        } else {
                            self.show_workflow_args_dialog(workflow.clone(), pane);
                        }
                    }
                    Accept::Action(_) => unreachable!("global actions were dispatched above"),
                }
            }
        }
    }

    fn generate_command_for_review(&self, request: String, pane: super::PaneLeaf) {
        match crate::ai::client_from_config(&self.config.borrow()) {
            Ok(_) => {}
            Err(error) => {
                self.show_ai_error(&error.to_string());
                return;
            }
        }
        let Some(target) = pane.block_view() else {
            self.show_ai_error(
                "This session cannot show inline command suggestions. Create a Block or Chat session and try again.",
            );
            return;
        };
        // The suggestion is delivered as an inline card. A backend with
        // nowhere visible to mount one refuses here rather than running the
        // request and dropping its answer off-screen.
        if !target.supports_inline_notices() {
            self.show_ai_error(
                "This session cannot display an inline suggestion card. Create a Block or Chat session, or use AI Chat in the inspector.",
            );
            return;
        }
        if let Some(previous) = self.command_suggestion.borrow_mut().take() {
            previous.shutdown();
        }

        let compact = self.config.borrow().block_compact;
        let cwd = match target.cwd() {
            cwd if !cwd.is_empty() => cwd,
            _ => ".".to_string(),
        };
        let cwd_display = jterm_core::review_input::safe_inline_display(&cwd, 4 * 1024);
        let shell = self
            .shell_argv
            .borrow()
            .first()
            .cloned()
            .unwrap_or_else(|| "sh".to_string());
        let block_context = target.selected_block_context(80);

        let outer = GBox::new(Orientation::Vertical, 0);
        outer.add_css_class("block-finished");
        outer.add_css_class("block-assistant");
        outer.add_css_class("command-suggestion");
        outer.set_hexpand(true);
        outer.set_vexpand(false);
        if compact {
            outer.add_css_class("block-compact");
            outer.set_margin_top(1);
            outer.set_margin_bottom(1);
            outer.set_margin_start(4);
            outer.set_margin_end(4);
        } else {
            outer.set_margin_top(4);
            outer.set_margin_bottom(4);
            outer.set_margin_start(8);
            outer.set_margin_end(8);
        }

        let header = GBox::new(Orientation::Horizontal, 8);
        header.add_css_class("block-header");
        header.set_margin_start(if compact { 8 } else { 12 });
        header.set_margin_end(if compact { 6 } else { 8 });
        header.set_margin_top(if compact { 3 } else { 6 });
        header.set_margin_bottom(if compact { 1 } else { 2 });
        let icon = Image::from_icon_name("dialog-information-symbolic");
        icon.add_css_class("assistant-card-icon");
        icon.set_accessible_role(gtk4::AccessibleRole::Presentation);
        header.append(&icon);
        let title = Label::new(Some("AI command suggestion"));
        title.add_css_class("assistant-card-title");
        title.set_xalign(0.0);
        header.append(&title);
        let binding = Label::new(Some(&format!("{cwd_display} · review only")));
        binding.add_css_class("assistant-card-badge");
        binding.set_hexpand(true);
        binding.set_halign(gtk4::Align::End);
        binding.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        binding.set_tooltip_text(Some(&cwd_display));
        header.append(&binding);
        let close = Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some("Stop and dismiss this suggestion (Esc)"));
        close.update_property(&[gtk4::accessible::Property::Label(
            "Dismiss command suggestion",
        )]);
        header.append(&close);
        outer.append(&header);

        let body = GBox::new(Orientation::Vertical, 7);
        body.set_margin_start(if compact { 8 } else { 12 });
        body.set_margin_end(if compact { 8 } else { 12 });
        body.set_margin_top(2);
        body.set_margin_bottom(if compact { 7 } else { 11 });
        let request_label = Label::new(Some(&format!(
            "Request: {}",
            compact_one_line(&request, 180)
        )));
        request_label.add_css_class("command-review-description");
        request_label.set_xalign(0.0);
        request_label.set_wrap(true);
        request_label.set_selectable(true);
        body.append(&request_label);
        if let Some(context) = block_context.as_ref() {
            let context_label = Label::new(Some(&format!(
                "Selected Block context · exit {}{} · {}",
                context.exit_code,
                if context.truncated {
                    " · output truncated"
                } else {
                    ""
                },
                compact_one_line(&context.cmd, 72)
            )));
            context_label.add_css_class("assistant-context-chip");
            context_label.set_xalign(0.0);
            context_label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
            context_label.set_tooltip_text(Some(
                "Attached as bounded, untrusted command/output context for this request",
            ));
            body.append(&context_label);
        }

        let spinner = Spinner::new();
        let status = Label::new(Some("Preparing command suggestion…"));
        status.add_css_class("assistant-status");
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_hexpand(true);
        status.set_accessible_role(gtk4::AccessibleRole::Status);
        let retry = Button::with_label("Retry");
        retry.set_visible(false);
        let stop = Button::with_label("Stop");
        stop.add_css_class("destructive-action");
        stop.set_visible(false);
        let status_row = GBox::new(Orientation::Horizontal, 7);
        status_row.add_css_class("assistant-status-row");
        status_row.append(&spinner);
        status_row.append(&status);
        status_row.append(&retry);
        status_row.append(&stop);
        body.append(&status_row);
        let review_box = GBox::new(Orientation::Vertical, 0);
        review_box.set_visible(false);
        body.append(&review_box);
        let hint = Label::new(Some(
            "Enter uses the labelled primary action · generated commands never run automatically",
        ));
        hint.add_css_class("agent-input-hint");
        hint.set_xalign(0.0);
        hint.set_wrap(true);
        body.append(&hint);
        outer.append(&body);

        let card: gtk4::Widget = outer.clone().upcast();
        let runtime = Rc::new(CommandSuggestionRuntime {
            target: target.clone(),
            config: self.config.clone(),
            slot: Rc::downgrade(&self.command_suggestion),
            card: card.clone(),
            review_box,
            status,
            spinner,
            stop: stop.clone(),
            retry: retry.clone(),
            request,
            cwd,
            shell,
            block_context,
            cancellation: RefCell::new(None),
            busy: Cell::new(false),
            alive: Cell::new(true),
        });

        {
            let weak = Rc::downgrade(&runtime);
            close.connect_clicked(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    CommandSuggestionRuntime::dismiss(runtime);
                }
            });
        }
        {
            let weak = Rc::downgrade(&runtime);
            stop.connect_clicked(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    runtime.stop_current_request();
                }
            });
        }
        {
            let weak = Rc::downgrade(&runtime);
            retry.connect_clicked(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    CommandSuggestionRuntime::request_model(runtime);
                }
            });
        }
        {
            let weak = Rc::downgrade(&runtime);
            let keys = EventControllerKey::new();
            keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == Key::Escape {
                    if let Some(runtime) = weak.upgrade() {
                        CommandSuggestionRuntime::dismiss(runtime);
                    }
                    gtk4::glib::Propagation::Stop
                } else {
                    gtk4::glib::Propagation::Proceed
                }
            });
            outer.add_controller(keys);
        }
        {
            let weak = Rc::downgrade(&runtime);
            target.connect_exited(move |_| {
                if let Some(runtime) = weak.upgrade() {
                    CommandSuggestionRuntime::dismiss(runtime);
                }
            });
        }

        *self.command_suggestion.borrow_mut() = Some(CommandSuggestionHandle {
            runtime: runtime.clone(),
        });
        target.insert_inline_notice(&card);
        CommandSuggestionRuntime::request_model(runtime);
    }

    pub(crate) fn toggle_unified_command_palette(&self) {
        self.show_unified_command_palette(PaletteMode::All);
    }

    pub(crate) fn show_unified_command_palette(&self, initial_mode: PaletteMode) {
        // `force_close` emits `closed` synchronously; release this borrow first
        // so its callback can clear the slot without a RefCell re-entry panic.
        let dialog_to_close = self.command_palette_dialog.borrow_mut().take();
        if let Some(dialog) = dialog_to_close {
            dialog.force_close();
            return;
        }

        // Capture the current pane immediately; disk history and workflows
        // arrive in the background without delaying dialog presentation.
        let input_target = self
            .current_pane_leaf()
            .map(|pane| pane.root_widget().downgrade());
        let greeting_target = self.organism_greeting_target();
        let vte_presentation_target = self.vte_companion_target();
        let output_review_target = self.current_term_view().map(|view| Rc::downgrade(&view));
        let library = self.workflow_library.snapshot().value;
        // Queue this opening's history before optional workflow discovery.
        let (snapshot, history_load) = self.palette_snapshot();
        self.refresh_workflow_library();
        let snapshot = Rc::new(RefCell::new(snapshot));
        let dialog = unified_palette_dialog(initial_mode);
        let header = adw::HeaderBar::new();
        let filter = SearchEntry::new();
        if let Some(text) = filter
            .delegate()
            .and_then(|delegate| delegate.downcast::<gtk4::Text>().ok())
        {
            text.set_max_length(MAX_PALETTE_QUERY_CHARS);
        }
        filter.set_placeholder_text(Some(palette_placeholder(initial_mode)));
        filter.set_hexpand(true);

        let list = ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::Single);
        list.add_css_class("boxed-list");
        list.set_margin_start(12);
        list.set_margin_end(12);
        list.set_margin_bottom(12);
        let entries = Rc::new(RefCell::new(self.gather_palette_entries(
            "",
            initial_mode,
            &snapshot.borrow(),
        )));
        render_rows(&list, &entries.borrow());

        let scrolled = ScrolledWindow::builder()
            .hexpand(true)
            .vexpand(true)
            .child(&list)
            .build();
        let body = gtk4::Box::new(Orientation::Vertical, 0);
        filter.set_margin_start(12);
        filter.set_margin_end(12);
        filter.set_margin_top(8);
        filter.set_margin_bottom(8);
        body.append(&filter);
        let history_status = Label::new(Some("Loading command history…"));
        history_status.set_wrap(true);
        history_status.add_css_class("dim-label");
        history_status.set_visible(history_load.snapshot().loading);
        body.append(&history_status);
        let load_status = Label::new(Some("Refreshing workflows…"));
        load_status.set_wrap(true);
        load_status.add_css_class("dim-label");
        body.append(&load_status);
        body.append(&scrolled);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&body));
        dialog.set_child(Some(&toolbar));

        let rendered_query = Rc::new(RefCell::new(String::new()));
        let refresh_query = {
            let ui = self.clone();
            let list = list.clone();
            let entries = entries.clone();
            let snapshot = snapshot.clone();
            let rendered_query = rendered_query.clone();
            Rc::new(move |query: &str| {
                if *rendered_query.borrow() == query {
                    return;
                }
                let next = ui.gather_palette_entries(query, initial_mode, &snapshot.borrow());
                render_rows(&list, &next);
                *entries.borrow_mut() = next;
                *rendered_query.borrow_mut() = query.to_string();
            })
        };
        {
            let refresh_query = refresh_query.clone();
            filter.connect_search_changed(move |entry| refresh_query(&entry.text()));
        }

        let activate = {
            let ui = self.clone();
            let entries = entries.clone();
            let dialog = dialog.downgrade();
            let snapshot = snapshot.clone();
            Rc::new(move |index: usize| {
                let Some(entry) = entries.borrow().get(index).cloned() else {
                    return;
                };
                let Some(dialog) = dialog.upgrade() else {
                    return;
                };
                dialog.force_close();
                if let Accept::Action(
                    action @ (crate::keybindings::Action::CompareSelectedOutputs
                    | crate::keybindings::Action::ExportSelectedMarkdown
                    | crate::keybindings::Action::ExportSelectedJson),
                ) = &entry.accept
                {
                    ui.review_command_output_for(&output_review_target, *action);
                } else if matches!(
                    &entry.accept,
                    Accept::Action(crate::keybindings::Action::GreetAsciiOrganism)
                ) {
                    ui.greet_ascii_organism_for(&greeting_target);
                } else if matches!(
                    &entry.accept,
                    Accept::Action(crate::keybindings::Action::InspectAsciiOrganism)
                ) {
                    ui.inspect_ascii_organism_for(&greeting_target);
                } else if let Accept::Action(
                    action @ (crate::keybindings::Action::ExpandVteCompanion
                    | crate::keybindings::Action::CollapseVteCompanion),
                ) = &entry.accept
                {
                    ui.set_vte_companion_for(
                        &vte_presentation_target,
                        *action == crate::keybindings::Action::ExpandVteCompanion,
                    );
                } else {
                    ui.accept_palette_entry(entry, &snapshot.borrow().workflows, &input_target);
                }
            })
        };
        {
            let activate = activate.clone();
            list.connect_row_activated(move |list, row| {
                let index = row.index();
                if index >= 0 && list.row_at_index(index).as_ref() == Some(row) {
                    activate(index as usize);
                }
            });
        }

        let keys = palette_controller(
            initial_mode,
            &list,
            &dialog,
            self.command_palette_dialog.clone(),
            &filter,
            refresh_query,
            activate,
        );
        dialog.add_controller(keys);

        let refresh_rows = {
            let ui = self.clone();
            let snapshot = snapshot.clone();
            let entries = entries.clone();
            let list = list.downgrade();
            let filter = filter.downgrade();
            Rc::new(move || {
                let (Some(list), Some(filter)) = (list.upgrade(), filter.upgrade()) else {
                    return;
                };
                let selected = list.selected_row().and_then(|row| {
                    entries
                        .borrow()
                        .get(row.index() as usize)
                        .map(|entry| entry.accept.clone())
                });
                let query = filter.text();
                let next = ui.gather_palette_entries(&query, initial_mode, &snapshot.borrow());
                let selected = selected
                    .and_then(|accept| next.iter().position(|entry| entry.accept == accept));
                render_rows(&list, &next);
                if let Some(row) = selected.and_then(|index| list.row_at_index(index as i32)) {
                    list.select_row(Some(&row));
                }
                *entries.borrow_mut() = next;
                *rendered_query.borrow_mut() = query.to_string();
            })
        };
        {
            let snapshot = snapshot.clone();
            let refresh_rows = refresh_rows.clone();
            self.watch_workflow_library(&dialog, &load_status, library, move |workflows| {
                snapshot.borrow_mut().workflows = workflows.to_vec();
                refresh_rows();
            });
        }
        {
            let dialog = dialog.downgrade();
            let status = history_status.downgrade();
            let snapshot = snapshot.clone();
            gtk4::glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
                let (Some(dialog), Some(status)) = (dialog.upgrade(), status.upgrade()) else {
                    return gtk4::glib::ControlFlow::Break;
                };
                // Each opening owns its load handle and widgets. Retiring it
                // cannot update a reopened palette or another pane's snapshot.
                if dialog.parent().is_none() {
                    return gtk4::glib::ControlFlow::Break;
                }
                let state = history_load.snapshot();
                if state.loading {
                    return gtk4::glib::ControlFlow::Continue;
                }
                let failed = state.error.is_some() || state.value.failed;
                status.set_label(if failed {
                    "Command history could not load. Current pane commands remain available."
                } else {
                    ""
                });
                status.set_visible(failed);
                if snapshot.borrow().history != state.value.entries {
                    snapshot.borrow_mut().history = state.value.entries.clone();
                    refresh_rows();
                }
                gtk4::glib::ControlFlow::Break
            });
        }
        let slot = self.command_palette_dialog.clone();
        dialog.connect_closed(move |_| *slot.borrow_mut() = None);
        *self.command_palette_dialog.borrow_mut() = Some(dialog.clone());
        dialog.present(Some(&self.window));
        filter.grab_focus();
    }
}

/// Shared native controller wiring; pane capture and action routing stay in the
/// caller. The existing fixture exercises this same Return/Escape controller.
pub(super) fn palette_controller(
    initial_mode: PaletteMode,
    list: &ListBox,
    dialog: &adw::Dialog,
    dialog_slot: Rc<RefCell<Option<adw::Dialog>>>,
    filter: &SearchEntry,
    refresh_query: Rc<dyn Fn(&str)>,
    activate: Rc<dyn Fn(usize)>,
) -> EventControllerKey {
    let keys = EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let list = list.clone();
    let dialog = dialog.downgrade();
    let filter = filter.clone();
    keys.connect_key_pressed(move |_, key, _, state| {
        // The dialog owns this controller; callbacks must not own it back.
        let Some(dialog) = dialog.upgrade() else {
            return false.into();
        };
        if key == Key::Escape || palette_toggle_key(initial_mode, key, state) {
            dialog_slot.borrow_mut().take();
            dialog.force_close();
            return true.into();
        }
        if matches!(key, Key::Return | Key::KP_Enter) {
            // search-changed is delayed; accept against the text that is
            // visible now, while retaining selection if it has not changed.
            refresh_query(&filter.text());
            if let Some(row) = list.selected_row() {
                activate(row.index() as usize);
            }
            return true.into();
        }
        if matches!(key, Key::Down | Key::Up) {
            let current = list.selected_row().map(|row| row.index()).unwrap_or(0);
            let next = if key == Key::Down {
                current.saturating_add(1)
            } else {
                current.saturating_sub(1)
            };
            if let Some(row) = list.row_at_index(next) {
                list.select_row(Some(&row));
            }
            return true.into();
        }
        false.into()
    });
    keys
}

#[cfg(test)]
mod tests {
    use super::{compact_one_line, merge_palette_history, palette_toggle_key};

    #[test]
    #[ignore = "requires DISPLAY; run on its own GTK main thread"]
    fn native_unified_palette_retains_workspace_theme_scope() {
        use libadwaita::prelude::*;
        libadwaita::init().expect("GTK/Adwaita display");
        for mode in [
            PaletteMode::All,
            PaletteMode::History,
            PaletteMode::Workflows,
        ] {
            let dialog = super::unified_palette_dialog(mode);
            assert!(dialog.has_css_class("forge-workspace-dialog"));
            assert!(!dialog.has_css_class("forge-font-chrome"));
        }
    }

    use crate::palette::PaletteMode;
    use gtk4::gdk::{Key, ModifierType};

    #[test]
    fn explicit_hello_keeps_the_palette_opening_owner_and_closes_before_request() {
        let source = include_str!("command_palette.rs");
        let open = source
            .split_once(concat!(
                "    pub(crate) fn show_unified_",
                "command_palette("
            ))
            .unwrap()
            .1
            .split_once("#[cfg(test)]")
            .unwrap()
            .0;
        assert!(open.contains("let greeting_target = self.organism_greeting_target();"));
        let activate = open
            .split_once("let activate = {")
            .unwrap()
            .1
            .split_once("let keys =")
            .unwrap()
            .0;
        assert!(
            activate.find("dialog.force_close()").unwrap()
                < activate.find("greet_ascii_organism_for").unwrap()
        );
        assert!(activate.contains("ui.greet_ascii_organism_for(&greeting_target)"));
        assert!(!activate.contains("organism_greeting_target()"));
        assert!(!activate.contains("current_term_view"));
    }

    #[test]
    fn palette_previews_neutralise_untrusted_formatting() {
        let preview = compact_one_line("show\n\u{202e}\u{fff0}\u{e0080}files", 80);

        assert_eq!(preview, "show����files");
        assert!(!preview.contains('\n'));
        assert!(!preview.contains('\u{202e}'));
        assert!(!preview.contains('\u{fff0}'));
        assert!(!preview.contains('\u{e0080}'));
    }

    #[test]
    fn palette_rows_disable_markup_for_literal_shell_text() {
        // A source-wiring check keeps this regression independent of a display
        // server. ActionRow interprets titles and subtitles as markup by
        // default, while safe_inline_display deliberately preserves shell
        // punctuation. The row must explicitly opt out of that interpretation.
        let source = include_str!("command_palette.rs");
        let render_rows = source
            .split_once("fn render_rows(")
            .unwrap()
            .1
            .split_once("\nstruct PaletteSnapshot")
            .unwrap()
            .0;
        assert!(render_rows.contains(".use_markup(false)"));

        for command in [
            "printf '<b>hello</b>'",
            "echo a && echo b",
            "cat < input.txt",
        ] {
            assert_eq!(
                jterm_core::review_input::safe_inline_display(command, 16 * 1024),
                command
            );
        }
        let (_, parsed, _) = gtk4::pango::parse_markup("printf '<b>hello</b>'", '\0').unwrap();
        assert_eq!(parsed.as_str(), "printf 'hello'");
        assert!(gtk4::pango::parse_markup("echo a && echo b", '\0').is_err());
        assert!(gtk4::pango::parse_markup("cat < input.txt", '\0').is_err());
    }

    #[test]
    fn enter_refreshes_the_current_query_before_resolving_selection() {
        // Source-wiring regression, independent of a display server. Native
        // GTK signal ordering still requires an integration test.
        let source = include_str!("command_palette.rs");
        let enter = source
            .split_once("if matches!(key, Key::Return | Key::KP_Enter) {")
            .unwrap()
            .1
            .split_once("return true.into();")
            .unwrap()
            .0;
        let refresh = enter.find("refresh_query(&filter.text());").unwrap();
        let selection = enter.find("list.selected_row()").unwrap();
        let activation = enter.find("activate(row.index() as usize)").unwrap();
        assert!(refresh < selection && selection < activation);
    }

    #[test]
    fn palette_history_snapshot_is_recent_first_and_deduplicated_across_sources() {
        let persisted = vec![
            jterm_core::command_history::CommandHistoryRecord {
                command: "cargo test".into(),
                cwd: Some("/repo".into()),
                exit_code: 0,
                end_time_ms: Some(20),
            },
            jterm_core::command_history::CommandHistoryRecord {
                command: "git status".into(),
                cwd: Some("/repo".into()),
                exit_code: 1,
                end_time_ms: Some(10),
            },
        ];
        let merged = merge_palette_history(
            vec!["cargo fmt".into(), "cargo test".into()],
            persisted,
            100,
        );

        assert_eq!(
            merged
                .iter()
                .map(|entry| entry.command.as_str())
                .collect::<Vec<_>>(),
            ["cargo fmt", "cargo test", "git status"]
        );
        assert_eq!(merged[1].exit_code, None, "live history wins duplicates");
        assert_eq!(merged[2].exit_code, Some(1));
    }

    #[test]
    fn initial_palette_modes_keep_their_own_toggle_shortcut() {
        let ctrl_shift = ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK;
        assert!(palette_toggle_key(PaletteMode::All, Key::p, ctrl_shift));
        assert!(palette_toggle_key(PaletteMode::History, Key::h, ctrl_shift));
        assert!(palette_toggle_key(
            PaletteMode::Workflows,
            Key::m,
            ctrl_shift
        ));
        assert!(!palette_toggle_key(
            PaletteMode::History,
            Key::p,
            ctrl_shift
        ));
    }
    fn wait_history(state: &jterm_core::workflows::BackgroundRefresh<super::PaletteHistoryLoad>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.snapshot().loading {
            assert!(
                std::time::Instant::now() < deadline,
                "history worker did not finish"
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    #[test]
    fn palette_history_live_commands_are_available_while_disk_load_is_blocked() {
        let (release, wait) = std::sync::mpsc::channel();
        let state = super::start_palette_history_load(vec!["cargo test".into()], move || {
            wait.recv().unwrap();
            Ok(vec![jterm_core::command_history::CommandHistoryRecord {
                command: "git status".into(),
                cwd: None,
                exit_code: 0,
                end_time_ms: None,
            }])
        });
        let initial = state.snapshot();
        assert!(initial.loading);
        assert_eq!(initial.value.entries[0].command, "cargo test");
        assert_eq!(initial.value.entries.len(), 1);
        release.send(()).unwrap();
        wait_history(&state);
        let completed = state.snapshot();
        assert_eq!(
            completed
                .value
                .entries
                .iter()
                .map(|entry| entry.command.as_str())
                .collect::<Vec<_>>(),
            ["cargo test", "git status"]
        );
        assert!(!completed.value.failed);
    }

    #[test]
    fn palette_history_reopen_has_separate_results_and_failure_keeps_live_commands() {
        let (release, wait) = std::sync::mpsc::channel();
        let (started, running) = std::sync::mpsc::channel();
        let old = super::start_palette_history_load(vec!["old pane".into()], move || {
            started.send(()).unwrap();
            wait.recv().unwrap();
            Ok(Vec::new())
        });
        running
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let reopened = super::start_palette_history_load(vec!["new pane".into()], || Err(()));
        drop(old);
        release.send(()).unwrap();
        wait_history(&reopened);
        let completed = reopened.snapshot();
        assert!(completed.value.failed);
        assert_eq!(completed.value.entries.len(), 1);
        assert_eq!(completed.value.entries[0].command, "new pane");
    }
    #[test]
    fn palette_input_rejects_closed_switched_or_missing_opening_targets() {
        assert!(super::palette_target_matches(Some(&1), Some(&1), true));
        assert!(!super::palette_target_matches(Some(&1), Some(&2), true));
        assert!(!super::palette_target_matches(Some(&1), None, true));
        assert!(!super::palette_target_matches(Some(&1), Some(&1), false));
        assert!(!super::palette_target_matches(None::<&u64>, Some(&1), true));
    }

    #[test]
    fn palette_input_and_ai_dispatch_use_the_captured_pane() {
        let source = include_str!("command_palette.rs");
        let accept = source
            .split("    fn accept_palette_entry(")
            .nth(1)
            .unwrap()
            .split("    fn generate_command_for_review(")
            .next()
            .unwrap();
        assert!(accept.contains("self.palette_input_target(target)"));
        assert!(!accept.contains("current_pane_leaf()"));
        assert!(accept.contains("self.insert_review_text(&pane, &command)"));
        assert!(accept.contains("self.generate_command_for_review(request, pane)"));
        assert!(accept.contains("self.show_workflow_args_dialog(workflow.clone(), pane)"));
        let ai = source
            .split("    fn generate_command_for_review(")
            .nth(1)
            .unwrap()
            .split("    pub(crate) fn toggle_unified_command_palette(")
            .next()
            .unwrap();
        assert!(ai.contains("pane.block_view()"));
        assert!(!ai.contains("current_term_view()"));
    }
}
