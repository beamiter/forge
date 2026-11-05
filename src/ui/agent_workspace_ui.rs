//! Native workspace routing. Browsing never sends input into a hidden PTY.
use super::workspace_navigation::{Action as WorkspaceAction, Row};
use super::*;
use gtk4::prelude::{CastNone, WidgetExt};

impl UiState {
    pub(crate) fn show_terminal_workspace(&self) {
        self.agent_tasks.borrow_mut().panel_visible = false;
        self.ai_panel_visible.set(false);
        self.workspace_stack.set_visible_child_name("terminal");
    }

    pub(crate) fn workspace_action(&self, action: WorkspaceAction) {
        match action {
            WorkspaceAction::Cli(id) => {
                if self.session_sidebar.contains(&id) {
                    self.session_sidebar.activate(&id);
                } else {
                    self.workspace_action(WorkspaceAction::Inspect(id));
                    self.task_toast("The saved CLI source is not available in the current local catalog. Its indexed evidence remains read only; refresh Sessions to rediscover the source.");
                }
            }
            WorkspaceAction::PinCli(id, pinned) => {
                self.session_sidebar.organize(&id, Some(pinned), None)
            }
            WorkspaceAction::ArchiveCli(id, archived) => {
                self.session_sidebar.organize(&id, None, Some(archived))
            }
            WorkspaceAction::Api(id) => {
                self.open_ai_panel();
                self.ai_panel.select_chat(id);
                if self.ai_panel_visible.get() {
                    super::reference_shell::navigation_entered_canvas(self);
                    self.ai_panel.focus_input();
                }
            }
            WorkspaceAction::SharedApi(id) => {
                self.open_ai_panel();
                self.workspace_stack.set_visible_child_name("chats");
                self.ai_panel.open_shared(id);
                if self.ai_panel_visible.get() {
                    super::reference_shell::navigation_entered_canvas(self);
                    self.ai_panel.focus_input();
                }
            }
            WorkspaceAction::NewApi => {
                if self.config.borrow().ai_enabled {
                    self.open_ai_panel();
                    self.ai_panel.create_new_chat();
                    super::reference_shell::navigation_entered_canvas(self);
                    self.ai_panel.focus_input();
                } else {
                    self.show_ai_error("Enable AI in Settings to create an API conversation.");
                }
            }
            WorkspaceAction::PrefillApi { body } => {
                if self.config.borrow().ai_enabled {
                    self.open_ai_panel();
                    self.ai_panel.create_draft_chat(body);
                    super::reference_shell::navigation_entered_canvas(self);
                    self.ai_panel.focus_input();
                } else {
                    self.show_ai_error("Enable AI in Settings to create an API handoff draft.");
                }
            }
            WorkspaceAction::PrefillTask {
                title,
                cwd,
                provider,
                body,
            } => {
                self.compose_handoff_task(title, cwd, provider, body);
            }
            WorkspaceAction::OpenTask(conversation) => {
                if let Some(row) = self.workspace_navigation.lookup(&conversation) {
                    if let Some(id) = row
                        .facts
                        .task_id
                        .filter(|id| self.agent_tasks.borrow().task_manager.get(*id).is_some())
                    {
                        self.workspace_action(WorkspaceAction::Task(id, conversation));
                    } else {
                        self.workspace_action(WorkspaceAction::Inspect(conversation));
                        self.task_toast("Task controls will appear after the host's task state has synchronized.");
                    }
                }
            }
            WorkspaceAction::Task(id, conversation) => {
                self.workspace_navigation
                    .mission_link_target(conversation.clone());
                let terminal = self
                    .agent_tasks
                    .borrow()
                    .agent_runtime
                    .hosted_terminal(id)
                    .cloned();
                if let Some(session) = terminal {
                    self.open_hosted_native_terminal(session);
                    return;
                }

                let status = self
                    .agent_tasks
                    .borrow()
                    .task_manager
                    .get(id)
                    .map(|task| task.status);
                if let Some(status) = status {
                    self.tasks_panel
                        .reveal_workspace_task(status == crate::agent_task::TaskStatus::Archived);
                    self.agent_tasks.borrow_mut().panel_visible = true;
                    self.sync_side_panel();
                    self.execute_task_panel_action(crate::agent_task_ui::TaskPanelAction::Select(
                        id,
                    ));
                    super::reference_shell::navigation_entered_canvas(self);
                } else {
                    self.workspace_action(WorkspaceAction::Inspect(conversation));
                }
            }
            WorkspaceAction::PinShared(id, pinned) => self
                .ai_panel
                .shared_mutation(jterm_core::agent_workspace::WorkspaceCommand::Pin { id, pinned }),
            WorkspaceAction::ArchiveShared(id, archived) => self.ai_panel.shared_mutation(
                jterm_core::agent_workspace::WorkspaceCommand::Archive { id, archived },
            ),
            WorkspaceAction::ExternalTask(id) | WorkspaceAction::Inspect(id) => {
                if let Some(row) = self
                    .workspace_navigation
                    .lookup(&id)
                    .or_else(|| self.session_sidebar.shared_row(&id))
                {
                    if row.kind == jterm_core::agent_workspace::ConversationKind::Task {
                        self.workspace_navigation.mission_link_target(id.clone());
                    }
                    if let Some(old) = self.workspace_stack.child_by_name("task-context") {
                        self.workspace_stack.remove(&old);
                    }
                    let page = self.workspace_navigation.inspect(&row);
                    self.workspace_stack.add_named(&page, Some("task-context"));
                    self.workspace_stack.set_visible_child_name("task-context");
                    super::reference_shell::navigation_entered_canvas(self);
                }
            }
            WorkspaceAction::InspectContext(id) => {
                if let Some(row) = self
                    .workspace_navigation
                    .lookup(&id)
                    .or_else(|| self.session_sidebar.shared_row(&id))
                {
                    if let Some(old) = self.workspace_stack.child_by_name("task-context") {
                        self.workspace_stack.remove(&old);
                    }
                    let page = self.workspace_navigation.inspect_context(&row);
                    self.workspace_stack.add_named(&page, Some("task-context"));
                    self.workspace_stack.set_visible_child_name("task-context");
                    super::reference_shell::navigation_entered_canvas(self);
                    page.grab_focus();
                }
            }
            WorkspaceAction::ShowEvidence(id) => {
                if let Some(page) = self.workspace_navigation.evidence(id) {
                    if let Some(old) = self.workspace_stack.child_by_name("workspace-evidence") {
                        self.workspace_stack.remove(&old);
                    }
                    self.workspace_stack
                        .add_named(&page, Some("workspace-evidence"));
                    self.workspace_stack
                        .set_visible_child_name("workspace-evidence");
                    super::reference_shell::navigation_entered_canvas(self);
                    page.grab_focus();
                }
            }
            WorkspaceAction::AddEvidenceContext(id) => {
                if let Some(page) = self.workspace_navigation.evidence_context(id) {
                    if let Some(old) = self.workspace_stack.child_by_name("task-context") {
                        self.workspace_stack.remove(&old);
                    }
                    self.workspace_stack.add_named(&page, Some("task-context"));
                    self.workspace_stack.set_visible_child_name("task-context");
                    super::reference_shell::navigation_entered_canvas(self);
                    page.grab_focus();
                }
            }
            WorkspaceAction::NewTask => {
                self.execute_task_panel_action(
                    crate::agent_task_ui::TaskPanelAction::ComposeProject,
                );
            }
            WorkspaceAction::NewTerminal => {
                self.show_terminal_workspace();
                self.execute_action(crate::keybindings::Action::NewTab);
            }
            WorkspaceAction::Missions | WorkspaceAction::Mission(_) => {
                let id = if let WorkspaceAction::Mission(id) = &action {
                    Some(id.as_str())
                } else {
                    None
                };
                if let Some(old) = self.workspace_stack.child_by_name("workspace-missions") {
                    self.workspace_stack.remove(&old);
                }
                let page = self.workspace_navigation.mission_page(id);
                if let Some(parent) = page.parent().and_downcast::<gtk4::ScrolledWindow>() {
                    parent.set_child(None::<&gtk4::Widget>);
                }
                let scroll = gtk4::ScrolledWindow::new();
                scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
                scroll.set_child(Some(&page));
                self.workspace_stack
                    .add_named(&scroll, Some("workspace-missions"));
                self.workspace_stack
                    .set_visible_child_name("workspace-missions");
                super::reference_shell::navigation_entered_canvas(self);
            }
            WorkspaceAction::RemoteWorkspace => super::workspace_remote_ui::show(&self.window),
        }
        self.sync_workspace_navigation();
    }

    pub(crate) fn open_hosted_native_terminal(
        &self,
        session: jterm_core::agent_sessions::AgentSession,
    ) {
        match jterm_core::agent_workspace::managed_launch(&session)
            .and_then(|launch| Ok((jterm_core::agent_workspace::session_id(&session)?, launch)))
        {
            Ok((runtime_key, launch)) => {
                let (completion, _) = std::sync::mpsc::channel();
                self.add_agent_session_tab(super::agent_sessions::ResumeRequest {
                    runtime_key,
                    provider: session.provider,
                    id: session.id,
                    title: session.title,
                    launch: Some(launch),
                    completion,
                });
            }
            Err(error) => self.task_toast(error),
        }
    }

    pub(crate) fn sync_workspace_navigation(&self) {
        self.ai_panel.poll_workspace();
        self.session_sidebar
            .set_existing(self.agent_session_tabs.borrow().keys().cloned().collect());
        let surface = self.workspace_stack.visible_child_name();
        self.session_sidebar
            .set_terminal_active(surface.as_deref() == Some("terminal"));
        let mut rows = self
            .ai_panel
            .workspace_rows(surface.as_deref() == Some("chats"));
        rows.extend(self.session_sidebar.cli_rows().into_iter().map(|mut row| {
            row.active &= surface.as_deref() == Some("terminal");
            row
        }));
        let domain = self.agent_tasks.borrow();
        let namespace = format!(
            "forge:{}",
            crate::config::config_file_path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "default".into())
        );
        let mut owned = std::collections::HashSet::new();
        for task in domain.task_manager.tasks() {
            let Some(metadata) = domain
                .agent_runtime
                .conversation_id(task.id)
                .cloned()
                .or_else(|| {
                    self.session_sidebar
                        .task_id(&namespace, &task.id.to_string())
                })
            else {
                continue;
            };
            owned.insert(metadata.clone());
            self.workspace_navigation.set_task_usage(
                metadata.clone(),
                domain
                    .agent_runtime
                    .shared_snapshot(task.id)
                    .as_deref()
                    .and_then(|snapshot| snapshot.usage.as_ref())
                    .map(|usage| usage.summary()),
            );
            let shared = self.session_sidebar.shared_row(&metadata);
            let activity = jterm_core::agent_task::DashboardActivity {
                has_agent_terminal: domain.agent_runtime.hosted_terminal(task.id).is_some(),
                has_validation_terminal: domain
                    .agent_runtime
                    .hosted_validation_terminal(task.id)
                    .is_some(),
                has_active_agent_stream: domain.task_manager.has_active_agent_event_stream(task.id),
                native_preparing: domain.agent_runtime.has_preparing(task.id),
            };
            let mut projection = domain
                .agent_runtime
                .workspace_row(task.id)
                .unwrap_or_else(|| jterm_core::agent_workspace::WorkspaceRow {
                    id: metadata.clone(),
                    kind: jterm_core::agent_workspace::ConversationKind::Task,
                    title: task.title.clone(),
                    provider: task.provider.display_name().into(),
                    cwd: task.repo_root.to_string_lossy().into_owned(),
                    preview: format!("{} · {}", task.status.label(), task.branch),
                    updated_at_ms: task.updated_at_ms,
                    archived: task.status == crate::agent_task::TaskStatus::Archived,
                    pinned: false,
                    tags: Vec::new(),
                    runtime: if activity.native_preparing {
                        jterm_core::agent_workspace::RuntimeState::Starting
                    } else if activity.has_agent_terminal
                        || activity.has_validation_terminal
                        || activity.has_active_agent_stream
                    {
                        jterm_core::agent_workspace::RuntimeState::Running
                    } else {
                        jterm_core::agent_workspace::RuntimeState::Idle
                    },
                    facts: jterm_core::agent_workspace::WorkspaceFacts::from_task(task, activity),
                });
            if let Some(shared) = &shared {
                projection.pinned = shared.pinned;
                projection.tags.clone_from(&shared.tags);
            }
            let presentation = projection.presentation();
            rows.push(Row {
                action: WorkspaceAction::Task(task.id, metadata),
                title: projection.title.clone(),
                detail: format!(
                    "{} · {}",
                    task.provider.display_name(),
                    presentation.status_label
                ),
                searchable: format!(
                    "{} {} {}",
                    task.repo_root.display(),
                    task.branch,
                    presentation.detail.as_deref().unwrap_or_default()
                ),
                active: domain.selected_task == Some(task.id)
                    && surface.as_deref() == Some("tasks"),
                busy: projection.runtime.is_live(),
                attention: presentation.attention.is_some(),
                archived: projection.archived,
                pinned: projection.pinned,
                project: projection.cwd.clone(),
                provider: projection.provider.clone(),
                updated: projection.updated_at_ms,
                source: Some(projection),
            });
        }
        rows.extend(self.session_sidebar.external_tasks(&owned));
        self.session_sidebar.set_projects(
            rows.iter()
                .filter(|row| !row.project.is_empty())
                .map(|row| std::path::PathBuf::from(&row.project))
                .collect(),
        );
        self.workspace_navigation
            .sync(rows, &self.session_sidebar.filter());
    }
}
