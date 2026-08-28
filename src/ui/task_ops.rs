//! Application-side execution for the agent Tasks panel.
//!
//! Plain-GTK port of anvil's Relm4 `task_ops.rs`: the panel stages
//! [`TaskPanelAction`] values; this module resolves the task again at
//! execution time, drives the ported `agent_task` domain (task manager,
//! native runtime, diff worker), spawns task terminals as pruned tabs, and
//! pushes composed snapshots back to the panel. The flow mirrors ember's
//! `app/tasks.rs` action executor and poll loop.
//!
//! Unlike anvil's per-pane `Pane` records, forge keeps task terminal markers
//! on the [`PaneLeaf`] itself (GObject data), so they survive tab moves and
//! split rearrangements without any index bookkeeping. Validation cwd pins are
//! keyed by the synthetic task session id for the same reason, and are
//! released from the spawn-resolved hook VTE runs once the child has entered
//! its working directory (forge has no `PaneLaunched` message).

use std::collections::HashMap;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use gtk4::glib;
use libadwaita as adw;

use crate::agent_task::{
    AgentProvider, AgentSessionOutcome, ApprovalDecision, CodexAppServerPhase, NativePromptPolicy,
    NewTask, TaskId, TaskManager, TaskRuntimeKind, TaskStatus, TaskTerminalRole,
    TaskValidationStatus,
};
use crate::agent_task_ui::{
    self, order_task_rows, row_status_line, TaskPanelAction, TaskRowSnapshot,
};
use crate::ui::tasks_panel::{DiffSync, TaskDetailSync, TasksPanelSync};
use crate::ui::{PaneLeaf, UiState};

/// Poll cadence while any task machinery is active; the runtime's own frame
/// budgets make each tick a bounded, nonblocking drain.
const TASKS_POLL_FAST: Duration = Duration::from_millis(120);
/// Idle cadence while tasks exist but nothing is running; keeps archived
/// tasks from ever waking the loop.
const TASKS_POLL_SLOW: Duration = Duration::from_millis(2_000);

/// Native Codex agent task domain owned by the window: the reducer, the
/// app-server runtime, and the single-flight diff worker, plus the panel
/// visibility preference (memory-only; the AI panel's persisted visibility
/// keeps its own config key).
pub(crate) struct AgentTaskDomain {
    pub(crate) task_manager: TaskManager,
    pub(crate) agent_runtime: crate::agent_task::AgentRuntimeManager,
    pub(crate) agent_diff: crate::agent_task::AgentDiffPanel,
    pub(crate) selected_task: Option<TaskId>,
    pub(crate) pending_task_creation: Option<agent_task_ui::PendingTaskCreation>,
    /// Validation cwd pins retained between tab spawn and the spawn-resolved
    /// hook, keyed by the synthetic task session id so tab/pane rearrangement
    /// cannot mis-key them. The child enters the worktree through the
    /// validated descriptor.
    pub(crate) pending_validation_pins: HashMap<String, crate::agent_task::PreparedTaskValidation>,
    pub(crate) panel_visible: bool,
    /// The tick always re-arms through this flag so a burst of work cannot
    /// stack multiple timers.
    pub(crate) timer_armed: bool,
    /// Mints the per-terminal component of `forge-<pid>-<serial>` task
    /// session identities. Runtime-only like the terminals themselves.
    pub(crate) next_task_terminal_serial: u64,
}

impl AgentTaskDomain {
    pub(crate) fn new() -> Self {
        Self {
            task_manager: TaskManager::new(),
            agent_runtime: crate::agent_task::AgentRuntimeManager::new(),
            agent_diff: crate::agent_task::AgentDiffPanel::new(),
            selected_task: None,
            pending_task_creation: None,
            pending_validation_pins: HashMap::new(),
            panel_visible: false,
            timer_armed: false,
            next_task_terminal_serial: 0,
        }
    }
}

/// Build the semantic evidence for a new task from one block snapshot.
///
/// The synthetic execution id is panel-local provenance: it never crosses a
/// provider boundary, but keeps the task's evidence keyed to the exact block
/// it came from. Command/cwd exactness flags come from the block lifecycle,
/// so a screen scrape can never pose as the shell's own report.
fn semantic_context_from_evidence(
    leaf: &PaneLeaf,
    evidence: crate::block_view::BlockAgentEvidence,
    source_shell: Option<String>,
) -> Option<crate::agent_task::SemanticCommandContext> {
    let source_session_id = leaf
        .session_id()
        .filter(|session_id| jterm_core::execution_journal::is_valid_jsh_session_id(session_id))?;
    Some(crate::agent_task::SemanticCommandContext {
        source_session_id,
        source_execution_id: format!("block-{}", evidence.block_id),
        source_sequence: evidence.block_id,
        source_shell,
        command: evidence.command,
        command_exact: evidence.command_exact,
        command_truncated: evidence.command_truncated,
        cwd: evidence.cwd.clone(),
        cwd_after: evidence.cwd,
        exit_code: evidence.exit_code,
        duration_ms: evidence.duration_ms,
        output_text: evidence.output_text,
        output_available: evidence.output_available,
        output_truncated: evidence.output_truncated,
        output_total_bytes: evidence.output_total_bytes,
        started_at: evidence.started_at,
        finished_at: evidence.finished_at,
    })
}

impl UiState {
    fn task_toast(&self, message: impl AsRef<str>) {
        self.toast_overlay
            .add_toast(adw::Toast::new(message.as_ref()));
    }

    fn task_prompt_policy(&self) -> NativePromptPolicy {
        agent_task_ui::prompt_policy(&self.config.borrow())
    }

    fn active_pane_leaf(&self) -> Option<PaneLeaf> {
        let page = self
            .notebook
            .current_page()
            .and_then(|num| self.notebook.nth_page(Some(num)))?;
        let node = crate::ui::PaneNode::from_widget(&page)?;
        node.active_leaf()
    }

    fn next_task_terminal_serial(&self) -> u64 {
        let mut domain = self.agent_tasks.borrow_mut();
        domain.next_task_terminal_serial = domain.next_task_terminal_serial.saturating_add(1);
        domain.next_task_terminal_serial
    }

    pub(crate) fn toggle_tasks_panel(&self) {
        // Safe mode needs no separate guard: it loads safe defaults, which
        // keep `agent_tasks_enabled` false and land on the same opt-in toast.
        if !self.config.borrow().agent_tasks_enabled {
            self.task_toast(
                "Agent tasks are opt-in: set agent_tasks_enabled = true in the config file first",
            );
            return;
        }
        let visible = !self.agent_tasks.borrow().panel_visible;
        self.agent_tasks.borrow_mut().panel_visible = visible;
        self.sync_side_panel();
        if visible {
            self.sync_tasks_panel();
        }
        self.ensure_agent_tasks_timer();
    }

    /// Keep one self-re-arming poll timer alive while task machinery has
    /// anything to do. The tick drains only already-buffered state.
    pub(crate) fn ensure_agent_tasks_timer(&self) {
        self.ensure_agent_tasks_timer_with(TASKS_POLL_FAST);
    }

    fn ensure_agent_tasks_timer_with(&self, interval: Duration) {
        {
            let mut domain = self.agent_tasks.borrow_mut();
            if domain.timer_armed {
                return;
            }
            domain.timer_armed = true;
        }
        let ui = self.clone();
        glib::timeout_add_local_once(interval, move || {
            ui.agent_tasks_tick();
        });
    }

    /// One nonblocking drain of every task-facing worker: native runtime
    /// events, pending worktree creation, and the diff worker. Rearms the
    /// timer at the cadence current activity justifies.
    pub(crate) fn agent_tasks_tick(&self) {
        self.agent_tasks.borrow_mut().timer_armed = false;
        let mut keep_fast = false;

        if self.config.borrow().agent_tasks_enabled {
            let policy = self.task_prompt_policy();
            let report = {
                let mut domain = self.agent_tasks.borrow_mut();
                let AgentTaskDomain {
                    agent_runtime,
                    task_manager,
                    ..
                } = &mut *domain;
                agent_runtime.poll(task_manager, policy)
            };
            if let Some(issue) = report.issues.last() {
                self.task_toast(format!("Native Agent issue: {}", issue.detail));
            } else if let Some(completion) = report.completions.last() {
                let message = if report.completions.len() > 1 {
                    format!(
                        "{} native Codex sessions stopped; open Tasks for individual results",
                        report.completions.len()
                    )
                } else {
                    match completion.outcome {
                        AgentSessionOutcome::Clean => {
                            "Native Codex stopped cleanly; review its diff, then run validation"
                                .to_string()
                        }
                        AgentSessionOutcome::Cancelled => {
                            "Native Codex was cancelled and fully stopped".to_string()
                        }
                        AgentSessionOutcome::Failed => format!(
                            "Native Codex failed: {}",
                            completion
                                .detail
                                .as_deref()
                                .unwrap_or("provider session did not complete")
                        ),
                    }
                };
                self.task_toast(message);
            }
            keep_fast |=
                report.made_progress() || self.agent_tasks.borrow().agent_runtime.needs_fast_poll();

            let pending_outcome = {
                let domain = self.agent_tasks.borrow();
                domain
                    .pending_task_creation
                    .as_ref()
                    .map(|pending| pending.receiver.try_recv())
            };
            match pending_outcome {
                Some(Ok(Ok(prepared))) => {
                    self.agent_tasks.borrow_mut().pending_task_creation = None;
                    self.register_prepared_task(prepared);
                }
                Some(Ok(Err(error))) => {
                    self.agent_tasks.borrow_mut().pending_task_creation = None;
                    self.task_toast(format!("Could not create task worktree: {error}"));
                }
                Some(Err(TryRecvError::Empty)) => {
                    keep_fast = true;
                }
                Some(Err(TryRecvError::Disconnected)) => {
                    self.agent_tasks.borrow_mut().pending_task_creation = None;
                    self.task_toast("Task worktree worker stopped without a result");
                }
                None => {}
            }

            let (diff_progress, diff_loading) = {
                let mut domain = self.agent_tasks.borrow_mut();
                (domain.agent_diff.poll(), domain.agent_diff.state().loading)
            };
            keep_fast |= diff_progress || diff_loading;

            if self.agent_tasks.borrow().panel_visible {
                self.sync_tasks_panel();
            }
        }

        let (has_tasks, panel_visible) = {
            let domain = self.agent_tasks.borrow();
            (
                domain.task_manager.tasks().iter().next().is_some(),
                domain.panel_visible,
            )
        };
        if keep_fast || has_tasks || panel_visible {
            self.ensure_agent_tasks_timer_with(if keep_fast {
                TASKS_POLL_FAST
            } else {
                TASKS_POLL_SLOW
            });
        }
    }

    /// Stage a panel action against the live domain. Every branch resolves
    /// the task again; a stale panel snapshot can never retarget an action.
    pub(crate) fn execute_task_panel_action(&self, action: TaskPanelAction) {
        if !self.config.borrow().agent_tasks_enabled {
            self.task_toast("Agent tasks are disabled in the configuration");
            return;
        }
        match action {
            TaskPanelAction::CreateFromBlock => self.create_task_from_block(),
            TaskPanelAction::Select(task_id) => {
                self.agent_tasks.borrow_mut().selected_task = Some(task_id);
                self.sync_tasks_panel();
            }
            TaskPanelAction::Close => {
                self.agent_tasks.borrow_mut().panel_visible = false;
                self.sync_side_panel();
            }
            TaskPanelAction::StartCodex(task_id) => {
                let policy = self.task_prompt_policy();
                let result = {
                    let mut domain = self.agent_tasks.borrow_mut();
                    let AgentTaskDomain {
                        agent_runtime,
                        task_manager,
                        ..
                    } = &mut *domain;
                    agent_runtime.start_codex(task_manager, task_id, policy)
                };
                match result {
                    Ok(()) => {
                        self.task_toast("Preparing native Codex prerequisites in the background…")
                    }
                    Err(error) => self.task_toast(format!("Could not start native Codex: {error}")),
                }
            }
            TaskPanelAction::StartTerminal(task_id) => self.start_task_agent_terminal(task_id),
            TaskPanelAction::StopCodex(task_id) => {
                let result = {
                    let mut domain = self.agent_tasks.borrow_mut();
                    domain.agent_runtime.cancel(task_id)
                };
                match result {
                    Ok(()) => {
                        if self.agent_tasks.borrow().agent_runtime.has_running(task_id) {
                            self.task_toast("Stopping Codex and waiting for process cleanup…");
                        } else {
                            self.task_toast(
                                "Native Codex preparation cancelled; finishing background cleanup…",
                            );
                        }
                    }
                    Err(error) => self.task_toast(error.to_string()),
                }
            }
            TaskPanelAction::FollowUp(task_id, text) => {
                let policy = self.task_prompt_policy();
                let result = {
                    let mut domain = self.agent_tasks.borrow_mut();
                    let AgentTaskDomain {
                        agent_runtime,
                        task_manager,
                        ..
                    } = &mut *domain;
                    agent_runtime.prompt_codex(task_manager, task_id, &text, policy)
                };
                match result {
                    Ok(()) => self.task_toast("Follow-up queued on the existing Codex thread…"),
                    Err(error) => self.task_toast(error.to_string()),
                }
            }
            TaskPanelAction::FinishCodex(task_id) => {
                let result = {
                    let mut domain = self.agent_tasks.borrow_mut();
                    let AgentTaskDomain {
                        agent_runtime,
                        task_manager,
                        ..
                    } = &mut *domain;
                    agent_runtime.finish_codex(task_manager, task_id)
                };
                match result {
                    Ok(()) => self.task_toast(
                        "Finishing Codex and waiting for containment cleanup before validation…",
                    ),
                    Err(error) => self.task_toast(error.to_string()),
                }
            }
            TaskPanelAction::Approve(task_id, approval_id) => {
                self.decide_native_approval(task_id, approval_id, ApprovalDecision::Approve)
            }
            TaskPanelAction::Deny(task_id, approval_id) => self.decide_native_approval(
                task_id,
                approval_id,
                ApprovalDecision::Deny { reason: None },
            ),
            TaskPanelAction::RunValidation(task_id) => self.start_task_validation(task_id),
            TaskPanelAction::Complete(task_id) => {
                let result = {
                    let mut domain = self.agent_tasks.borrow_mut();
                    domain.task_manager.complete_after_validation(task_id)
                };
                match result {
                    Ok(()) => self.task_toast("Task marked complete after passing validation"),
                    Err(error) => self.task_toast(error.to_string()),
                }
            }
            TaskPanelAction::ReviewDiff(task_id) => {
                let task_review = {
                    let domain = self.agent_tasks.borrow();
                    domain
                        .task_manager
                        .get(task_id)
                        .map(|task| (task.worktree_path.clone(), task.base_commit.clone()))
                };
                let Some((worktree, base_commit)) = task_review else {
                    self.task_toast("Task is no longer available");
                    return;
                };
                let request_result = {
                    let mut domain = self.agent_tasks.borrow_mut();
                    domain.agent_diff.is_open = true;
                    domain.agent_diff.request_from(worktree, base_commit)
                };
                if let Err(error) = request_result {
                    self.task_toast(format!("Could not open task diff: {error}"));
                }
            }
            TaskPanelAction::Archive(task_id) => {
                let result = {
                    let mut domain = self.agent_tasks.borrow_mut();
                    let archived = domain.task_manager.archive(task_id);
                    if archived.is_ok() {
                        domain.agent_runtime.clear_retained(task_id);
                        if domain.selected_task == Some(task_id) {
                            domain.selected_task = None;
                        }
                    }
                    archived
                };
                match result {
                    Ok(()) => self.task_toast("Task hidden; worktree left in place"),
                    Err(error) => self.task_toast(error.to_string()),
                }
            }
        }
        self.ensure_agent_tasks_timer();
        if self.agent_tasks.borrow().panel_visible {
            self.sync_tasks_panel();
        }
    }

    fn decide_native_approval(
        &self,
        task_id: TaskId,
        approval_id: crate::agent_task::ApprovalId,
        decision: ApprovalDecision,
    ) {
        let label = if matches!(&decision, ApprovalDecision::Approve) {
            "Approval sent to Codex"
        } else {
            "Denial sent to Codex"
        };
        let result = {
            let mut domain = self.agent_tasks.borrow_mut();
            domain
                .agent_runtime
                .decide_approval(task_id, approval_id, decision)
        };
        match result {
            Ok(()) => self.task_toast(label),
            Err(error) => self.task_toast(error.to_string()),
        }
    }

    /// Begin task creation from the active pane's selected block. The block
    /// preflight is the exact one ember shares between its block menu and
    /// panel: exact shell-reported command and cwd are mandatory, and the
    /// sharing consent gates nothing here because no provider is contacted
    /// until Start Codex.
    fn create_task_from_block(&self) {
        if self.agent_tasks.borrow().pending_task_creation.is_some() {
            self.task_toast("Another task worktree is still being created");
            return;
        }
        let Some(leaf) = self.active_pane_leaf() else {
            self.task_toast("No active terminal pane");
            return;
        };
        let Some(view) = leaf.block_view() else {
            self.task_toast("Select a finished block in a Block-mode pane to create an agent task");
            return;
        };
        let Some(evidence) = view.selected_block_agent_evidence(80) else {
            self.task_toast("Select a finished block in a Block-mode pane to create an agent task");
            return;
        };
        if let Some(reason) = crate::agent_task::context::block_agent_context_disabled_reason(
            evidence.command.as_deref(),
            evidence.command_exact,
            evidence.command_truncated,
            evidence.cwd.as_deref(),
            Some(evidence.output_available),
        ) {
            self.task_toast(format!("Cannot create an agent task: {reason}"));
            return;
        }
        let source_shell = self.shell_argv.borrow().first().cloned();
        let Some(context) = semantic_context_from_evidence(&leaf, evidence, source_shell) else {
            self.task_toast("Cannot create an agent task: the pane has no verified shell session");
            return;
        };
        match agent_task_ui::begin_worktree_creation(context, AgentProvider::Codex) {
            Ok(pending) => {
                self.agent_tasks.borrow_mut().pending_task_creation = Some(pending);
                self.task_toast("Creating the isolated task worktree in the background…");
                self.ensure_agent_tasks_timer();
                if self.agent_tasks.borrow().panel_visible {
                    self.sync_tasks_panel();
                }
            }
            Err(error) => self.task_toast(format!("Could not create task: {error}")),
        }
    }

    fn register_prepared_task(&self, prepared: agent_task_ui::PreparedTask) {
        let new_task = NewTask {
            title: prepared.title,
            provider: prepared.provider,
            repo_root: prepared.worktree.repository.clone(),
            worktree_path: prepared.worktree.path.clone(),
            branch: prepared.worktree.branch.clone(),
            base_commit: prepared.worktree.head.clone(),
            source_context: Some(prepared.context),
        };
        let result = {
            let mut domain = self.agent_tasks.borrow_mut();
            domain.task_manager.create(new_task)
        };
        match result {
            Ok(task_id) => {
                self.agent_tasks.borrow_mut().selected_task = Some(task_id);
                self.task_toast("Task created in an isolated worktree; start Codex when ready");
            }
            Err(error) => {
                self.task_toast(format!("Could not register task: {error}"));
            }
        }
        self.ensure_agent_tasks_timer();
        if self.agent_tasks.borrow().panel_visible {
            self.sync_tasks_panel();
        }
    }

    /// Open the provider CLI in an ordinary PTY inside the task worktree.
    /// This is the compatibility path: no native events, no approval cards;
    /// containment is the worktree plus the exact audited launcher argv.
    fn start_task_agent_terminal(&self, task_id: TaskId) {
        let (provider, title, repository, worktree, failed_terminal_retry, native_recovery) = {
            let domain = self.agent_tasks.borrow();
            if domain.agent_runtime.has_preparing(task_id) {
                drop(domain);
                self.task_toast("Cancel native Codex preparation before starting a terminal");
                return;
            }
            let failed_terminal_retry = domain
                .task_manager
                .terminal_retry_session_id(task_id)
                .ok()
                .map(str::to_owned);
            let native_recovery = failed_terminal_retry.is_none()
                && domain.agent_runtime.can_continue_in_terminal(task_id)
                && domain
                    .task_manager
                    .native_terminal_fallback_eligible(task_id)
                    .is_ok();
            let launch = domain.task_manager.get(task_id).and_then(|task| {
                ((task.status == TaskStatus::Created && task.terminal_session_id.is_none())
                    || (native_recovery && task.terminal_session_id.is_none())
                    || failed_terminal_retry
                        .as_deref()
                        .is_some_and(|old| task.terminal_session_id.as_deref() == Some(old)))
                .then(|| {
                    (
                        task.provider,
                        task.title.clone(),
                        task.repo_root.clone(),
                        task.worktree_path.clone(),
                    )
                })
            });
            let Some((provider, title, repository, worktree)) = launch else {
                drop(domain);
                self.task_toast("Task is no longer waiting for an Agent terminal");
                return;
            };
            (
                provider,
                title,
                repository,
                worktree,
                failed_terminal_retry,
                native_recovery,
            )
        };
        let launch =
            match crate::agent_task::AgentLaunchSpec::resolve(provider, &repository, &worktree) {
                Ok(launch) => launch,
                Err(error) => {
                    if failed_terminal_retry.is_none() && !native_recovery {
                        // update_status preserves TerminalFallback provenance, so
                        // a failed compatibility launch remains terminal-only.
                        let _ = self.agent_tasks.borrow_mut().task_manager.update_status(
                            task_id,
                            TaskStatus::Created,
                            Some(error.to_string()),
                        );
                    }
                    self.task_toast(error.to_string());
                    return;
                }
            };
        if failed_terminal_retry.is_none() && !native_recovery {
            let _ = self.agent_tasks.borrow_mut().task_manager.update_status(
                task_id,
                TaskStatus::Starting,
                None,
            );
        }

        let session_name = format!(
            "{} · {}",
            provider.display_name(),
            crate::review_text::visible_bounded(&title, 96)
        );
        let serial = self.next_task_terminal_serial();
        let session_id = agent_task_ui::terminal_session_id(serial);
        let Some(leaf) = self.add_task_terminal_tab(
            &session_name,
            launch.argv,
            Some(worktree.to_string_lossy().into_owned()),
            Vec::new(),
            TaskTerminalRole::Agent,
            None,
        ) else {
            if failed_terminal_retry.is_none() && !native_recovery {
                let _ = self.agent_tasks.borrow_mut().task_manager.update_status(
                    task_id,
                    TaskStatus::Created,
                    Some("the task terminal pane could not be resolved".to_string()),
                );
            }
            return;
        };
        leaf.set_task_session_id(&session_id);

        let binding = {
            let mut domain = self.agent_tasks.borrow_mut();
            if let Some(old_session) = failed_terminal_retry.as_deref() {
                domain.task_manager.bind_terminal_retry_session(
                    task_id,
                    old_session,
                    session_id.clone(),
                )
            } else if native_recovery {
                domain
                    .task_manager
                    .bind_native_terminal_fallback_session(task_id, session_id.clone())
            } else {
                domain
                    .task_manager
                    .bind_terminal_session(task_id, session_id.clone())
            }
        };
        if let Err(error) = binding {
            // The pane never gained task authority; close it so a stray shell
            // cannot linger inside the task worktree.
            leaf.kill();
            if failed_terminal_retry.is_none() && !native_recovery {
                let _ = self.agent_tasks.borrow_mut().task_manager.update_status(
                    task_id,
                    TaskStatus::Created,
                    Some(error.to_string()),
                );
            }
            self.task_toast(error.to_string());
            return;
        }

        if native_recovery {
            self.agent_tasks
                .borrow_mut()
                .agent_runtime
                .clear_retained(task_id);
        }
        // No explicit persist: the Notebook page-added signal already queues
        // the snapshot, and the pruning side drops this tab from it.
        self.task_toast(format!(
            "Opened {} in an isolated task terminal; task context remains in Forge",
            provider.display_name()
        ));
    }

    /// Rerun the task's exact validation command in a fresh PTY inside the
    /// pinned worktree cwd. The prepared pin is retained until the spawn
    /// resolves, so the child enters the directory through the validated
    /// descriptor rather than a re-resolved path.
    fn start_task_validation(&self, task_id: TaskId) {
        let next_attempt = {
            let domain = self.agent_tasks.borrow();
            match domain.task_manager.next_validation_attempt(task_id) {
                Ok(attempt) => attempt,
                Err(error) => {
                    drop(domain);
                    self.task_toast(error.to_string());
                    return;
                }
            }
        };
        let prepared = {
            let domain = self.agent_tasks.borrow();
            let Some(task) = domain.task_manager.get(task_id) else {
                drop(domain);
                self.task_toast("Task is no longer available");
                return;
            };
            match crate::agent_task::prepare_task_validation(task) {
                Ok(prepared) => prepared,
                Err(error) => {
                    drop(domain);
                    self.task_toast(format!("Could not prepare validation: {error}"));
                    return;
                }
            }
        };
        let argv = match agent_task_ui::validation_command_argv(
            Some(prepared.source_shell.as_str()),
            &prepared.command,
        ) {
            Ok(argv) => argv,
            Err(error) => {
                self.task_toast(format!("Could not resolve validation shell: {error}"));
                return;
            }
        };
        let task_title = {
            let domain = self.agent_tasks.borrow();
            match domain.task_manager.get(task_id) {
                Some(task) => task.title.clone(),
                None => {
                    drop(domain);
                    self.task_toast("Task is no longer available");
                    return;
                }
            }
        };
        let session_name = format!(
            "Validate #{} · {}",
            next_attempt,
            crate::review_text::visible_bounded(&task_title, 88)
        );
        let pinned_path = prepared.pinned_cwd.proc_path();
        let env_extra: Vec<(String, String)> = agent_task_ui::VALIDATION_ENV_OVERRIDES
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        // The synthetic identity is minted before the tab so the
        // spawn-resolved hook can release exactly this terminal's pin.
        let serial = self.next_task_terminal_serial();
        let session_id = agent_task_ui::terminal_session_id(serial);
        let pin_session = session_id.clone();
        let domain_for_launch = self.agent_tasks.clone();
        let on_launched: Box<dyn FnOnce()> = Box::new(move || {
            domain_for_launch
                .borrow_mut()
                .pending_validation_pins
                .remove(&pin_session);
        });
        let Some(leaf) = self.add_task_terminal_tab(
            &session_name,
            argv,
            Some(pinned_path.to_string_lossy().into_owned()),
            env_extra,
            TaskTerminalRole::Validation,
            Some(on_launched),
        ) else {
            return;
        };
        leaf.set_task_session_id(&session_id);

        let binding = {
            let mut domain = self.agent_tasks.borrow_mut();
            domain
                .task_manager
                .bind_validation_session(task_id, session_id.clone())
        };
        if let Err(error) = binding {
            leaf.kill();
            self.task_toast(error.to_string());
            return;
        }
        // Hold the validated descriptor open until the spawn resolves (or the
        // pane dies trying); see the spawn hook and the task terminal exit
        // paths.
        self.agent_tasks
            .borrow_mut()
            .pending_validation_pins
            .insert(session_id, prepared);
        self.task_toast(format!(
            "Validation #{next_attempt} is running in the isolated task worktree"
        ));
    }

    /// A pane process exited and its leaf carried a task marker. Apply the
    /// authoritative exit to the task model before the pane collapses.
    pub(crate) fn note_task_terminal_exited(&self, leaf: &PaneLeaf, exit_code: i32) {
        let Some((session_id, role)) = leaf.task_session_id().zip(leaf.task_role()) else {
            return;
        };
        let (outcome, panel_visible) = {
            let mut domain = self.agent_tasks.borrow_mut();
            domain.pending_validation_pins.remove(&session_id);
            domain
                .task_manager
                .handle_terminal_session_exit(&session_id, Some(exit_code));
            let outcome = domain
                .task_manager
                .handle_terminal_session_closed(&session_id)
                .and_then(|task_id| {
                    (role == TaskTerminalRole::Validation)
                        .then(|| {
                            domain.task_manager.get(task_id).map(|task| {
                                (
                                    task.validation.status,
                                    task.validation.status_detail.clone(),
                                )
                            })
                        })
                        .flatten()
                });
            (outcome, domain.panel_visible)
        };
        if let Some((status, detail)) = outcome {
            let message = match status {
                TaskValidationStatus::Passed => "Validation passed".to_string(),
                TaskValidationStatus::Failed => {
                    format!("Validation failed (exit {exit_code})")
                }
                _ => detail.unwrap_or_else(|| "Validation ended".to_string()),
            };
            self.task_toast(message);
        }
        if panel_visible {
            self.sync_tasks_panel();
        }
    }

    /// A task-terminal pane is being removed without a process-exit signal
    /// (the user closed its tab). This is the close half of ember's
    /// exit/closed split: a validation still marked Running becomes Cancelled
    /// rather than silently retaining a stale Running state, and any retained
    /// cwd pin is released. Safe to call after `note_task_terminal_exited`:
    /// the reducer ignores closes for terminals already in a terminal state.
    pub(crate) fn note_task_terminal_closed(&self, leaf: &PaneLeaf) {
        let Some(session_id) = leaf.task_session_id() else {
            return;
        };
        let (closed, panel_visible) = {
            let mut domain = self.agent_tasks.borrow_mut();
            domain.pending_validation_pins.remove(&session_id);
            let closed = domain
                .task_manager
                .handle_terminal_session_closed(&session_id)
                .is_some();
            (closed, domain.panel_visible)
        };
        if closed && panel_visible {
            self.sync_tasks_panel();
        }
    }

    /// Push the composed panel state. Rows are rebuilt from the domain every
    /// time; the panel never holds its own task copies.
    pub(crate) fn sync_tasks_panel(&self) {
        let policy = self.task_prompt_policy();
        let native_ai_enabled = policy.share_command_context;
        let domain = self.agent_tasks.borrow();
        let mut rows: Vec<TaskRowSnapshot> = domain
            .task_manager
            .tasks()
            .iter()
            .map(|task| TaskRowSnapshot {
                id: task.id,
                title: task.title.clone(),
                provider: task.provider,
                status: task.status,
                runtime_kind: task.runtime_kind,
                branch: task.branch.clone(),
                has_agent_terminal: task.terminal_session_id.is_some(),
                has_validation_terminal: task.validation.terminal_session_id.is_some(),
                has_active_agent_stream: domain.task_manager.has_active_agent_event_stream(task.id),
                native_preparing: domain.agent_runtime.has_preparing(task.id),
                validation_status: task.validation.status,
                validation_attempt: task.validation.attempt,
                needs_attention: task.needs_attention(),
                status_detail: task.status_detail.clone(),
            })
            .collect();
        let updated: HashMap<TaskId, u64> = domain
            .task_manager
            .tasks()
            .iter()
            .map(|task| (task.id, task.updated_at_ms))
            .collect();
        order_task_rows(&mut rows, |id| updated.get(&id).copied().unwrap_or(0));

        let selected = domain
            .selected_task
            .filter(|id| domain.task_manager.get(*id).is_some());
        let detail = selected.and_then(|id| task_detail_sync(&domain, id, native_ai_enabled));

        let create_hint = if domain.pending_task_creation.is_some() {
            "Creating the isolated task worktree…".to_string()
        } else if !native_ai_enabled {
            "Start Codex needs AI enabled plus command-context sharing consent (config: ai_enabled, ai_share_command_context)"
                .to_string()
        } else {
            String::new()
        };
        let sync = TasksPanelSync {
            rows,
            selected,
            detail: detail.map(Box::new),
            create_enabled: true,
            create_hint,
            pending_creation: domain.pending_task_creation.is_some(),
        };
        drop(domain);
        self.tasks_panel.sync(sync);
    }
}

/// Everything the detail pane renders for the selected task, resolved once
/// against the live domain so the view never reaches into it.
fn task_detail_sync(
    domain: &AgentTaskDomain,
    task_id: TaskId,
    native_ai_enabled: bool,
) -> Option<TaskDetailSync> {
    let task = domain.task_manager.get(task_id)?;
    let view = domain.agent_runtime.snapshot(task_id);
    let has_stream = domain.task_manager.has_active_agent_event_stream(task_id);
    let native_idle = task.status == TaskStatus::ReadyForReview
        && has_stream
        && view
            .as_ref()
            .is_some_and(|view| view.phase == CodexAppServerPhase::Ready);
    let native_preparing = domain.agent_runtime.has_preparing(task_id);
    let terminal_retry_available = domain
        .task_manager
        .terminal_retry_session_id(task_id)
        .is_ok();
    let native_terminal_fallback_available = domain.agent_runtime.can_continue_in_terminal(task_id)
        && domain
            .task_manager
            .native_terminal_fallback_eligible(task_id)
            .is_ok();
    let mut status_line = row_status_line(&TaskRowSnapshot {
        id: task.id,
        title: task.title.clone(),
        provider: task.provider,
        status: task.status,
        runtime_kind: task.runtime_kind,
        branch: task.branch.clone(),
        has_agent_terminal: task.terminal_session_id.is_some(),
        has_validation_terminal: task.validation.terminal_session_id.is_some(),
        has_active_agent_stream: has_stream,
        native_preparing,
        validation_status: task.validation.status,
        validation_attempt: task.validation.attempt,
        needs_attention: task.needs_attention(),
        status_detail: task.status_detail.clone(),
    });
    if let Some(detail) = &task.status_detail {
        status_line = format!("{status_line} · {detail}");
    }
    if task.validation.attempt > 0 {
        status_line = format!(
            "{status_line} · validation #{} {}",
            task.validation.attempt,
            task.validation.status.label()
        );
        if let Some(detail) = &task.validation.status_detail {
            status_line = format!("{status_line} · {detail}");
        }
    }

    let can_start_terminal = (task.status == TaskStatus::Created
        && matches!(task.runtime_kind, TaskRuntimeKind::Unassigned)
        && !native_preparing)
        || (task.status == TaskStatus::Created
            && matches!(
                task.runtime_kind,
                TaskRuntimeKind::Terminal | TaskRuntimeKind::TerminalFallback
            ))
        || native_terminal_fallback_available
        || (task.status == TaskStatus::Failed && terminal_retry_available);
    let follow_up_hint = if native_idle && !native_ai_enabled {
        "Enable AI features and command-context sharing before sending another turn".to_string()
    } else if native_idle {
        "Send another turn on this loaded Codex thread, or finish the session to unlock validation"
            .to_string()
    } else {
        String::new()
    };
    let diff = domain
        .agent_diff
        .requested_cwd()
        .filter(|requested| *requested == task.worktree_path)
        .map(|_| DiffSync {
            header: format!(
                "git diff {} · {}",
                domain.agent_diff.requested_base().unwrap_or("HEAD"),
                crate::agent_task::diff::visible_diff_cwd(&task.worktree_path)
            ),
            scope: if domain.agent_diff.requested_base() == Some("HEAD")
                || domain.agent_diff.requested_base().is_none()
            {
                "Current working tree; this view can include changes that predate the Agent task."
                    .to_string()
            } else {
                "Compared with the immutable task baseline; includes Agent commits plus current working-tree changes."
                    .to_string()
            },
            loading: domain.agent_diff.state().loading,
            error: domain.agent_diff.state().error.clone(),
            truncated: domain.agent_diff.state().truncated,
            text: domain.agent_diff.state().text.clone(),
        });

    Some(TaskDetailSync {
        id: task.id,
        title: task.title.clone(),
        status_line,
        branch: task.branch.clone(),
        stream: view.map(Box::new),
        approvals: domain
            .agent_runtime
            .snapshot(task_id)
            .map(|snapshot| snapshot.pending_approvals.clone())
            .unwrap_or_default(),
        completed_turns: domain
            .agent_runtime
            .snapshot(task_id)
            .map_or(0, |snapshot| snapshot.completed_turns),
        can_start_codex: task.status == TaskStatus::Created
            && task.runtime_kind == TaskRuntimeKind::Unassigned
            && !native_preparing
            && native_ai_enabled,
        can_start_terminal,
        can_stop: (native_preparing || has_stream) && !native_idle,
        can_finish: native_idle,
        can_run_validation: task.status == TaskStatus::ReadyForReview
            && task.validation.status != TaskValidationStatus::Running
            && !has_stream,
        can_complete: task.status == TaskStatus::ReadyForReview
            && task.validation.status == TaskValidationStatus::Passed,
        can_follow_up: native_idle && native_ai_enabled,
        follow_up_hint,
        diff,
    })
}
