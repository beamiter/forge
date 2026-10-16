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

use crate::agent_task::{CodexAppServerApproval, TaskId};

#[path = "approval_details.rs"]
mod approval_details;
use crate::agent_task_ui::{
    approval_summary, native_follow_up_can_send, plan_list_refresh, render_stream_text,
    row_status_line, task_reason, TaskPanelAction, TaskRowSnapshot,
};

/// Counts describe current search matches before the selected quick view.
fn dashboard_filter_label(
    view: crate::agent_task::DashboardView,
    counts: &crate::agent_task::DashboardCounts,
) -> String {
    use crate::agent_task::DashboardView;
    let (label, count) = match view {
        DashboardView::All => ("All", counts.total),
        DashboardView::NeedsAttention => ("Needs you", counts.attention),
        DashboardView::Running => ("Running", counts.running),
        DashboardView::NotStarted => ("Not started", counts.created),
        DashboardView::Review => ("Review", counts.review),
        DashboardView::Finished => ("Finished", counts.done + counts.failed + counts.cancelled),
    };
    format!("{label} {count}")
}

const STREAM_PAGE: &str = "stream";
const DIFF_PAGE: &str = "diff";
const CREATE_TASK_LABEL: &str = "New agent task from selected block";
const CLOSE_TASKS_LABEL: &str = "Close Tasks panel";

/// Full panel state pushed by the application after every domain change.
#[derive(Clone, Debug, Default)]
pub(crate) struct TasksPanelSync {
    pub(crate) rows: Vec<TaskRowSnapshot>,
    pub(crate) dashboard: crate::agent_task::DashboardSnapshot,
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
    pub(crate) can_focus_terminal: bool,
    pub(crate) can_focus_validation: bool,
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
    FocusTerminal,
    FocusValidation,
    StopCodex,
    FinishCodex,
    RunValidation,
    Complete,
    ReviewDiff,
    Archive,
}

const ACTION_KINDS: [(ActionKind, &str); 10] = [
    (ActionKind::StartCodex, "Start Codex"),
    (ActionKind::StartTerminal, "Start terminal"),
    (ActionKind::FocusTerminal, "Agent terminal"),
    (ActionKind::FocusValidation, "Validation output"),
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
    unfiltered_state: Rc<RefCell<TasksPanelSync>>,
    dashboard_view: Rc<Cell<crate::agent_task::DashboardView>>,
    dashboard_query: Entry,
    dashboard_summary: Label,
    dashboard_clear: Button,
    dashboard_reveal: Button,
    dashboard_buttons: Vec<(crate::agent_task::DashboardView, Button)>,
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
    approval_owner: Rc<Cell<Option<TaskId>>>,
    approval_generation: Rc<Cell<u64>>,
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

        let dashboard_query = Entry::new();
        dashboard_query.set_placeholder_text(Some(
            "Search tasks, providers or repositories · Enter to open",
        ));
        dashboard_query.set_max_length(256);
        dashboard_query.update_property(&[gtk4::accessible::Property::Label("Search agent tasks")]);
        root.append(&dashboard_query);
        let filters = FlowBox::new();
        filters.set_selection_mode(gtk4::SelectionMode::None);
        filters.set_max_children_per_line(3);
        let mut dashboard_buttons = Vec::new();
        use crate::agent_task::DashboardView;
        for view in [
            DashboardView::All,
            DashboardView::NeedsAttention,
            DashboardView::Running,
            DashboardView::NotStarted,
            DashboardView::Review,
            DashboardView::Finished,
        ] {
            let button = Button::with_label(&dashboard_filter_label(view, &Default::default()));
            button.add_css_class("caption");
            filters.insert(&button, -1);
            dashboard_buttons.push((view, button));
        }
        root.append(&filters);
        let dashboard_summary = Label::new(None);
        dashboard_summary.set_wrap(true);
        dashboard_summary.add_css_class("caption");
        root.append(&dashboard_summary);
        let dashboard_clear = Button::with_label("Clear filters");
        dashboard_clear.set_visible(false);
        root.append(&dashboard_clear);
        let dashboard_reveal = Button::new();
        let reveal_label = Label::new(Some("Selected task hidden · Show selected task"));
        reveal_label.set_wrap(true);
        dashboard_reveal.set_child(Some(&reveal_label));
        dashboard_reveal.set_visible(false);
        root.append(&dashboard_reveal);

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
            unfiltered_state: Rc::new(RefCell::new(TasksPanelSync::default())),
            dashboard_view: Rc::new(Cell::new(crate::agent_task::DashboardView::All)),
            dashboard_query,
            dashboard_summary,
            dashboard_clear,
            dashboard_reveal,
            dashboard_buttons,
            follow_up_drafts: Rc::new(RefCell::new(HashMap::new())),
            rendered_rows: Rc::new(RefCell::new(Vec::new())),
            rendered_selected: Rc::new(Cell::new(None)),
            rendered_approvals: Rc::new(RefCell::new(Vec::new())),
            approval_owner: Rc::new(Cell::new(None)),
            approval_generation: Rc::new(Cell::new(0)),
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
        self.dashboard_query
            .connect_changed(move |_| panel.apply_dashboard_filters());
        let panel = self.clone();
        self.dashboard_query.connect_activate(move |_| {
            panel.apply_dashboard_filters();
            let (target, selected) = {
                let state = panel.state.borrow();
                (state.rows.first().map(|row| row.id), state.selected)
            };
            if let Some(id) = target.filter(|id| Some(*id) != selected) {
                panel.emit(TaskPanelAction::Select(id));
            }
        });
        for (view, button) in &self.dashboard_buttons {
            let panel = self.clone();
            let view = *view;
            button.connect_clicked(move |_| {
                panel.dashboard_view.set(view);
                panel.apply_dashboard_filters();
            });
        }
        for button in [&self.dashboard_clear, &self.dashboard_reveal] {
            let panel = self.clone();
            button.connect_clicked(move |_| {
                panel
                    .dashboard_view
                    .set(crate::agent_task::DashboardView::All);
                panel.dashboard_query.set_text("");
                panel.apply_dashboard_filters();
            });
        }
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
            ActionKind::FocusTerminal => TaskPanelAction::FocusTerminal(id),
            ActionKind::FocusValidation => TaskPanelAction::FocusValidation(id),
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
        *self.unfiltered_state.borrow_mut() = sync;
        self.apply_dashboard_filters();
    }

    fn apply_dashboard_filters(&self) {
        let mut sync = self.unfiltered_state.borrow().clone();
        let dashboard = crate::agent_task::DashboardSnapshot::filter_rows(
            &sync.dashboard.rows,
            self.dashboard_query.text().as_str(),
            self.dashboard_view.get(),
        );
        let mut by_id: HashMap<_, _> = sync.rows.into_iter().map(|row| (row.id, row)).collect();
        sync.rows = dashboard
            .rows
            .iter()
            .filter_map(|projected| {
                let mut row = by_id.remove(&projected.id)?;
                row.title.clone_from(&projected.title);
                row.branch.clone_from(&projected.branch);
                row.status_detail.clone_from(&projected.status_detail);
                row.needs_attention = projected.needs_attention;
                Some(row)
            })
            .collect();
        if dashboard.selected_index(sync.selected).is_none() {
            sync.detail = None;
        }
        self.dashboard_summary.set_label(&format!(
            "{} shown · {} search matches · {} need you · {} awaiting approval",
            dashboard.rows.len(),
            dashboard.counts.total,
            dashboard.counts.attention,
            dashboard.counts.pending_approval,
        ));
        self.dashboard_clear.set_visible(
            !self.dashboard_query.text().is_empty()
                || self.dashboard_view.get() != crate::agent_task::DashboardView::All,
        );
        let selected_hidden = sync.selected.is_some_and(|id| {
            self.unfiltered_state
                .borrow()
                .dashboard
                .rows
                .iter()
                .any(|row| row.id == id)
                && dashboard.selected_index(Some(id)).is_none()
        });
        self.dashboard_reveal.set_visible(selected_hidden);
        for (view, button) in &self.dashboard_buttons {
            let label = dashboard_filter_label(*view, &dashboard.counts);
            if button.label().as_deref() != Some(label.as_str()) {
                button.set_label(&label);
            }
            if *view == self.dashboard_view.get() {
                button.add_css_class("suggested-action");
            } else {
                button.remove_css_class("suggested-action");
            }
        }
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
                ActionKind::FocusTerminal => detail.can_focus_terminal,
                ActionKind::FocusValidation => detail.can_focus_validation,
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
        self.page_stack.set_visible(detail.is_some());
        self.stream_page_button.set_visible(detail.is_some());
        self.diff_page_button.set_visible(detail.is_some());
        self.follow_up_row
            .set_visible(detail.is_some_and(|d| d.can_follow_up));
        if detail.is_none() {
            self.follow_up_hint.set_visible(false);
            self.follow_up_send.set_sensitive(false);
        }

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
            self.selection_guard.set(true);
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
                if let Some(reason) = task_reason(
                    row.status,
                    row.validation_status,
                    row.status_detail.as_deref(),
                    row.validation_detail.as_deref(),
                ) {
                    let preview = gtk4::Label::new(Some(&reason));
                    preview.set_halign(gtk4::Align::Start);
                    preview.set_single_line_mode(true);
                    preview.set_ellipsize(gtk4::pango::EllipsizeMode::End);
                    preview.set_max_width_chars(28);
                    preview.set_tooltip_text(Some(&reason));
                    preview.add_css_class("caption");
                    preview.add_css_class("dim-label");
                    preview.add_css_class("task-reason");
                    lines.append(&preview);
                }
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
            self.selection_guard.set(false);
            self.rendered_selected.set(state.selected);
        }
    }

    fn approval_is_current(
        &self,
        task: TaskId,
        approval: &CodexAppServerApproval,
        generation: u64,
    ) -> bool {
        let state = self.state.borrow();
        self.root.is_mapped()
            && state.selected == Some(task)
            && state.detail.as_deref().is_some_and(|detail| {
                approval_details::is_current(
                    task,
                    approval,
                    generation,
                    detail.id,
                    &detail.approvals,
                    self.approval_generation.get(),
                )
            })
    }

    fn approval_details(
        &self,
        task: TaskId,
        approval: &CodexAppServerApproval,
        generation: u64,
    ) -> gtk4::Expander {
        let expander = gtk4::Expander::new(Some("Read request details"));
        let body = GBox::new(gtk4::Orientation::Vertical, 4);
        let view = TextView::new();
        view.set_editable(false);
        view.set_accepts_tab(false);
        view.set_cursor_visible(true);
        view.set_wrap_mode(gtk4::WrapMode::WordChar);
        view.set_monospace(true);
        view.update_property(&[gtk4::accessible::Property::Label(
            "Read-only approval request details",
        )]);
        let scroll = ScrolledWindow::new();
        scroll.set_min_content_height(180);
        scroll.set_max_content_height(260);
        scroll.set_propagate_natural_height(true);
        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scroll.set_child(Some(&view));
        body.append(&scroll);
        let nav = GBox::new(gtk4::Orientation::Horizontal, 6);
        let previous = Button::with_label("Previous page");
        let next = Button::with_label("Next page");
        let position = Label::new(None);
        position.set_wrap(true);
        position.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
        position.set_max_width_chars(30);
        let close = Button::with_label("Close details");
        for widget in [previous.clone(), next.clone(), close.clone()] {
            nav.append(&widget);
        }
        body.append(&nav);
        body.append(&position);
        expander.set_child(Some(&body));
        let pages = Rc::new(approval_details::pages(approval));
        let index = Rc::new(Cell::new(0usize));
        let render: Rc<dyn Fn()> = {
            let pages = pages.clone();
            let index = index.clone();
            let view = view.downgrade();
            let previous = previous.downgrade();
            let next = next.downgrade();
            let position = position.downgrade();
            Rc::new(move || {
                let (Some(view), Some(previous), Some(next), Some(position)) = (
                    view.upgrade(),
                    previous.upgrade(),
                    next.upgrade(),
                    position.upgrade(),
                ) else {
                    return;
                };
                view.buffer().set_text(&pages[index.get()]);
                let mut start = view.buffer().start_iter();
                view.buffer().place_cursor(&start);
                view.scroll_to_iter(&mut start, 0.0, true, 0.0, 0.0);
                view.update_property(&[gtk4::accessible::Property::Label(&format!(
                    "Read-only approval request details, page {} of {}",
                    index.get() + 1,
                    pages.len()
                ))]);
                position.set_text(&format!(
                    "Page {} of {} (other pages are not shown)",
                    index.get() + 1,
                    pages.len()
                ));
                previous.set_sensitive(index.get() > 0);
                next.set_sensitive(index.get() + 1 < pages.len());
            })
        };
        render();
        for (button, forward) in [(previous, false), (next, true)] {
            let index = index.clone();
            let pages = pages.clone();
            let render = render.clone();
            let panel = self.clone();
            let frozen = approval.clone();
            button.connect_clicked(move |_| {
                if !panel.approval_is_current(task, &frozen, generation) {
                    return;
                }
                let target = if forward {
                    index.get().saturating_add(1)
                } else {
                    index.get().saturating_sub(1)
                };
                index.set(target.min(pages.len() - 1));
                render();
            });
        }
        let weak = expander.downgrade();
        let panel = self.clone();
        let frozen = approval.clone();
        close.connect_clicked(move |_| {
            if !panel.approval_is_current(task, &frozen, generation) {
                return;
            }
            if let Some(expander) = weak.upgrade() {
                expander.set_expanded(false);
                expander.grab_focus();
            }
        });
        let keys = gtk4::EventControllerKey::new();
        let weak = expander.downgrade();
        let panel = self.clone();
        let frozen = approval.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk4::gdk::Key::Escape && panel.approval_is_current(task, &frozen, generation)
            {
                if let Some(expander) = weak.upgrade() {
                    expander.set_expanded(false);
                    expander.grab_focus();
                }
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
        body.add_controller(keys);
        expander
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
        let owner = state.detail.as_deref().map(|detail| detail.id);
        if self.approval_owner.get() == owner
            && self.rendered_approvals.borrow().as_slice() == incoming
        {
            self.approvals_box.set_visible(!incoming.is_empty());
            return;
        }
        self.approval_owner.set(owner);
        let generation = self
            .approval_generation
            .get()
            .checked_add(1)
            .expect("approval generation exhausted");
        self.approval_generation.set(generation);
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
                card.append(&self.approval_details(task_id, approval, generation));
                let buttons = GBox::new(gtk4::Orientation::Horizontal, 6);
                buttons.set_halign(gtk4::Align::End);
                buttons.set_margin_bottom(4);
                let approve = Button::with_label("Approve");
                approve.add_css_class("suggested-action");
                let deny = Button::with_label("Deny");
                deny.add_css_class("destructive-action");
                let approval_id = approval.id;
                let frozen = approval.clone();
                let panel = self.clone();
                approve.connect_clicked(move |_| {
                    if panel.approval_is_current(task_id, &frozen, generation) {
                        panel.emit(TaskPanelAction::Approve(task_id, approval_id));
                    }
                });
                let panel = self.clone();
                let frozen = approval.clone();
                deny.connect_clicked(move |_| {
                    if panel.approval_is_current(task_id, &frozen, generation) {
                        panel.emit(TaskPanelAction::Deny(task_id, approval_id));
                    }
                });
                buttons.append(&approve);
                buttons.append(&deny);
                card.append(&buttons);
                self.approvals_box.append(&card);
            }
        }
        replace_approval_cache(&mut self.rendered_approvals.borrow_mut(), incoming);
    }
}

/// Approval arrivals and decisions change the cache's length. Slice copying
/// requires equal lengths, so replace the vector at this shared boundary.
fn replace_approval_cache<T: Clone>(cache: &mut Vec<T>, incoming: &[T]) {
    *cache = incoming.to_vec();
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

#[cfg(test)]
mod tests {
    #[test]
    fn dashboard_filter_labels_map_activity_and_finished_counts() {
        use crate::agent_task::{DashboardCounts, DashboardView};
        let counts = DashboardCounts {
            total: 20,
            attention: 7,
            running: 9,
            created: 3,
            review: 4,
            done: 2,
            failed: 5,
            cancelled: 1,
            archived: 99,
            ..Default::default()
        };
        for (view, expected) in [
            (DashboardView::All, "All 20"),
            (DashboardView::NeedsAttention, "Needs you 7"),
            (DashboardView::Running, "Running 9"),
            (DashboardView::NotStarted, "Not started 3"),
            (DashboardView::Review, "Review 4"),
            (DashboardView::Finished, "Finished 8"),
        ] {
            assert_eq!(super::dashboard_filter_label(view, &counts), expected);
        }
        assert_eq!(
            super::dashboard_filter_label(DashboardView::Running, &DashboardCounts::default()),
            "Running 0"
        );
    }

    use super::*;

    use super::replace_approval_cache;

    #[test]
    fn approval_cache_accepts_arrivals_and_removals() {
        let mut cache = Vec::<u64>::new();
        for incoming in [vec![], vec![1], vec![1, 2], vec![], vec![3], vec![]] {
            replace_approval_cache(&mut cache, &incoming);
            assert_eq!(cache, incoming);
        }
    }

    #[test]
    fn approval_cache_replaces_equal_length_and_unchanged_sets() {
        let mut cache = vec![1, 2];
        let incoming = vec![3, 4];
        replace_approval_cache(&mut cache, &incoming);
        replace_approval_cache(&mut cache, &incoming);
        assert_eq!(cache, incoming);
    }
    fn dashboard_test_sync() -> TasksPanelSync {
        use crate::agent_task::{AgentProvider, NewTask, TaskManager, TaskStatus};
        let mut manager = TaskManager::new();
        for title in ["First task", "Needs review"] {
            manager
                .create(NewTask {
                    title: title.into(),
                    provider: AgentProvider::Codex,
                    repo_root: "/fixture".into(),
                    worktree_path: "/fixture/worktree".into(),
                    branch: "test/dashboard".into(),
                    base_commit: "a".repeat(40),
                    source_context: None,
                })
                .unwrap();
        }
        let mut tasks = manager.tasks().to_vec();
        tasks[1].status = TaskStatus::Failed;
        let selected = tasks[0].id;
        let dashboard = crate::agent_task::DashboardSnapshot::project(
            &tasks,
            |_| Default::default(),
            Default::default(),
        );
        let rows = tasks
            .iter()
            .map(|task| TaskRowSnapshot {
                id: task.id,
                title: task.title.clone(),
                provider: task.provider,
                status: task.status,
                runtime_kind: task.runtime_kind,
                branch: task.branch.clone(),
                has_agent_terminal: false,
                has_validation_terminal: false,
                has_active_agent_stream: false,
                native_preparing: false,
                validation_status: task.validation.status,
                validation_attempt: 0,
                needs_attention: task.needs_attention(),
                status_detail: None,
                validation_detail: None,
            })
            .collect();
        let detail = TaskDetailSync {
            id: selected,
            title: "First task".into(),
            status_line: "Created".into(),
            branch: "test/dashboard".into(),
            stream: None,
            approvals: Vec::new(),
            completed_turns: 0,
            can_start_codex: false,
            can_start_terminal: false,
            can_focus_terminal: false,
            can_focus_validation: false,
            can_stop: false,
            can_finish: false,
            can_run_validation: false,
            can_complete: false,
            can_follow_up: false,
            follow_up_hint: String::new(),
            diff: None,
        };
        TasksPanelSync {
            rows,
            dashboard,
            selected: Some(selected),
            detail: Some(Box::new(detail)),
            ..Default::default()
        }
    }

    fn descendants(root: &impl IsA<gtk4::Widget>) -> Vec<gtk4::Widget> {
        let mut result = vec![root.as_ref().clone()];
        let mut index = 0;
        while index < result.len() {
            let mut child = result[index].first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                result.push(widget);
            }
            index += 1;
        }
        result
    }

    #[test]
    #[ignore = "requires a native GTK display"]
    fn approval_details_are_paged_read_only_and_stale_controls_cannot_decide() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let window = gtk4::Window::new();
        window.set_child(Some(&panel.root));
        window.present();
        while gtk4::glib::MainContext::default().pending() {
            gtk4::glib::MainContext::default().iteration(false);
        }
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let mut sync = dashboard_test_sync();
        let task = sync.selected.unwrap();
        let approval = crate::agent_task::CodexAppServerApproval {
            id: crate::agent_task::ApprovalId::new(),
            kind: crate::agent_task::CodexAppServerApprovalKind::Command,
            item_id: "request-1".into(),
            command: Some(format!("{} <b>END</b>", "x".repeat(2000))),
            cwd: Some("/tmp/project".into()),
            reason: Some("r".repeat(4096)),
            file_paths: vec![],
            file_changes: vec![],
        };
        let summary = approval_summary(&approval);
        assert!(summary.len() <= 256);
        assert!(summary.ends_with('…'));
        sync.detail.as_mut().unwrap().approvals = vec![approval.clone()];
        panel.sync(sync.clone());
        let expander = descendants(&panel.approvals_box)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk4::Expander>().ok())
            .unwrap();
        // A collapsed GtkExpander keeps its content detached from the widget tree.
        expander.set_expanded(true);
        while gtk4::glib::MainContext::default().pending() {
            gtk4::glib::MainContext::default().iteration(false);
        }
        let widgets = descendants(&panel.approvals_box);
        let button = |label: &str| {
            widgets
                .iter()
                .filter_map(|widget| widget.clone().downcast::<Button>().ok())
                .find(|button| button.label().as_deref() == Some(label))
                .unwrap()
        };
        let approve = button("Approve");
        let deny = button("Deny");
        let next = button("Next page");
        let previous = button("Previous page");
        let close = button("Close details");
        let view = widgets
            .iter()
            .find_map(|widget| widget.clone().downcast::<TextView>().ok())
            .unwrap();
        assert!(!view.is_editable());
        assert!(!view.accepts_tab());
        assert!(view.is_focusable());
        fn assert_page_label(view: &TextView, expected: &str) {
            use gtk4::glib::translate::{from_glib_full, IntoGlib, ToGlibPtr};
            let expected = std::ffi::CString::new(expected).unwrap();
            // GTK's test API returns null when the string property matches.
            let mismatch: Option<gtk4::glib::GString> = unsafe {
                from_glib_full(gtk4::ffi::gtk_test_accessible_check_property(
                    view.upcast_ref::<gtk4::Accessible>().to_glib_none().0,
                    gtk4::AccessibleProperty::Label.into_glib(),
                    expected.as_ptr(),
                ))
            };
            assert!(
                mismatch.is_none(),
                "Accessible label mismatch: {mismatch:?}"
            );
        }
        assert_page_label(&view, "Read-only approval request details, page 1 of 2");
        let body = expander.child().unwrap();
        assert!(descendants(&body)
            .iter()
            .filter_map(|widget| widget.clone().downcast::<Button>().ok())
            .all(|button| !matches!(button.label().as_deref(), Some("Approve" | "Deny"))));

        assert!(view
            .buffer()
            .text(
                &view.buffer().start_iter(),
                &view.buffer().end_iter(),
                false
            )
            .contains("<b>END</b>"));
        assert!(next.is_sensitive());
        assert!(!previous.is_sensitive());
        assert!(next.grab_focus());
        let focus = gtk4::prelude::GtkWindowExt::focus(&window);
        panel.sync(sync.clone());
        assert_eq!(gtk4::prelude::GtkWindowExt::focus(&window), focus);
        assert!(expander.is_expanded());
        assert!(view.grab_focus());
        // The standard text-view keyboard action remains usable while read-only.
        view.emit_by_name::<()>(
            "move-cursor",
            &[&gtk4::MovementStep::BufferEnds, &1i32, &false],
        );
        assert_eq!(view.buffer().cursor_position(), view.buffer().char_count());
        assert!(body.child_focus(gtk4::DirectionType::TabForward));
        assert!(
            next.has_focus(),
            "Tab leaves the read-only text and reaches page navigation"
        );
        assert!(gtk4::prelude::WidgetExt::activate(&next));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !previous.is_sensitive() && std::time::Instant::now() < deadline {
            gtk4::glib::MainContext::default().iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(view.buffer().cursor_position(), 0);
        assert_page_label(&view, "Read-only approval request details, page 2 of 2");
        assert!(close.grab_focus());
        assert!(body.child_focus(gtk4::DirectionType::TabBackward));
        assert!(
            previous.has_focus(),
            "Shift-Tab reaches the previous page button"
        );
        assert!(previous.is_sensitive());
        assert!(!next.is_sensitive());
        previous.emit_clicked();
        assert!(!previous.is_sensitive());
        let controllers = body.observe_controllers();
        let keys = (0..controllers.n_items())
            .find_map(|index| {
                controllers
                    .item(index)
                    .unwrap()
                    .downcast::<gtk4::EventControllerKey>()
                    .ok()
            })
            .unwrap();
        let propagation = keys.emit_by_name::<bool>(
            "key-pressed",
            &[
                &gtk4::gdk::Key::Escape,
                &0u32,
                &gtk4::gdk::ModifierType::empty(),
            ],
        );
        assert!(propagation);
        assert!(!expander.is_expanded());
        expander.set_expanded(true);
        close.emit_clicked();
        assert!(!expander.is_expanded());
        panel.root.set_visible(false);
        approve.emit_clicked();
        assert!(actions.borrow().is_empty());
        panel.root.set_visible(true);

        // Same task and same approval remain actionable after read-only navigation.
        approve.emit_clicked();
        assert!(
            matches!(actions.borrow().last(), Some(TaskPanelAction::Approve(id, request)) if *id == task && *request == approval.id)
        );
        actions.borrow_mut().clear();
        // Same task/request ID, changed frozen content invalidates old callbacks.
        sync.detail.as_mut().unwrap().approvals[0].reason = Some("replaced".into());
        panel.sync(sync.clone());
        approve.emit_clicked();
        deny.emit_clicked();
        assert!(actions.borrow().is_empty());
        // ABA: restore identical request; the first generation still cannot decide.
        sync.detail.as_mut().unwrap().approvals = vec![approval.clone()];
        panel.sync(sync.clone());
        approve.emit_clicked();
        assert!(actions.borrow().is_empty());
        let current = descendants(&panel.approvals_box)
            .into_iter()
            .filter_map(|widget| widget.downcast::<Button>().ok())
            .find(|button| button.label().as_deref() == Some("Approve"))
            .unwrap();
        let retired_body = descendants(&panel.approvals_box)
            .into_iter()
            .find_map(|widget| widget.downcast::<gtk4::Expander>().ok())
            .unwrap()
            .child()
            .unwrap();
        let retired_view = descendants(&retired_body)
            .into_iter()
            .find_map(|widget| widget.downcast::<TextView>().ok())
            .unwrap()
            .downgrade();
        drop(retired_body);
        // Switching task with identical approval values also invalidates old controls.
        let other = sync.rows.iter().find(|row| row.id != task).unwrap().id;
        sync.selected = Some(other);
        sync.detail.as_mut().unwrap().id = other;
        panel.sync(sync.clone());
        current.emit_clicked();
        assert!(actions.borrow().is_empty());
        assert!(
            retired_view.upgrade().is_none(),
            "removed details must not be retained by signal cycles"
        );
        sync.detail.as_mut().unwrap().approvals.clear();
        panel.sync(sync);
        current.emit_clicked();
        next.emit_clicked();
        assert!(actions.borrow().is_empty());
        window.close();
    }

    #[test]
    #[ignore = "requires a native GTK display"]
    fn task_reason_updates_and_disappears_without_retargeting() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let find_reason = || {
            let mut pending = vec![panel.task_list.clone().upcast::<gtk4::Widget>()];
            while let Some(widget) = pending.pop() {
                if widget.has_css_class("task-reason") {
                    return Some(widget.downcast::<gtk4::Label>().unwrap());
                }
                let mut child = widget.first_child();
                while let Some(widget) = child {
                    child = widget.next_sibling();
                    pending.push(widget);
                }
            }
            None
        };
        let mut sync = dashboard_test_sync();
        let selected = sync.selected.unwrap();
        let row = sync.rows.iter_mut().find(|row| row.id == selected).unwrap();
        row.status = crate::agent_task::TaskStatus::Failed;
        row.status_detail = Some("task reason".into());
        row.validation_status = crate::agent_task::TaskValidationStatus::Failed;
        row.validation_detail = Some("validation old".into());
        let row = sync
            .dashboard
            .rows
            .iter_mut()
            .find(|row| row.id == selected)
            .unwrap();
        row.status = crate::agent_task::TaskStatus::Failed;
        row.status_detail = Some("task reason".into());
        row.validation_status = crate::agent_task::TaskValidationStatus::Failed;
        panel.sync(sync.clone());
        let reason = find_reason().unwrap();
        assert_eq!(reason.label(), "validation old");
        assert_eq!(reason.tooltip_text().as_deref(), Some("validation old"));
        assert_eq!(reason.ellipsize(), gtk4::pango::EllipsizeMode::End);
        assert!(reason.is_single_line_mode());
        sync.rows
            .iter_mut()
            .find(|row| row.id == selected)
            .unwrap()
            .validation_detail = Some("updated".into());
        panel.sync(sync.clone());
        assert_eq!(find_reason().unwrap().label(), "updated");
        sync.rows
            .iter_mut()
            .find(|row| row.id == selected)
            .unwrap()
            .validation_detail = None;
        sync.rows
            .iter_mut()
            .find(|row| row.id == selected)
            .unwrap()
            .status_detail = None;
        sync.dashboard
            .rows
            .iter_mut()
            .find(|row| row.id == selected)
            .unwrap()
            .status_detail = None;
        panel.sync(sync);
        assert!(
            find_reason().is_none(),
            "old preview and tooltip must disappear"
        );
        assert_eq!(panel.state.borrow().selected, Some(selected));
        assert!(actions.borrow().is_empty());
    }

    #[test]
    #[ignore = "requires a native GTK display"]
    fn task_terminal_buttons_only_emit_focus_for_existing_bindings() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let mut sync = dashboard_test_sync();
        let id = sync.selected.unwrap();
        let detail = sync.detail.as_mut().unwrap();
        detail.can_focus_terminal = true;
        detail.can_focus_validation = true;
        panel.sync(sync.clone());
        for kind in [ActionKind::FocusTerminal, ActionKind::FocusValidation] {
            let button = panel
                .action_buttons
                .borrow()
                .iter()
                .find(|(candidate, _)| *candidate == kind)
                .unwrap()
                .1
                .clone();
            assert!(button.is_sensitive());
            button.emit_clicked();
        }
        assert_eq!(
            *actions.borrow(),
            [
                TaskPanelAction::FocusTerminal(id),
                TaskPanelAction::FocusValidation(id)
            ]
        );
        assert_eq!(panel.state.borrow().selected, Some(id));
        assert_eq!(panel.unfiltered_state.borrow().rows, sync.rows);
        sync.detail.as_mut().unwrap().can_focus_terminal = false;
        sync.detail.as_mut().unwrap().can_focus_validation = false;
        panel.sync(sync);
        assert!(panel
            .action_buttons
            .borrow()
            .iter()
            .filter(|(kind, _)| matches!(
                kind,
                ActionKind::FocusTerminal | ActionKind::FocusValidation
            ))
            .all(|(_, button)| !button.is_sensitive()));
    }

    #[test]
    #[ignore = "requires a native GTK display"]
    fn dashboard_filter_counts_follow_search_not_selected_view() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let sync = dashboard_test_sync();
        let selected = sync.selected;
        panel.sync(sync.clone());
        let labels = || {
            panel
                .dashboard_buttons
                .iter()
                .map(|(_, button)| button.label().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(),
            [
                "All 2",
                "Needs you 1",
                "Running 0",
                "Not started 1",
                "Review 0",
                "Finished 1"
            ]
        );
        panel
            .dashboard_view
            .set(crate::agent_task::DashboardView::Running);
        panel.apply_dashboard_filters();
        assert!(panel.state.borrow().rows.is_empty());
        assert_eq!(
            labels()[0],
            "All 2",
            "choosing a view must not shrink badges"
        );
        panel.dashboard_query.set_text("First");
        assert_eq!(
            labels(),
            [
                "All 1",
                "Needs you 0",
                "Running 0",
                "Not started 1",
                "Review 0",
                "Finished 0"
            ]
        );
        panel.dashboard_query.set_text("missing");
        assert!(labels().iter().all(|label| label.ends_with(" 0")));
        assert!(panel
            .dashboard_buttons
            .iter()
            .all(|(_, button)| button.is_sensitive()));
        assert_eq!(panel.state.borrow().selected, selected);
        assert_eq!(panel.unfiltered_state.borrow().rows, sync.rows);
        assert!(actions.borrow().is_empty());
    }

    #[test]
    #[ignore = "requires a native GTK display"]
    fn dashboard_search_activate_selects_current_visible_result_only() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let sync = dashboard_test_sync();
        let expected = sync
            .dashboard
            .rows
            .iter()
            .find(|row| row.title == "Needs review")
            .unwrap()
            .id;
        panel.sync(sync.clone());
        panel.dashboard_query.set_text("review");
        assert!(actions.borrow().is_empty(), "typing must not select");
        panel
            .dashboard_view
            .set(crate::agent_task::DashboardView::Running);
        panel.dashboard_query.emit_activate();
        assert!(
            actions.borrow().is_empty(),
            "the current view can hide all matches"
        );
        panel
            .dashboard_view
            .set(crate::agent_task::DashboardView::All);
        panel.dashboard_query.emit_activate();
        assert!(
            matches!(actions.borrow().as_slice(), [TaskPanelAction::Select(id)] if *id == expected)
        );
        assert_eq!(panel.unfiltered_state.borrow().rows, sync.rows);
        panel.dashboard_query.set_text("no matches");
        panel.dashboard_query.emit_activate();
        assert_eq!(actions.borrow().len(), 1, "empty results are inert");
    }

    #[test]
    #[ignore = "requires a native GTK display"]
    fn dashboard_filters_preserve_selection_without_actions() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let window = gtk4::Window::new();
        window.set_child(Some(&panel.root));
        // is_visible includes ancestor visibility, so exercise a shown window.
        window.present();
        let sync = dashboard_test_sync();
        let selected = sync.selected;
        panel.sync(sync.clone());
        assert_eq!(panel.rendered_rows.borrow().len(), 2);
        panel
            .dashboard_view
            .set(crate::agent_task::DashboardView::NeedsAttention);
        panel.apply_dashboard_filters();
        assert_eq!(panel.state.borrow().selected, selected);
        assert_eq!(panel.rendered_rows.borrow().len(), 1);
        assert!(panel.state.borrow().detail.is_none());
        assert!(panel.dashboard_clear.is_visible());
        assert!(panel.dashboard_reveal.is_visible());
        assert!(!panel.page_stack.is_visible());
        panel.dashboard_query.set_text("missing");
        assert!(panel.rendered_rows.borrow().is_empty());
        panel.dashboard_reveal.emit_clicked();
        assert!(!panel.dashboard_reveal.is_visible());
        assert!(!panel.dashboard_clear.is_visible());
        assert_eq!(panel.rendered_rows.borrow().len(), 2);
        assert_eq!(panel.state.borrow().selected, selected);
        assert_eq!(panel.state.borrow().detail.as_ref().map(|d| d.id), selected);
        assert!(panel.page_stack.is_visible());
        panel.sync(sync.clone());
        assert_eq!(panel.rendered_selected.get(), selected);
        let mut removed = sync;
        removed.rows.retain(|row| Some(row.id) != selected);
        removed
            .dashboard
            .rows
            .retain(|row| Some(row.id) != selected);
        removed.detail = None;
        panel.sync(removed);
        assert!(!panel.dashboard_reveal.is_visible());
        assert!(
            actions.borrow().is_empty(),
            "filtering must not stage task actions"
        );
        window.close();
    }
}
