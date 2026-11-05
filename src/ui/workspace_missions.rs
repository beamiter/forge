//! Native delivery plans over the shared host-owned Mission controller.
//! Browsing and inspection never start tasks, validation, Git writes or a provider.
use super::workspace_navigation::{Action, Row};
use gtk::prelude::*;
use gtk4 as gtk;
use jterm_core::agent_workspace::{
    ConversationId, MissionWorkbench, WorkspaceMission, WorkspaceMissionReport,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone)]
pub(crate) struct MissionSurface(Rc<MissionWidgets>);

pub(crate) struct MissionWidgets {
    pub root: gtk::Box,
    state: Rc<RefCell<MissionWorkbench>>,
    catalog: gtk::Box,
    catalog_rows: Rc<RefCell<HashMap<String, gtk::Button>>>,
    editor: gtk::TextBuffer,
    editor_view: gtk::TextView,
    status: gtk::Label,
    evidence: gtk::Label,
    task_links: gtk::Box,
    rendered_links: RefCell<Option<(String, u64)>>,
    callback: Rc<dyn Fn(Action)>,
    save: gtk::Button,
    inspect: gtk::Button,
    adopt: gtk::Button,
    link: gtk::Button,
    linked_task: Rc<RefCell<Option<ConversationId>>>,
    dirty: Rc<Cell<bool>>,
    submitted: RefCell<Option<String>>,
    rendering: Rc<Cell<bool>>,
    loaded: Rc<RefCell<Option<(String, u64)>>>,
}

impl std::ops::Deref for MissionSurface {
    type Target = MissionWidgets;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn label(text: &str) -> gtk::Label {
    let widget = gtk::Label::new(Some(text));
    widget.set_xalign(0.0);
    widget.set_wrap(true);
    widget.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    widget
}

fn editor_plan(text: &str, selected: &WorkspaceMission) -> Result<WorkspaceMission, String> {
    let mission: WorkspaceMission =
        serde_json::from_str(text).map_err(|error| error.to_string())?;
    if mission.id != selected.id || mission.revision != selected.revision {
        return Err("The plan identity or revision changed. Reload before editing; a draft cannot replace another Mission.".into());
    }
    mission.validate()?;
    Ok(mission)
}

fn report_text(report: Option<&WorkspaceMissionReport>) -> String {
    let Some(report) = report else {
        return "Not inspected. Inspect the saved plan to capture current checkout, dependency and delivery facts. A saved plan is not a successful delivery.".into();
    };
    let mut lines = vec![format!(
        "{} · contract revision {}",
        report.state.label(),
        report.revision
    )];
    lines.extend(
        report
            .errors
            .iter()
            .map(|error| format!("Needs you: {error}")),
    );
    for repository in &report.repositories {
        lines.push(format!(
            "\n{}\n{}\nCandidate: {}",
            repository.identity.full_name,
            repository.path.display(),
            repository.candidate.as_deref().unwrap_or("Not frozen")
        ));
        lines.push(format!(
            "Committed: {} · Pushed: {} · Integrated: {} · Validated: {} · Core pin current: {}",
            repository.committed,
            repository.pushed,
            repository.integrated,
            repository.validated,
            repository.pins_current
        ));
        if let Some(observed) = &repository.observed {
            lines.push(format!("Observed HEAD: {} · dirty: {} · observed at {} ms\nFingerprint: {}\nCargo manifest pin: {}\nCargo lock pin: {}",
                observed.head, observed.dirty, observed.observed_at_ms, observed.fingerprint,
                observed.manifest_core_pin.as_deref().unwrap_or("None"), observed.lock_core_pin.as_deref().unwrap_or("None")));
        }
        if let Some(observation) = &repository.pushed_observation {
            lines.push(format!(
                "Push observation (not a current gate): {} {} = {} at {} ms",
                observation.repository.full_name,
                observation.ref_name,
                observation.head,
                observation.observed_at_ms
            ));
        }
        if let Some(observation) = &repository.integration_observation {
            lines.push(format!("Integration observation (not a current gate): {} {} = {} at {} ms; candidate {} contained: {}", observation.remote.repository.full_name, observation.remote.ref_name, observation.remote.head, observation.remote.observed_at_ms, observation.candidate, observation.contains_candidate));
        }
        for check in &repository.checks {
            lines.push(format!(
                "{} · {} · {} · receipt {}{}",
                check.check_name,
                check.environment,
                check.status.label(),
                check.receipt_id.as_deref().unwrap_or("None"),
                check
                    .detail
                    .as_ref()
                    .map(|detail| format!("\n  {detail}"))
                    .unwrap_or_default()
            ));
        }
        lines.extend(
            repository
                .errors
                .iter()
                .map(|error| format!("Needs you: {error}")),
        );
    }
    lines.join("\n")
}

impl MissionSurface {
    pub fn new(callback: Rc<dyn Fn(Action)>) -> Self {
        let state = Rc::new(RefCell::new(MissionWorkbench::new()));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.set_margin_top(16);
        root.set_margin_bottom(16);
        root.set_margin_start(16);
        root.set_margin_end(16);
        let heading = label("Missions · linked repository delivery");
        heading.add_css_class("title-2");
        root.append(&heading);
        root.append(&label("A Mission links existing tasks and exact repository candidates. Save edits only the plan. Inspection is read only. Start, validation, review and integration stay in each task's existing controls."));
        let refresh = gtk::Button::with_label("Refresh saved Missions and expire old evidence");
        root.append(&refresh);
        let catalog = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.append(&catalog);
        let create_fields = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let title = gtk::Entry::new();
        title.set_placeholder_text(Some("Delivery title"));
        let goal = gtk::Entry::new();
        goal.set_placeholder_text(Some("Acceptance goal"));
        let parent = gtk::Entry::new();
        parent.set_placeholder_text(Some(
            "Absolute parent directory containing all five repositories",
        ));
        for (name, entry) in [
            ("Title", &title),
            ("Goal", &goal),
            ("Checkout parent", &parent),
        ] {
            entry.set_hexpand(true);
            create_fields.append(&label(name));
            create_fields.append(entry);
        }
        let create = gtk::Button::with_label("Create five-repository plan");
        create_fields.append(&create);
        let new = gtk::Expander::new(Some("New Mission"));
        new.set_child(Some(&create_fields));
        root.append(&new);
        let status = label("Open a saved Mission or create a new plan.");
        status.set_selectable(true);
        root.append(&status);
        root.append(&label("Editable plan: paths, candidate commits, target refs, exact core pins, required checks and task/run references. Host-owned evidence is shown separately below."));
        let view = gtk::TextView::new();
        view.set_monospace(true);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_top_margin(8);
        view.set_bottom_margin(16);
        let editor = view.buffer();
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroll.set_min_content_height(240);
        scroll.set_child(Some(&view));
        root.append(&scroll);
        let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let save = gtk::Button::with_label("Save plan with revision check");
        let reload = gtk::Button::with_label("Discard local edits and reload selected plan");
        let inspect = gtk::Button::with_label("Inspect saved repository candidates");
        let adopt = gtk::Button::with_label("Freeze inspected clean commits as candidates");
        let link = gtk::Button::with_label("Link the last selected hosted task and run");
        for button in [&save, &reload, &inspect, &adopt, &link] {
            actions.append(button);
        }
        root.append(&actions);
        let evidence = label("");
        evidence.set_selectable(true);
        root.append(&evidence);
        root.append(&label("Linked task evidence opens its exact conversation. Compare the saved run ID below with the inspector's current capture before using a result."));
        let task_links = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.append(&task_links);
        let surface = Self(Rc::new(MissionWidgets {
            root,
            state,
            catalog,
            catalog_rows: Rc::new(RefCell::new(HashMap::new())),
            editor,
            editor_view: view,
            status,
            evidence,
            task_links,
            rendered_links: RefCell::new(None),
            callback,
            save,
            inspect,
            adopt,
            link,
            linked_task: Rc::new(RefCell::new(None)),
            dirty: Rc::new(Cell::new(false)),
            submitted: RefCell::new(None),
            rendering: Rc::new(Cell::new(false)),
            loaded: Rc::new(RefCell::new(None)),
        }));
        let dirty = surface.dirty.clone();
        let rendering = surface.rendering.clone();
        surface.editor.connect_changed(move |_| {
            if !rendering.get() {
                dirty.set(true);
            }
        });
        let weak = Rc::downgrade(&surface.0);
        refresh.connect_clicked(move |_| {
            let Some(data) = weak.upgrade().map(MissionSurface) else {
                return;
            };
            data.state.borrow_mut().refresh();
            data.render();
        });
        let weak = Rc::downgrade(&surface.0);
        create.connect_clicked(move |_| {
            let Some(data) = weak.upgrade().map(MissionSurface) else {
                return;
            };
            if data.dirty.get() {
                data.state.borrow_mut().notice =
                    Some("Save or discard local edits before creating another Mission.".into());
                data.render();
                return;
            }
            let parent = PathBuf::from(parent.text().as_str());
            if !parent.is_absolute() {
                data.state.borrow_mut().notice =
                    Some("Use an absolute checkout parent directory.".into());
                data.render();
                return;
            }
            let roots = ["jterm_core", "anvil", "forge", "ember", "frost"]
                .into_iter()
                .map(|name| (format!("beamiter/{name}"), parent.join(name)))
                .collect::<Vec<_>>();
            data.state
                .borrow_mut()
                .create(title.text().as_str(), goal.text().as_str(), &roots);
            data.render();
        });
        let weak = Rc::downgrade(&surface.0);
        surface.save.connect_clicked(move |_| {
            let Some(data) = weak.upgrade().map(MissionSurface) else {
                return;
            };
            let text = data
                .editor
                .text(&data.editor.start_iter(), &data.editor.end_iter(), true);
            let selected = data.state.borrow().selected.clone();
            let Some(selected) = selected else {
                return;
            };
            match editor_plan(text.as_str(), &selected) {
                Ok(plan) => {
                    *data.submitted.borrow_mut() = Some(text.to_string());
                    data.state.borrow_mut().save(plan);
                }
                Err(error) => {
                    data.state.borrow_mut().notice = Some(error);
                    data.render();
                    return;
                }
            }
            data.render();
        });
        let weak = Rc::downgrade(&surface.0);
        reload.connect_clicked(move |_| {
            let Some(data) = weak.upgrade().map(MissionSurface) else {
                return;
            };
            let selected = data.state.borrow().selected.as_ref().map(|m| m.id.clone());
            if let Some(id) = selected {
                data.dirty.set(false);
                *data.submitted.borrow_mut() = None;
                *data.loaded.borrow_mut() = None;
                data.state.borrow_mut().select(&id);
                data.render();
            }
        });
        let weak = Rc::downgrade(&surface.0);
        surface.inspect.connect_clicked(move |_| {
            let Some(data) = weak.upgrade().map(MissionSurface) else {
                return;
            };
            if data.dirty.get() {
                data.state.borrow_mut().notice =
                    Some("Save or discard local edits before inspecting the saved plan.".into());
                data.render();
                return;
            }
            data.state.borrow_mut().inspect();
            data.render();
        });
        let weak = Rc::downgrade(&surface.0);
        surface.adopt.connect_clicked(move |_| {
            let Some(data) = weak.upgrade().map(MissionSurface) else {
                return;
            };
            // The core preserves old observations for provenance after reload.
            // Only the shared controller's fresh inspection authorizes using
            // one as a new candidate; old saved observations remain read only.
            if !data.can_adopt() {
                data.state.borrow_mut().notice =
                    Some("Inspect the current saved plan before freezing candidates.".into());
                data.render();
                return;
            }
            data.state.borrow_mut().adopt_candidates();
            data.render();
        });
        let weak = Rc::downgrade(&surface.0);
        surface.link.connect_clicked(move |_| {
            let Some(data) = weak.upgrade().map(MissionSurface) else {
                return;
            };
            if data.dirty.get() {
                data.state.borrow_mut().notice =
                    Some("Save or discard local edits before linking a task.".into());
                data.render();
                return;
            }
            if let Some(id) = data.linked_task.borrow().as_ref() {
                data.state.borrow_mut().link_conversation(id);
            }
            data.render();
        });
        super::workspace_navigation::wrap_button_labels(&surface.root);
        surface.render();
        surface
    }

    pub fn open(&self, id: Option<&str>) {
        if self.dirty.get()
            && id.is_some_and(|id| {
                self.state
                    .borrow()
                    .selected
                    .as_ref()
                    .is_none_or(|m| m.id != id)
            })
        {
            self.state.borrow_mut().notice =
                Some("Save or discard local edits before opening another Mission.".into());
            self.render();
            return;
        }
        if let Some(id) = id {
            self.state.borrow_mut().select(id);
        } else {
            self.state.borrow_mut().refresh();
        }
        self.render();
    }

    fn can_adopt(&self) -> bool {
        !self.dirty.get() && self.state.borrow().can_adopt_candidates()
    }

    pub fn link_target(&self, id: ConversationId) {
        *self.linked_task.borrow_mut() = Some(id);
    }

    pub fn poll(&self) {
        self.state.borrow_mut().poll();
        self.render();
    }

    pub fn rows(&self, project: Option<&std::path::Path>) -> Vec<Row> {
        let data = self.state.borrow();
        data.missions
            .iter()
            .filter(|mission| {
                project.is_none_or(|project| {
                    mission
                        .repositories
                        .iter()
                        .any(|repo| repo.path.starts_with(project))
                })
            })
            .map(|mission| {
                let report = data.report_for(&mission.id);
                let dirty = self.dirty.get()
                    && data
                        .selected
                        .as_ref()
                        .is_some_and(|selected| selected.id == mission.id);
                Row {
                    action: Action::Mission(mission.id.clone()),
                    title: mission.title.clone(),
                    detail: format!(
                        "Mission · {} · {} repositories",
                        if dirty {
                            "Unsaved edits"
                        } else {
                            report
                                .map(|r| r.state.label())
                                .unwrap_or("Inspection needed")
                        },
                        mission.repositories.len()
                    ),
                    searchable: format!(
                        "{} {}",
                        mission.goal,
                        mission
                            .repositories
                            .iter()
                            .map(|r| r.identity.full_name.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                    active: false,
                    busy: false,
                    attention: dirty || report.is_none_or(WorkspaceMissionReport::needs_attention),
                    archived: false,
                    pinned: false,
                    project: project
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                    provider: "Mission".into(),
                    updated: mission.updated_at_ms,
                    source: None,
                }
            })
            .collect()
    }

    fn render(&self) {
        let data = self.state.borrow();
        self.editor_view.set_editable(data.selected.is_some());
        self.editor_view.set_cursor_visible(data.selected.is_some());
        let loaded = data.selected.as_ref().map(|m| (m.id.clone(), m.revision));
        // A write acknowledges only its submitted snapshot. Never replace edits
        // made while that write (or its reconciliation) was in flight.
        let submitted = if data.busy {
            None
        } else {
            self.submitted.borrow_mut().take()
        };
        let accepted_save = submitted.as_deref().is_some_and(|submitted| {
            data.notice.is_none()
                && submitted
                    == self
                        .editor
                        .text(&self.editor.start_iter(), &self.editor.end_iter(), true)
                        .as_str()
        });
        if *self.loaded.borrow() != loaded && (!self.dirty.get() || accepted_save) {
            self.rendering.set(true);
            self.editor.set_text(
                &data
                    .selected
                    .as_ref()
                    .and_then(|m| serde_json::to_string_pretty(m).ok())
                    .unwrap_or_default(),
            );
            self.rendering.set(false);
            self.dirty.set(false);
            *self.loaded.borrow_mut() = loaded.clone();
        }
        let status = data.notice.clone().unwrap_or_else(|| {
            if data.busy {
                "Waiting for the session host…".into()
            } else if self.dirty.get() {
                if *self.loaded.borrow() != loaded {
                    format!("Newer local edits were preserved. The host now has revision {}. Copy your draft before discarding and reloading to compare; a stale revision cannot overwrite the saved plan.", data.selected.as_ref().map(|m| m.revision).unwrap_or_default())
                } else {
                    "Unsaved local plan edits. Evidence is not current for this draft.".into()
                }
            } else {
                format!(
                    "{} saved Missions · {} need you",
                    data.missions.len(),
                    data.needs_attention()
                )
            }
        });
        self.status.set_text(&status);
        let ready = data.selected.is_some() && !data.busy;
        self.save.set_sensitive(ready && self.dirty.get());
        self.inspect.set_sensitive(ready && !self.dirty.get());
        self.adopt.set_sensitive(self.can_adopt());
        let link_caption = self
            .linked_task
            .borrow()
            .as_ref()
            .map(|id| format!("Link selected hosted task and current run: {}", id.0))
            .unwrap_or_else(|| "Select a hosted task before linking its run".into());
        if self.link.label().as_deref() != Some(link_caption.as_str()) {
            self.link.set_label(&link_caption);
            super::workspace_navigation::wrap_button_labels(&self.link);
        }
        self.link
            .set_sensitive(ready && !self.dirty.get() && self.linked_task.borrow().is_some());
        self.evidence.set_text(&if self.dirty.get() {
            "Save or discard draft changes before using evidence.".into()
        } else {
            report_text(data.report.as_ref())
        });
        let link_key = data
            .selected
            .as_ref()
            .map(|mission| (mission.id.clone(), mission.revision));
        if *self.rendered_links.borrow() != link_key {
            while let Some(child) = self.task_links.first_child() {
                self.task_links.remove(&child);
            }
            if let Some(mission) = &data.selected {
                for repository in &mission.repositories {
                    for task in &repository.tasks {
                        let button = gtk::Button::with_label(&format!(
                            "{} · saved run {} · inspect task",
                            repository.identity.full_name, task.run_id
                        ));
                        let callback = self.callback.clone();
                        let conversation = task.conversation.clone();
                        button.connect_clicked(move |_| {
                            callback(Action::Inspect(conversation.clone()))
                        });
                        self.task_links.append(&button);
                    }
                }
            }
            super::workspace_navigation::wrap_button_labels(&self.task_links);
            *self.rendered_links.borrow_mut() = link_key;
        }
        let mut widgets = self.catalog_rows.borrow_mut();
        widgets.retain(|id, button| {
            if data.missions.iter().any(|mission| &mission.id == id) {
                true
            } else {
                self.catalog.remove(button);
                false
            }
        });
        for mission in &data.missions {
            let button = widgets.entry(mission.id.clone()).or_insert_with(|| {
                let button = gtk::Button::new();
                let weak = Rc::downgrade(&self.0);
                let id = mission.id.clone();
                button.connect_clicked(move |_| {
                    if let Some(surface) = weak.upgrade().map(MissionSurface) {
                        surface.open(Some(&id));
                    }
                });
                self.catalog.append(&button);
                button
            });
            let caption = format!("{} · revision {}", mission.title, mission.revision);
            if button.label().as_deref() != Some(caption.as_str()) {
                button.set_label(&caption);
                super::workspace_navigation::wrap_button_labels(button);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_editor_cannot_retarget_identity_or_overwrite_a_newer_revision() {
        let original = WorkspaceMission::new("Delivery", "Verify exact candidates");
        let valid = serde_json::to_string(&original).unwrap();
        assert_eq!(editor_plan(&valid, &original).unwrap(), original);
        let mut changed = original.clone();
        changed.id = WorkspaceMission::new_id();
        assert!(editor_plan(&serde_json::to_string(&changed).unwrap(), &original).is_err());
        changed = original.clone();
        changed.revision += 1;
        assert!(editor_plan(&serde_json::to_string(&changed).unwrap(), &original).is_err());
    }

    #[test]
    #[ignore = "requires an isolated GTK display and one test per process"]
    fn mission_editor_preserves_newer_drafts_stable_rows_and_releases_callbacks() {
        gtk::init().expect("isolated GTK display is required");
        let surface = MissionSurface::new(Rc::new(|_| panic!("rendering must not navigate")));
        assert!(!surface.editor_view.is_editable());
        let mut plan = WorkspaceMission::new(&"M".repeat(256), "Exact delivery");
        plan.revision = 1;
        {
            let mut state = surface.state.borrow_mut();
            state.selected = Some(plan.clone());
            state.missions = vec![plan.clone()];
        }
        surface.render();
        let original_row = surface.catalog_rows.borrow()[&plan.id].clone();
        let initial = surface
            .editor
            .text(
                &surface.editor.start_iter(),
                &surface.editor.end_iter(),
                true,
            )
            .to_string();
        *surface.submitted.borrow_mut() = Some(initial.clone());
        surface.state.borrow_mut().busy = true;
        surface.editor.set_text(&format!("{initial} "));
        surface.render();
        plan.revision = 2;
        {
            let mut state = surface.state.borrow_mut();
            state.busy = false;
            state.selected = Some(plan.clone());
            state.missions = vec![plan.clone()];
        }
        surface.render();
        assert!(surface.dirty.get());
        assert!(surface.status.text().contains("preserved"));
        let newer_draft = surface.editor.text(
            &surface.editor.start_iter(),
            &surface.editor.end_iter(),
            true,
        );
        assert_eq!(newer_draft.as_str(), format!("{initial} "));
        assert!(editor_plan(&newer_draft, &plan).is_err());
        assert!(!surface.inspect.is_sensitive());
        assert_eq!(surface.catalog_rows.borrow()[&plan.id], original_row);
        assert!(surface.rows(None)[0].attention);
        assert!(!surface.adopt.is_sensitive());
        assert!(surface.evidence.text().contains("draft"));
        // Explicit discard/reload is the only operation allowed to remove that
        // newer draft. An ordinary unchanged acknowledgement can update it.
        surface.dirty.set(false);
        surface.render();
        let current = surface
            .editor
            .text(
                &surface.editor.start_iter(),
                &surface.editor.end_iter(),
                true,
            )
            .to_string();
        *surface.submitted.borrow_mut() = Some(current);
        surface.dirty.set(true);
        plan.revision = 3;
        surface.state.borrow_mut().selected = Some(plan.clone());
        surface.render();
        assert!(!surface.dirty.get());
        assert!(surface.inspect.is_sensitive());
        assert_eq!(
            surface.loaded.borrow().as_ref(),
            Some(&(plan.id.clone(), 3))
        );
        // A retained saved observation cannot enable Freeze after reopening or
        // refresh. A completed explicit inspection is a separate authority.
        surface.state.borrow_mut().report = Some(plan.project(&[]));
        surface.render();
        assert!(!surface.adopt.is_sensitive());
        surface
            .state
            .borrow_mut()
            .report
            .as_mut()
            .unwrap()
            .require_reinspection();
        surface.render();
        assert!(!surface.adopt.is_sensitive());
        let (minimum, _, _, _) = surface.root.measure(gtk::Orientation::Horizontal, -1);
        assert!(minimum <= 360, "Mission content minimum width is {minimum}");
        let root = surface.root.clone();
        let weak = Rc::downgrade(&surface.0);
        drop(surface);
        assert!(
            weak.upgrade().is_none(),
            "widget callbacks must not retain the surface"
        );
        drop(root);
    }

    #[test]
    fn missing_inspection_is_not_a_pass() {
        let summary = report_text(None);
        assert!(summary.contains("Not inspected"));
        assert!(!summary.contains("Mission completed"));
    }

    #[test]
    fn cached_clean_report_cannot_authorize_candidate_adoption() {
        use jterm_core::agent_workspace::WorkspaceMissionLocalEvidence;
        let plan = WorkspaceMission::four_terminals("Delivery", "Current candidates", "/work");
        let mut report = plan.project(&[]);
        for repository in &mut report.repositories {
            repository.observed = Some(WorkspaceMissionLocalEvidence {
                baseline: "a".repeat(40),
                head: "b".repeat(40),
                fingerprint: "saved-fingerprint".into(),
                modified: true,
                dirty: false,
                manifest_core_pin: None,
                lock_core_pin: None,
                observed_at_ms: 42,
            });
        }
        let mut controller = MissionWorkbench::new();
        controller.selected = Some(plan);
        controller.report = Some(report);
        assert!(!controller.can_adopt_candidates());
        controller.report.as_mut().unwrap().require_reinspection();
        assert!(!controller.can_adopt_candidates());
    }

    #[test]
    fn stale_remote_observations_remain_visible_without_restoring_gates() {
        use jterm_core::agent_workspace::{
            WorkspaceMissionCheckReport, WorkspaceMissionCheckStatus,
            WorkspaceMissionIntegrationEvidence, WorkspaceMissionRemoteEvidence,
        };
        let plan = WorkspaceMission::four_terminals("Delivery", "Verify all five", "/work");
        let mut report = plan.project(&[]);
        let repository = &mut report.repositories[0];
        let remote = WorkspaceMissionRemoteEvidence {
            repository: repository.identity.clone(),
            ref_name: "refs/heads/master".into(),
            head: "a".repeat(40),
            observed_at_ms: 42,
        };
        repository.pushed_observation = Some(remote.clone());
        repository.integration_observation = Some(WorkspaceMissionIntegrationEvidence {
            remote,
            candidate: "b".repeat(40),
            contains_candidate: true,
        });
        repository.pushed = true;
        repository.integrated = true;
        repository.checks.push(WorkspaceMissionCheckReport {
            check_name: "native-smoke".into(),
            environment: "Linux X11".into(),
            status: WorkspaceMissionCheckStatus::Passed,
            receipt_id: Some("exact-host-receipt".into()),
            detail: None,
        });
        report.require_reinspection();
        let summary = report_text(Some(&report));
        assert!(summary.contains("Pushed: false · Integrated: false"));
        assert!(summary.contains("Push observation (not a current gate)"));
        assert!(summary.contains("refs/heads/master"));
        assert!(summary.contains(&"a".repeat(40)));
        assert!(summary.contains("at 42 ms"));
        assert!(summary.contains("native-smoke · Linux X11 · Stale · receipt exact-host-receipt"));
        assert!(!summary.contains("Mission completed"));
    }
}
