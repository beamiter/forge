//! GTK agent Tasks panel.
//!
//! Plain-GTK port of anvil's Relm4 `dialogs/tasks_panel.rs`: the panel is a
//! pure view. [`UiState`](super::UiState) owns the task manager, native
//! runtime, and diff worker, pushes composed snapshots in through
//! [`TasksPanel::sync`], and executes the [`TaskPanelAction`] values this
//! panel stages through its action callback. Provider-controlled text arrives
//! already display-safe; every widget here treats it as plain text.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    Box as GBox, Button, Entry, FlowBox, Label, ListBox, ScrolledWindow, Stack, TextView,
    ToggleButton,
};

use crate::agent_task::{ApprovalId, CodexAppServerApproval, TaskId};
use crate::agent_task_ui::{
    approval_summary, native_follow_up_can_send, plan_list_refresh, render_stream_text,
    row_status_line, TaskPanelAction, TaskRowSnapshot,
};

const STREAM_PAGE: &str = "stream";
const DIFF_PAGE: &str = "diff";
const CREATE_TASK_LABEL: &str = "New agent task from selected block";
const CLOSE_TASKS_LABEL: &str = "Close Tasks panel";

/// Full panel state pushed by the application after every domain change.
#[derive(Clone, Debug, Default)]
pub(crate) struct TasksPanelSync {
    pub(crate) rows: Vec<TaskRowSnapshot>,
    pub(crate) selected: Option<TaskId>,
    pub(crate) detail: Option<Box<TaskDetailSync>>,
    pub(crate) create_enabled: bool,
    pub(crate) create_hint: String,
    pub(crate) pending_creation: bool,
}

/// Everything the detail pane renders for the selected task.
#[derive(Clone, Debug)]
pub(crate) struct TaskDetailSync {
    pub(crate) id: TaskId,
    pub(crate) title: String,
    pub(crate) status_line: String,
    pub(crate) branch: String,
    pub(crate) stream: Option<Box<crate::agent_task::CodexAppServerViewSnapshot>>,
    pub(crate) approvals: Vec<crate::agent_task::CodexAppServerApproval>,
    pub(crate) completed_turns: usize,
    pub(crate) can_start_codex: bool,
    pub(crate) can_start_terminal: bool,
    pub(crate) can_stop: bool,
    pub(crate) can_finish: bool,
    pub(crate) can_run_validation: bool,
    pub(crate) can_complete: bool,
    pub(crate) can_follow_up: bool,
    pub(crate) follow_up_hint: String,
    pub(crate) diff: Option<DiffSync>,
}

#[derive(Clone, Debug)]
pub(crate) struct DiffSync {
    pub(crate) header: String,
    pub(crate) scope: String,
    pub(crate) loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) truncated: bool,
    pub(crate) text: String,
}

/// Fixed set of task action buttons; the selected task id is resolved when
/// the action fires, never captured into a stale GTK closure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionKind {
    StartCodex,
    StartTerminal,
    StopCodex,
    FinishCodex,
    RunValidation,
    Complete,
    ReviewDiff,
    Archive,
}

const ACTION_KINDS: [(ActionKind, &str); 8] = [
    (ActionKind::StartCodex, "Start Codex"),
    (ActionKind::StartTerminal, "Terminal"),
    (ActionKind::StopCodex, "Stop"),
    (ActionKind::FinishCodex, "Finish"),
    (ActionKind::RunValidation, "Validate"),
    (ActionKind::Complete, "Complete"),
    (ActionKind::ReviewDiff, "Diff"),
    (ActionKind::Archive, "Archive"),
];

type ActionCallback = Rc<RefCell<Box<dyn Fn(TaskPanelAction)>>>;

/// The right-side agent Tasks panel. Cheap to clone; every clone shares the
/// same widgets and staged state, like the other forge panel handles.
#[derive(Clone)]
pub(crate) struct TasksPanel {
    pub(crate) root: GBox,
    state: Rc<RefCell<TasksPanelSync>>,
    /// Per-task follow-up drafts; the app never sees keystrokes, only the
    /// final text carried by `TaskPanelAction::FollowUp`.
    follow_up_drafts: Rc<RefCell<HashMap<TaskId, String>>>,
    /// Render cache: what the list and approval widgets currently show.
    /// Refresh diffs each pushed Sync against it and touches GTK only on
    /// real change, so an unchanged Sync is a pure no-op — no widget churn,
    /// and no selection signals echoing back as user gestures.
    rendered_rows: Rc<RefCell<Vec<TaskRowSnapshot>>>,
    rendered_selected: Rc<Cell<Option<TaskId>>>,
    rendered_approvals: Rc<RefCell<Vec<CodexAppServerApproval>>>,
    /// Set while refresh applies a programmatic selection, so the resulting
    /// `row-selected` emission is not mistaken for a user gesture.
    selection_guard: Rc<Cell<bool>>,
    on_action: ActionCallback,
    create_button: Button,
    close_button: Button,
    create_hint: Label,
    task_list: ListBox,
    detail_title: Label,
    detail_status: Label,
    actions_box: FlowBox,
    action_buttons: Rc<RefCell<Vec<(ActionKind, Button)>>>,
    approvals_box: GBox,
    stream_page_button: ToggleButton,
    diff_page_button: ToggleButton,
    page_stack: Stack,
    stream_view: TextView,
    diff_header: Label,
    diff_view: TextView,
    follow_up_hint: Label,
    follow_up_row: GBox,
    follow_up_entry: Entry,
    follow_up_send: Button,
}

impl TasksPanel {
    pub(crate) fn build() -> Self {
        let root = GBox::new(gtk4::Orientation::Vertical, 0);
        // No width floor: the AI Chats page has none, and both pages share
        // the side stack, so a floor here would raise the stack's minimum
        // width for every page.
        root.set_hexpand(false);
        root.set_vexpand(true);
        root.add_css_class("ai-panel");

        let header = GBox::new(gtk4::Orientation::Horizontal, 4);
        header.set_margin_top(6);
        header.set_margin_bottom(6);
        header.set_margin_start(6);
        header.set_margin_end(6);
        let title = Label::new(Some("Agent tasks"));
        title.set_halign(gtk4::Align::Start);
        title.set_hexpand(true);
        title.add_css_class("heading");
        header.append(&title);
        let create_button = Button::from_icon_name("list-add-symbolic");
        create_button.add_css_class("flat");
        create_button.set_tooltip_text(Some("New agent task from the selected block"));
        create_button.update_property(&[gtk4::accessible::Property::Label(CREATE_TASK_LABEL)]);
        header.append(&create_button);
        let close_button = Button::from_icon_name("window-close-symbolic");
        close_button.add_css_class("flat");
        close_button.set_tooltip_text(Some(CLOSE_TASKS_LABEL));
        close_button.update_property(&[gtk4::accessible::Property::Label(CLOSE_TASKS_LABEL)]);
        header.append(&close_button);
        root.append(&header);

        let create_hint = Label::new(None);
        create_hint.set_halign(gtk4::Align::Start);
        create_hint.set_margin_start(8);
        create_hint.set_margin_end(8);
        create_hint.set_wrap(true);
        create_hint.add_css_class("dim-label");
        create_hint.add_css_class("caption");
        create_hint.set_visible(false);
        root.append(&create_hint);

        let paned = gtk4::Paned::new(gtk4::Orientation::Vertical);
        paned.set_vexpand(true);
        paned.set_wide_handle(true);
        paned.set_position(170);
        root.append(&paned);

        let list_scroll = ScrolledWindow::new();
        list_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        list_scroll.set_min_content_height(110);
        let task_list = ListBox::new();
        task_list.set_selection_mode(gtk4::SelectionMode::Single);
        task_list.add_css_class("navigation-sidebar");
        list_scroll.set_child(Some(&task_list));
        paned.set_start_child(Some(&list_scroll));

        let detail_box = GBox::new(gtk4::Orientation::Vertical, 4);
        detail_box.set_margin_top(6);
        detail_box.set_margin_bottom(6);
        detail_box.set_margin_start(6);
        detail_box.set_margin_end(6);
        paned.set_end_child(Some(&detail_box));

        let detail_title = Label::new(None);
        detail_title.set_halign(gtk4::Align::Start);
        detail_title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        detail_title.add_css_class("heading");
        detail_title.set_visible(false);
        detail_box.append(&detail_title);

        let detail_status = Label::new(None);
        detail_status.set_halign(gtk4::Align::Start);
        detail_status.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        detail_status.add_css_class("dim-label");
        detail_status.add_css_class("caption");
        detail_status.set_visible(false);
        detail_box.append(&detail_status);

        let actions_box = FlowBox::new();
        actions_box.set_selection_mode(gtk4::SelectionMode::None);
        actions_box.set_column_spacing(4);
        actions_box.set_row_spacing(4);
        actions_box.set_max_children_per_line(4);
        actions_box.set_visible(false);
        detail_box.append(&actions_box);

        let approvals_box = GBox::new(gtk4::Orientation::Vertical, 4);
        approvals_box.set_visible(false);
        detail_box.append(&approvals_box);

        let page_buttons = GBox::new(gtk4::Orientation::Horizontal, 0);
        page_buttons.set_halign(gtk4::Align::Center);
        let stream_page_button = ToggleButton::with_label("Stream");
        stream_page_button.set_active(true);
        stream_page_button.add_css_class("flat");
        stream_page_button.add_css_class("caption");
        page_buttons.append(&stream_page_button);
        let diff_page_button = ToggleButton::with_label("Diff");
        diff_page_button.add_css_class("flat");
        diff_page_button.add_css_class("caption");
        page_buttons.append(&diff_page_button);
        detail_box.append(&page_buttons);

        let page_stack = Stack::new();
        page_stack.set_hexpand(true);
        page_stack.set_vexpand(true);

        let stream_scroll = ScrolledWindow::new();
        stream_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        let stream_view = TextView::new();
        stream_view.set_editable(false);
        stream_view.set_cursor_visible(false);
        stream_view.set_wrap_mode(gtk4::WrapMode::WordChar);
        stream_view.set_left_margin(4);
        stream_view.set_right_margin(4);
        stream_scroll.set_child(Some(&stream_view));
        page_stack.add_named(&stream_scroll, Some(STREAM_PAGE));

        let diff_box = GBox::new(gtk4::Orientation::Vertical, 2);
        let diff_header = Label::new(None);
        diff_header.set_halign(gtk4::Align::Start);
        diff_header.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        diff_header.add_css_class("dim-label");
        diff_header.add_css_class("caption");
        diff_box.append(&diff_header);
        let diff_warning = Label::new(Some(
            "Repository-controlled paths and content are untrusted; control and bidirectional formatting characters are made visible or replaced.",
        ));
        diff_warning.set_halign(gtk4::Align::Start);
        diff_warning.set_wrap(true);
        diff_warning.add_css_class("warning");
        diff_warning.add_css_class("caption");
        diff_box.append(&diff_warning);
        let diff_scroll = ScrolledWindow::new();
        diff_scroll.set_hexpand(true);
        diff_scroll.set_vexpand(true);
        diff_scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Automatic);
        let diff_view = TextView::new();
        diff_view.set_editable(false);
        diff_view.set_cursor_visible(false);
        diff_view.set_monospace(true);
        diff_view.set_left_margin(4);
        diff_view.set_right_margin(4);
        diff_scroll.set_child(Some(&diff_view));
        diff_box.append(&diff_scroll);
        page_stack.add_named(&diff_box, Some(DIFF_PAGE));
        detail_box.append(&page_stack);

        let follow_up_hint = Label::new(None);
        follow_up_hint.set_halign(gtk4::Align::Start);
        follow_up_hint.add_css_class("dim-label");
        follow_up_hint.add_css_class("caption");
        follow_up_hint.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        follow_up_hint.set_visible(false);
        detail_box.append(&follow_up_hint);

        let follow_up_row = GBox::new(gtk4::Orientation::Horizontal, 4);
        follow_up_row.set_visible(false);
        let follow_up_entry = Entry::new();
        follow_up_entry.set_hexpand(true);
        follow_up_entry.set_placeholder_text(Some("Follow-up turn for Codex…"));
        follow_up_row.append(&follow_up_entry);
        let follow_up_send = Button::from_icon_name("go-next-symbolic");
        follow_up_send.add_css_class("flat");
        follow_up_send.set_tooltip_text(Some("Send follow-up turn"));
        follow_up_row.append(&follow_up_send);
        detail_box.append(&follow_up_row);

        let panel = Self {
            root,
            state: Rc::new(RefCell::new(TasksPanelSync::default())),
            follow_up_drafts: Rc::new(RefCell::new(HashMap::new())),
            rendered_rows: Rc::new(RefCell::new(Vec::new())),
            rendered_selected: Rc::new(Cell::new(None)),
            rendered_approvals: Rc::new(RefCell::new(Vec::new())),
            selection_guard: Rc::new(Cell::new(false)),
            on_action: Rc::new(RefCell::new(Box::new(|_| {}))),
            create_button,
            close_button,
            create_hint,
            task_list,
            detail_title,
            detail_status,
            actions_box,
            action_buttons: Rc::new(RefCell::new(Vec::new())),
            approvals_box,
            stream_page_button,
            diff_page_button,
            page_stack,
            stream_view,
            diff_header,
            diff_view,
            follow_up_hint,
            follow_up_row,
            follow_up_entry,
            follow_up_send,
        };
        panel.build_action_buttons();
        panel.connect_signals();
        panel
    }

    fn build_action_buttons(&self) {
        // Populated once at construction; sensitivity is re-resolved from the
        // latest snapshot on every `sync`.
        for (kind, label) in ACTION_KINDS {
            let button = Button::with_label(label);
            button.add_css_class("caption");
            let panel = self.clone();
            button.connect_clicked(move |_| panel.act(kind));
            self.actions_box.insert(&button, -1);
            self.action_buttons.borrow_mut().push((kind, button));
        }
    }

    fn connect_signals(&self) {
        let panel = self.clone();
        self.create_button.connect_clicked(move |_| {
            panel.emit(TaskPanelAction::CreateFromBlock);
        });
        let panel = self.clone();
        self.close_button.connect_clicked(move |_| {
            panel.emit(TaskPanelAction::Close);
        });

        let panel = self.clone();
        self.task_list.connect_row_selected(move |_, row| {
            // Only genuine gestures may stage Select; programmatic selection
            // applied by refresh runs behind the guard. Checking first also
            // keeps the handler from re-entering the state cell while
            // refresh still holds it.
            if panel.selection_guard.get() {
                return;
            }
            let Some(row) = row else { return };
            let index = usize::try_from(row.index()).unwrap_or(usize::MAX);
            // Row indices map through the panel's current row table, so a list
            // rebuild mid-gesture cannot retarget the selection.
            let selected = panel.state.borrow().rows.get(index).map(|row| row.id);
            if let Some(id) = selected {
                if panel.state.borrow().selected != Some(id) {
                    panel.emit(TaskPanelAction::Select(id));
                }
            }
        });

        let panel = self.clone();
        self.follow_up_entry.connect_changed(move |entry| {
            let selected = panel.state.borrow().selected;
            if let Some(id) = selected {
                panel
                    .follow_up_drafts
                    .borrow_mut()
                    .insert(id, entry.text().to_string());
                panel.refresh_follow_up_send();
            }
        });
        let panel = self.clone();
        self.follow_up_entry
            .connect_activate(move |_| panel.send_follow_up());
        let panel = self.clone();
        self.follow_up_send
            .connect_clicked(move |_| panel.send_follow_up());

        let panel = self.clone();
        self.stream_page_button.connect_toggled(move |button| {
            if button.is_active() {
                panel.show_page(STREAM_PAGE);
            }
        });
        let panel = self.clone();
        self.diff_page_button.connect_toggled(move |button| {
            if button.is_active() {
                panel.show_page(DIFF_PAGE);
            }
        });
    }

    /// Route one staged action to the application layer, which resolves the
    /// task again at execution time so a concurrent terminal exit or tab
    /// removal cannot redirect it to an unrelated task.
    pub(crate) fn connect_action(&self, callback: impl Fn(TaskPanelAction) + 'static) {
        *self.on_action.borrow_mut() = Box::new(callback);
    }

    fn emit(&self, action: TaskPanelAction) {
        (self.on_action.borrow())(action);
    }

    fn show_page(&self, page: &'static str) {
        self.page_stack.set_visible_child_name(page);
        self.stream_page_button.set_active(page == STREAM_PAGE);
        self.diff_page_button.set_active(page == DIFF_PAGE);
    }

    fn act(&self, kind: ActionKind) {
        // Requesting a diff also opens the diff page so the action has a
        // visible effect even while the worker is still loading.
        if kind == ActionKind::ReviewDiff {
            self.show_page(DIFF_PAGE);
        }
        let Some(id) = self.state.borrow().selected else {
            return;
        };
        let action = match kind {
            ActionKind::StartCodex => TaskPanelAction::StartCodex(id),
            ActionKind::StartTerminal => TaskPanelAction::StartTerminal(id),
            ActionKind::StopCodex => TaskPanelAction::StopCodex(id),
            ActionKind::FinishCodex => TaskPanelAction::FinishCodex(id),
            ActionKind::RunValidation => TaskPanelAction::RunValidation(id),
            ActionKind::Complete => TaskPanelAction::Complete(id),
            ActionKind::ReviewDiff => TaskPanelAction::ReviewDiff(id),
            ActionKind::Archive => TaskPanelAction::Archive(id),
        };
        self.emit(action);
    }

    fn send_follow_up(&self) {
        let detail = self.state.borrow().detail.as_ref().map(|d| d.id);
        let Some(id) = detail else { return };
        let (can_follow_up, completed_turns) = {
            let state = self.state.borrow();
            match state.detail.as_deref() {
                Some(detail) => (detail.can_follow_up, detail.completed_turns),
                None => return,
            }
        };
        let text = self
            .follow_up_drafts
            .borrow()
            .get(&id)
            .cloned()
            .unwrap_or_default();
        if native_follow_up_can_send(&text, completed_turns) && can_follow_up {
            self.follow_up_drafts.borrow_mut().remove(&id);
            self.follow_up_entry.set_text("");
            self.emit(TaskPanelAction::FollowUp(id, text));
        }
    }

    /// Replace the composed panel state and repaint. The view diffs each push
    /// against what it currently renders and touches widgets only on real
    /// change, so an unchanged push is allocation-quiet; the domain remains
    /// the only owner of task state.
    pub(crate) fn sync(&self, sync: TasksPanelSync) {
        *self.state.borrow_mut() = sync;
        self.refresh_view();
    }

    fn refresh_view(&self) {
        let state = self.state.borrow();
        self.create_button
            .set_sensitive(state.create_enabled && !state.pending_creation);
        let show_hint = !state.create_hint.is_empty();
        self.create_hint.set_visible(show_hint);
        if show_hint {
            self.create_hint.set_label(&state.create_hint);
        }

        self.sync_task_list(&state);

        let detail = state.detail.as_deref();
        for (kind, button) in self.action_buttons.borrow().iter() {
            let sensitive = detail.is_some_and(|detail| match kind {
                ActionKind::StartCodex => detail.can_start_codex,
                ActionKind::StartTerminal => detail.can_start_terminal,
                ActionKind::StopCodex => detail.can_stop,
                ActionKind::FinishCodex => detail.can_finish,
                ActionKind::RunValidation => detail.can_run_validation,
                ActionKind::Complete => detail.can_complete,
                ActionKind::ReviewDiff => true,
                // Archiving is always safe to offer for a selected task.
                ActionKind::Archive => true,
            });
            button.set_sensitive(sensitive);
        }
        self.actions_box.set_visible(detail.is_some());
        self.detail_title.set_visible(detail.is_some());
        self.detail_status.set_visible(detail.is_some());

        if let Some(detail) = detail {
            self.detail_title.set_label(&detail.title);
            self.detail_status
                .set_label(&format!("{} · {}", detail.status_line, detail.branch));

            let stream_text = detail
                .stream
                .as_deref()
                .map(render_stream_text)
                .unwrap_or_else(|| {
                    "No native Codex session snapshot for this task yet.".to_string()
                });
            set_view_text(&self.stream_view, &stream_text);

            if let Some(diff) = &detail.diff {
                self.diff_header.set_label(&diff.header);
                self.diff_header.set_tooltip_text(Some(&diff.scope));
                let text = if diff.loading {
                    "Loading tracked changes…".to_string()
                } else if let Some(error) = &diff.error {
                    error.clone()
                } else if diff.text.is_empty() {
                    "No tracked changes.".to_string()
                } else {
                    let mut text = diff.text.clone();
                    if diff.truncated {
                        text.push_str("\n(diff exceeded the retained limits; output truncated)");
                    }
                    text
                };
                set_view_text(&self.diff_view, &text);
            } else {
                self.diff_header
                    .set_label("Use Diff to review this task's worktree changes.");
                set_view_text(&self.diff_view, "");
            }

            self.follow_up_row.set_visible(detail.can_follow_up);
            let show_hint = !detail.follow_up_hint.is_empty();
            self.follow_up_hint.set_visible(show_hint);
            if show_hint {
                self.follow_up_hint.set_label(&detail.follow_up_hint);
            }
            let draft = self
                .follow_up_drafts
                .borrow()
                .get(&detail.id)
                .cloned()
                .unwrap_or_default();
            if self.follow_up_entry.text().as_str() != draft {
                self.follow_up_entry.set_text(&draft);
            }
        }
        drop(state);
        self.refresh_follow_up_send();
        self.sync_approvals();
    }

    fn refresh_follow_up_send(&self) {
        let state = self.state.borrow();
        let sensitive = state.detail.as_deref().is_some_and(|detail| {
            let draft = self
                .follow_up_drafts
                .borrow()
                .get(&detail.id)
                .cloned()
                .unwrap_or_default();
            detail.can_follow_up && native_follow_up_can_send(&draft, detail.completed_turns)
        });
        self.follow_up_send.set_sensitive(sensitive);
    }

    /// Reconcile the task list widget with the pushed row table. Rows are
    /// rebuilt only when their snapshots change, and selection is applied
    /// only when it (or the table) changed, behind `selection_guard` so the
    /// emitted `row-selected` cannot echo back as a Select action. Together
    /// these make refreshing unchanged state idempotent; an unguarded
    /// re-select on every refresh would also re-enter the state cell while
    /// refresh still borrows it.
    fn sync_task_list(&self, state: &TasksPanelSync) {
        let plan = plan_list_refresh(
            &self.rendered_rows.borrow(),
            self.rendered_selected.get(),
            &state.rows,
            state.selected,
        );
        if plan.rebuild_rows {
            while let Some(child) = self.task_list.first_child() {
                self.task_list.remove(&child);
            }
            for row in state.rows.iter() {
                let title = Label::new(Some(&row.title));
                title.set_halign(gtk4::Align::Start);
                title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                title.set_max_width_chars(28);
                let status = Label::new(Some(&row_status_line(row)));
                status.set_halign(gtk4::Align::Start);
                status.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                status.add_css_class("dim-label");
                status.add_css_class("caption");
                if row.needs_attention {
                    status.add_css_class("warning");
                }
                let lines = GBox::new(gtk4::Orientation::Vertical, 2);
                lines.set_margin_top(4);
                lines.set_margin_bottom(4);
                lines.set_margin_start(4);
                lines.append(&title);
                lines.append(&status);
                let list_row = gtk4::ListBoxRow::new();
                list_row.set_child(Some(&lines));
                self.task_list.append(&list_row);
            }
            self.rendered_rows.borrow_mut().clone_from(&state.rows);
        }
        if plan.apply_selection {
            self.selection_guard.set(true);
            match plan.select_index {
                Some(index) => {
                    let row = self.task_list.row_at_index(index as i32);
                    self.task_list.select_row(row.as_ref());
                }
                None => self.task_list.unselect_all(),
            }
            self.selection_guard.set(false);
        }
        if plan.rebuild_rows || plan.apply_selection {
            self.rendered_selected.set(state.selected);
        }
    }

    /// Rebuild the approval cards only when the pushed approval set changed;
    /// an unchanged Sync leaves the existing cards (and their wired
    /// Approve/Deny closures) untouched.
    fn sync_approvals(&self) {
        let state = self.state.borrow();
        let incoming: &[CodexAppServerApproval] = state
            .detail
            .as_deref()
            .map(|detail| detail.approvals.as_slice())
            .unwrap_or(&[]);
        if self.rendered_approvals.borrow().as_slice() == incoming {
            self.approvals_box.set_visible(!incoming.is_empty());
            return;
        }
        while let Some(child) = self.approvals_box.first_child() {
            self.approvals_box.remove(&child);
        }
        self.approvals_box.set_visible(!incoming.is_empty());
        if let Some(detail) = state.detail.as_deref() {
            let task_id = detail.id;
            for approval in incoming {
                let card = GBox::new(gtk4::Orientation::Vertical, 2);
                card.add_css_class("card");
                let summary_label = Label::new(Some(&approval_summary(approval)));
                summary_label.set_halign(gtk4::Align::Start);
                summary_label.set_wrap(true);
                summary_label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
                summary_label.set_margin_top(6);
                summary_label.set_margin_bottom(6);
                summary_label.set_margin_start(6);
                summary_label.set_margin_end(6);
                card.append(&summary_label);
                let buttons = GBox::new(gtk4::Orientation::Horizontal, 6);
                buttons.set_halign(gtk4::Align::End);
                buttons.set_margin_bottom(4);
                let approve = Button::with_label("Approve");
                approve.add_css_class("suggested-action");
                let deny = Button::with_label("Deny");
                deny.add_css_class("destructive-action");
                let approval_id: ApprovalId = approval.id;
                let panel = self.clone();
                approve.connect_clicked(move |_| {
                    panel.emit(TaskPanelAction::Approve(task_id, approval_id));
                });
                let panel = self.clone();
                deny.connect_clicked(move |_| {
                    panel.emit(TaskPanelAction::Deny(task_id, approval_id));
                });
                buttons.append(&approve);
                buttons.append(&deny);
                card.append(&buttons);
                self.approvals_box.append(&card);
            }
        }
        self.rendered_approvals
            .borrow_mut()
            .clone_from_slice(incoming);
    }
}

/// Set a read-only text view's buffer only when the content actually
/// changed; re-setting identical text still forces a fresh layout pass.
fn set_view_text(view: &TextView, text: &str) {
    let buffer = view.buffer();
    if buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .as_str()
        != text
    {
        buffer.set_text(text);
    }
}
