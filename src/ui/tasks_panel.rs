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
#[path = "task_filters.rs"]
mod task_filters;
use crate::agent_task_ui::{
    approval_summary, native_follow_up_can_send, plan_list_refresh, render_stream_text,
    row_status_line, task_reason, FollowUpReceipts, TaskFollowUpRequest, TaskPanelAction,
    TaskRowSnapshot,
};
use task_filters::{TaskFilters, STATES};

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
const INSPECTOR_PAGE: &str = "details";
const CREATE_TASK_LABEL: &str = "New agent task from selected block";
const CLOSE_TASKS_LABEL: &str = "Close Tasks panel";

/// Full panel state pushed by the application after every domain change.
#[derive(Clone, Debug, Default)]
pub(crate) struct TasksPanelSync {
    pub(crate) rows: Vec<TaskRowSnapshot>,
    pub(crate) dashboard: crate::agent_task::DashboardSnapshot,
    pub(crate) workspaces: HashMap<TaskId, std::path::PathBuf>,
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
    pub(crate) inspector: String,
    pub(crate) guidance: crate::agent_task::TaskGuidance,
    pub(crate) validation_status: crate::agent_task::TaskValidationStatus,
    pub(crate) native_label: String,
    pub(crate) manual_review: bool,
    pub(crate) can_accept_review: bool,
    pub(crate) stream: Option<Box<crate::agent_task::CodexAppServerViewSnapshot>>,
    pub(crate) approvals: Vec<crate::agent_task::CodexAppServerApproval>,
    pub(crate) completed_turns: usize,
    pub(crate) can_start_native: bool,
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
    StartNative,
    StartTerminal,
    FocusTerminal,
    FocusValidation,
    StopCodex,
    FinishCodex,
    RunValidation,
    Complete,
    AcceptReviewed,
    ReviewDiff,
    Archive,
}

/// Choose a visible next operation only from the adapter's runtime gates.
/// Guidance text never grants permission or approves a pending request.
fn primary_task_action(detail: &TaskDetailSync) -> Option<(ActionKind, String)> {
    let (kind, label) = if !detail.approvals.is_empty() {
        return None;
    } else if detail.can_finish {
        (ActionKind::FinishCodex, "Finish session")
    } else if detail.can_focus_validation
        && (detail.validation_status.needs_attention()
            || detail.validation_status == crate::agent_task::TaskValidationStatus::Running)
    {
        (ActionKind::FocusValidation, "Open validation output")
    } else if detail.can_complete {
        (ActionKind::Complete, "Mark complete")
    } else if detail.can_accept_review {
        (ActionKind::ReviewDiff, "Review changes")
    } else if detail.can_run_validation {
        (ActionKind::RunValidation, "Run validation")
    } else if detail.can_start_native {
        return Some((ActionKind::StartNative, detail.native_label.clone()));
    } else if detail.can_start_terminal {
        (ActionKind::StartTerminal, "Start terminal")
    } else if detail.can_stop {
        return detail
            .can_focus_terminal
            .then(|| (ActionKind::FocusTerminal, "Open agent terminal".into()));
    } else if detail.can_focus_validation {
        (ActionKind::FocusValidation, "Open validation output")
    } else if detail.can_focus_terminal {
        (ActionKind::FocusTerminal, "Open agent terminal")
    } else {
        (ActionKind::ReviewDiff, "Review changes")
    };
    Some((kind, label.to_string()))
}

const ACTION_KINDS: [(ActionKind, &str); 11] = [
    (ActionKind::StartNative, "Start provider"),
    (ActionKind::StartTerminal, "Start terminal"),
    (ActionKind::FocusTerminal, "Agent terminal"),
    (ActionKind::FocusValidation, "Validation output"),
    (ActionKind::StopCodex, "Stop"),
    (ActionKind::FinishCodex, "Finish"),
    (ActionKind::RunValidation, "Validate"),
    (ActionKind::Complete, "Complete"),
    (ActionKind::AcceptReviewed, "Accept reviewed work"),
    (ActionKind::ReviewDiff, "Diff"),
    (ActionKind::Archive, "Archive"),
];

type ActionCallback = Rc<RefCell<Box<dyn Fn(TaskPanelAction)>>>;

/// Cached by stable task identity, including currently filtered-out live rows.
#[derive(Clone)]
struct TaskRowWidgets {
    row: gtk4::ListBoxRow,
    title: Label,
    status: Label,
    reason: Label,
    snapshot: TaskRowSnapshot,
}

impl TaskRowWidgets {
    fn new(snapshot: &TaskRowSnapshot) -> Self {
        let title = Label::new(None);
        title.set_max_width_chars(28);
        let status = Label::new(None);
        status.add_css_class("dim-label");
        status.add_css_class("caption");
        let reason = Label::new(None);
        reason.set_single_line_mode(true);
        reason.set_max_width_chars(28);
        reason.add_css_class("caption");
        reason.add_css_class("dim-label");
        reason.add_css_class("task-reason");
        let lines = GBox::new(gtk4::Orientation::Vertical, 2);
        lines.set_margin_top(4);
        lines.set_margin_bottom(4);
        lines.set_margin_start(4);
        for label in [&title, &status, &reason] {
            label.set_halign(gtk4::Align::Start);
            label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            lines.append(label);
        }
        let row = gtk4::ListBoxRow::new();
        row.set_child(Some(&lines));
        let mut widgets = Self {
            row,
            title,
            status,
            reason,
            snapshot: snapshot.clone(),
        };
        widgets.update(snapshot);
        widgets
    }

    fn update(&mut self, snapshot: &TaskRowSnapshot) {
        self.title.set_label(&snapshot.title);
        self.status.set_label(&row_status_line(snapshot));
        if snapshot.needs_attention {
            self.status.add_css_class("warning");
        } else {
            self.status.remove_css_class("warning");
        }
        let reason = task_reason(
            snapshot.status,
            snapshot.validation_status,
            snapshot.status_detail.as_deref(),
            snapshot.validation_detail.as_deref(),
        );
        self.reason.set_visible(reason.is_some());
        // Existing diagnostics identify present previews by this class.
        if reason.is_some() {
            self.reason.add_css_class("task-reason");
        } else {
            self.reason.remove_css_class("task-reason");
        }
        self.reason.set_label(reason.as_deref().unwrap_or_default());
        self.reason.set_tooltip_text(reason.as_deref());
        self.snapshot.clone_from(snapshot);
    }
}

/// The right-side agent Tasks panel. Cheap to clone; every clone shares the
/// same widgets and staged state, like the other forge panel handles.
#[derive(Clone)]
pub(crate) struct TasksPanel {
    pub(crate) root: GBox,
    state: Rc<RefCell<TasksPanelSync>>,
    unfiltered_state: Rc<RefCell<TasksPanelSync>>,
    dashboard_view: Rc<Cell<crate::agent_task::DashboardView>>,
    attention_kind: Rc<Cell<Option<crate::agent_task::DashboardAttentionKind>>>,
    attention_filter: gtk4::DropDown,
    dashboard_query_error: Label,
    dashboard_sort: gtk4::DropDown,
    next_attention: Button,
    previous_attention: Button,
    dashboard_focus_project: Button,
    dashboard_query: Entry,
    filters: Rc<RefCell<TaskFilters>>,
    scope_filters: gtk4::Expander,
    provider_filter: gtk4::DropDown,
    workspace_filter: gtk4::DropDown,
    status_filter: gtk4::DropDown,
    workspace_choices: Rc<RefCell<Vec<std::path::PathBuf>>>,
    filter_guard: Rc<Cell<bool>>,
    row_widgets: Rc<RefCell<HashMap<TaskId, TaskRowWidgets>>>,
    dashboard_summary: Label,
    dashboard_clear: Button,
    dashboard_reveal: Button,
    dashboard_buttons: Vec<(crate::agent_task::DashboardView, Button)>,
    /// Per-task follow-up drafts; the app never sees keystrokes, only the
    /// final text carried by `TaskPanelAction::FollowUp`.
    follow_up_drafts: Rc<RefCell<HashMap<TaskId, String>>>,
    follow_up_receipts: Rc<RefCell<FollowUpReceipts>>,
    follow_up_guard: Rc<Cell<bool>>,
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
    compose_button: Button,
    close_button: Button,
    create_hint: Label,
    task_list: ListBox,
    detail_title: Label,
    detail_status: Label,
    actions_box: FlowBox,
    actions_expander: gtk4::Expander,
    detail_scroll: ScrolledWindow,
    action_buttons: Rc<RefCell<Vec<(ActionKind, Button)>>>,
    action_owner: Rc<Cell<Option<TaskId>>>,
    primary_target: Rc<Cell<Option<(TaskId, ActionKind)>>>,
    primary_action: Button,
    next_step_card: GBox,
    next_step_title: Label,
    next_step_detail: Label,
    next_step_reason: Label,
    approvals_box: GBox,
    stream_page_button: ToggleButton,
    diff_page_button: ToggleButton,
    inspector_page_button: Button,
    inspector_view: TextView,
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
        let compose_button = Button::with_label("New task");
        compose_button.add_css_class("suggested-action");
        compose_button.set_tooltip_text(Some("Create a project task from your own goal"));
        header.append(&compose_button);
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
        dashboard_query.set_placeholder_text(Some("Search tasks · provider:codex status:review"));
        dashboard_query.set_max_length(4096);
        dashboard_query.update_property(&[gtk4::accessible::Property::Label("Search agent tasks")]);
        root.append(&dashboard_query);
        let dashboard_query_error = Label::new(None);
        dashboard_query_error.set_xalign(0.0);
        dashboard_query_error.set_wrap(true);
        dashboard_query_error.add_css_class("error");
        dashboard_query_error.add_css_class("caption");
        dashboard_query_error.set_visible(false);
        root.append(&dashboard_query_error);
        let search_help = gtk4::Expander::new(Some("Search syntax"));
        let help_label = Label::new(Some("Words combine with AND. Quote phrases and use - to exclude.\nprovider:codex status:working,review\nattention:approval validation:failed\nproject:repo branch:feature -runtime:terminal\nOther fields: id:, is:. Filters and views stay active."));
        help_label.set_xalign(0.0);
        help_label.set_wrap(true);
        help_label.set_selectable(true);
        help_label.set_max_width_chars(36);
        help_label.add_css_class("caption");
        help_label.set_margin_top(6);
        help_label.set_margin_bottom(6);
        search_help.set_child(Some(&help_label));
        root.append(&search_help);
        let provider_labels = std::iter::once("All providers")
            .chain(
                crate::agent_task::AgentProvider::ALL
                    .iter()
                    .map(|provider| provider.display_name()),
            )
            .collect::<Vec<_>>();
        let provider_filter = gtk4::DropDown::from_strings(&provider_labels);
        let workspace_filter = gtk4::DropDown::from_strings(&["All workspaces"]);
        let state_labels = std::iter::once("All states")
            .chain(STATES.iter().map(|status| status.label()))
            .collect::<Vec<_>>();
        let status_filter = gtk4::DropDown::from_strings(&state_labels);
        let attention_filter = gtk4::DropDown::from_strings(&["All attention reasons"]);
        attention_filter.set_tooltip_text(Some("Focus approvals, questions, failed validations, review or failed tasks without replacing your search"));
        let factory = gtk4::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
            let label = Label::new(None);
            label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
            label.set_max_width_chars(24);
            label.set_halign(gtk4::Align::Start);
            item.set_child(Some(&label));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk4::ListItem>().unwrap();
            let value = item
                .item()
                .unwrap()
                .downcast::<gtk4::StringObject>()
                .unwrap();
            let label = item.child().unwrap().downcast::<Label>().unwrap();
            label.set_label(&value.string());
            label.set_tooltip_text(Some(&value.string()));
        });
        workspace_filter.set_factory(Some(&factory));
        workspace_filter.set_list_factory(Some(&factory));
        workspace_filter.set_enable_search(true);
        let scope_filters = gtk4::Expander::new(Some("Provider, project and state"));
        let advanced_filters = GBox::new(gtk4::Orientation::Vertical, 4);
        for (label, control) in [
            ("Provider", &provider_filter),
            ("Workspace", &workspace_filter),
            ("State", &status_filter),
        ] {
            control.update_property(&[gtk4::accessible::Property::Label(label)]);
            control.set_hexpand(true);
            let line = GBox::new(gtk4::Orientation::Horizontal, 4);
            let caption = Label::new(Some(label));
            caption.set_width_chars(9);
            caption.set_halign(gtk4::Align::Start);
            line.append(&caption);
            line.append(control);
            advanced_filters.append(&line);
        }
        scope_filters.set_child(Some(&advanced_filters));
        root.append(&scope_filters);
        attention_filter
            .update_property(&[gtk4::accessible::Property::Label("Task attention reason")]);
        root.append(&attention_filter);
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
        let dashboard_controls = GBox::new(gtk4::Orientation::Vertical, 6);
        let dashboard_sort = gtk4::DropDown::from_strings(
            &crate::agent_task::DashboardSort::ALL.map(|sort| sort.label()),
        );
        dashboard_sort.set_hexpand(true);
        dashboard_sort.set_tooltip_text(Some("Order visible tasks"));
        dashboard_sort.update_property(&[gtk4::accessible::Property::Label("Task sort order")]);
        let previous_attention = Button::with_label("Previous needs you");
        previous_attention
            .set_tooltip_text(Some("Select the previous visible task needing attention"));
        let next_attention = Button::with_label("Next needs you");
        next_attention.set_tooltip_text(Some("Select the next visible task needing attention"));
        dashboard_controls.append(&dashboard_sort);
        let attention_navigation = GBox::new(gtk4::Orientation::Horizontal, 6);
        attention_navigation.append(&previous_attention);
        attention_navigation.append(&next_attention);
        dashboard_controls.append(&attention_navigation);
        root.append(&dashboard_controls);
        let dashboard_focus_project = Button::with_label("Selected project");
        dashboard_focus_project.set_tooltip_text(Some(
            "Narrow to the selected task's exact project; keep the current search and provider",
        ));
        root.append(&dashboard_focus_project);
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
        let detail_scroll = ScrolledWindow::new();
        detail_scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        detail_scroll.set_child(Some(&detail_box));
        paned.set_end_child(Some(&detail_scroll));

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

        let next_step_card = GBox::new(gtk4::Orientation::Vertical, 4);
        next_step_card.set_margin_top(4);
        next_step_card.set_margin_bottom(4);
        next_step_card.add_css_class("card");
        next_step_card.set_visible(false);
        let next_step_title = Label::new(None);
        next_step_title.set_halign(gtk4::Align::Start);
        next_step_title.set_margin_top(6);
        next_step_title.set_wrap(true);
        next_step_title.add_css_class("heading");
        let next_step_detail = Label::new(None);
        let next_step_reason = Label::new(None);
        for label in [&next_step_title, &next_step_detail, &next_step_reason] {
            label.set_margin_start(8);
            label.set_margin_end(8);
            label.set_xalign(0.0);
            label.set_wrap(true);
            label.set_max_width_chars(36);
            next_step_card.append(label);
        }
        next_step_detail.add_css_class("caption");
        next_step_detail.set_lines(3);
        next_step_detail.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        next_step_reason.add_css_class("caption");
        next_step_reason.add_css_class("warning");
        next_step_reason.set_lines(3);
        next_step_reason.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        let primary_action = Button::new();
        primary_action.set_halign(gtk4::Align::Start);
        primary_action.set_margin_top(6);
        primary_action.set_margin_bottom(6);
        primary_action.set_margin_start(6);
        primary_action.set_margin_end(6);
        primary_action.add_css_class("suggested-action");
        primary_action
            .update_property(&[gtk4::accessible::Property::Label("Recommended task action")]);
        next_step_card.append(&primary_action);
        detail_box.append(&next_step_card);

        let actions_box = FlowBox::new();
        actions_box.set_selection_mode(gtk4::SelectionMode::None);
        actions_box.set_column_spacing(4);
        actions_box.set_row_spacing(4);
        actions_box.set_max_children_per_line(4);
        let actions_expander = gtk4::Expander::new(Some("All task actions"));
        actions_expander.set_child(Some(&actions_box));
        actions_expander.set_visible(false);
        detail_box.append(&actions_expander);

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
        let inspector_page_button = Button::with_label("Details");
        inspector_page_button.add_css_class("flat");
        inspector_page_button.add_css_class("caption");
        page_buttons.append(&inspector_page_button);
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

        let inspector_view = TextView::new();
        inspector_view.set_editable(false);
        inspector_view.set_cursor_visible(false);
        inspector_view.set_wrap_mode(gtk4::WrapMode::WordChar);
        inspector_view.set_left_margin(6);
        inspector_view.set_right_margin(6);
        inspector_view.set_top_margin(8);
        inspector_view.set_bottom_margin(8);
        let inspector_scroll = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .child(&inspector_view)
            .build();
        page_stack.add_named(&inspector_scroll, Some(INSPECTOR_PAGE));

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
            attention_kind: Rc::new(Cell::new(None)),
            attention_filter,
            dashboard_query_error,
            dashboard_sort,
            next_attention,
            previous_attention,
            dashboard_focus_project,
            dashboard_query,
            filters: Rc::new(RefCell::new(TaskFilters::default())),
            scope_filters,
            provider_filter,
            workspace_filter,
            status_filter,
            workspace_choices: Rc::new(RefCell::new(Vec::new())),
            filter_guard: Rc::new(Cell::new(false)),
            row_widgets: Rc::new(RefCell::new(HashMap::new())),
            dashboard_summary,
            dashboard_clear,
            dashboard_reveal,
            dashboard_buttons,
            follow_up_drafts: Rc::new(RefCell::new(HashMap::new())),
            follow_up_receipts: Rc::new(RefCell::new(FollowUpReceipts::default())),
            follow_up_guard: Rc::new(Cell::new(false)),
            rendered_rows: Rc::new(RefCell::new(Vec::new())),
            rendered_selected: Rc::new(Cell::new(None)),
            rendered_approvals: Rc::new(RefCell::new(Vec::new())),
            approval_owner: Rc::new(Cell::new(None)),
            approval_generation: Rc::new(Cell::new(0)),
            selection_guard: Rc::new(Cell::new(false)),
            on_action: Rc::new(RefCell::new(Box::new(|_| {}))),
            create_button,
            compose_button,
            close_button,
            create_hint,
            task_list,
            detail_title,
            detail_status,
            actions_box,
            actions_expander,
            detail_scroll,
            action_buttons: Rc::new(RefCell::new(Vec::new())),
            action_owner: Rc::new(Cell::new(None)),
            primary_target: Rc::new(Cell::new(None)),
            primary_action,
            next_step_card,
            next_step_title,
            next_step_detail,
            next_step_reason,
            approvals_box,
            stream_page_button,
            diff_page_button,
            inspector_page_button,
            inspector_view,
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
        self.attention_filter
            .connect_selected_notify(move |dropdown| {
                if panel.filter_guard.get() {
                    return;
                }
                let kind = dropdown.selected().checked_sub(1).and_then(|index| {
                    crate::agent_task::DashboardAttentionKind::ALL
                        .get(index as usize)
                        .copied()
                });
                panel.attention_kind.set(kind);
                panel
                    .dashboard_view
                    .set(crate::agent_task::DashboardView::All);
                panel.filters.borrow_mut().status = None;
                let guard = panel.filter_guard.replace(true);
                panel.status_filter.set_selected(0);
                panel.filter_guard.set(guard);
                panel.apply_dashboard_filters();
            });
        let panel = self.clone();
        self.primary_action.connect_clicked(move |_| {
            if let Some((id, kind)) = panel.primary_target.get() {
                if panel
                    .state
                    .borrow()
                    .detail
                    .as_ref()
                    .is_some_and(|detail| detail.id == id)
                {
                    panel.act(kind);
                }
            }
        });
        for control in [
            &self.provider_filter,
            &self.workspace_filter,
            &self.status_filter,
        ] {
            let panel = self.clone();
            let is_status = control == &self.status_filter;
            control.connect_selected_notify(move |_| {
                if panel.filter_guard.get() {
                    return;
                }
                if is_status {
                    panel.attention_kind.set(None);
                }
                *panel.filters.borrow_mut() =
                    TaskFilters {
                        provider: panel.provider_filter.selected().checked_sub(1).and_then(
                            |index| {
                                crate::agent_task::AgentProvider::ALL
                                    .get(index as usize)
                                    .copied()
                            },
                        ),
                        workspace: panel.workspace_filter.selected().checked_sub(1).and_then(
                            |index| {
                                panel
                                    .workspace_choices
                                    .borrow()
                                    .get(index as usize)
                                    .cloned()
                            },
                        ),
                        status: panel
                            .status_filter
                            .selected()
                            .checked_sub(1)
                            .and_then(|index| STATES.get(index as usize).copied()),
                    };
                panel.apply_dashboard_filters();
            });
        }
        let panel = self.clone();
        self.dashboard_query
            .connect_changed(move |_| panel.apply_dashboard_filters());
        let panel = self.clone();
        self.dashboard_sort.connect_selected_notify(move |_| {
            if !panel.filter_guard.get() {
                panel.apply_dashboard_filters();
            }
        });
        let panel = self.clone();
        self.dashboard_focus_project.connect_clicked(move |_| {
            panel.apply_dashboard_filters();
            let project = {
                let state = panel.state.borrow();
                TaskFilters::selected_project(&state.dashboard, state.selected, &state.workspaces)
            };
            if let Some(project) = project {
                let index = panel
                    .workspace_choices
                    .borrow()
                    .iter()
                    .position(|path| *path == project);
                if let Some(index) = index {
                    panel.workspace_filter.set_selected(index as u32 + 1);
                }
            }
        });
        let panel = self.clone();
        self.previous_attention.connect_clicked(move |_| {
            panel.apply_dashboard_filters();
            let (target, selected) = {
                let state = panel.state.borrow();
                (
                    state.dashboard.previous_attention(state.selected),
                    state.selected,
                )
            };
            if let Some(id) = target.filter(|id| Some(*id) != selected) {
                panel.emit(TaskPanelAction::Select(id));
            }
        });
        let panel = self.clone();
        self.next_attention.connect_clicked(move |_| {
            panel.apply_dashboard_filters();
            let (target, selected) = {
                let state = panel.state.borrow();
                (
                    state.dashboard.next_attention(state.selected),
                    state.selected,
                )
            };
            if let Some(id) = target.filter(|id| Some(*id) != selected) {
                panel.emit(TaskPanelAction::Select(id));
            }
        });
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
                panel.attention_kind.set(None);
                panel.dashboard_view.set(view);
                panel.apply_dashboard_filters();
            });
        }
        for button in [&self.dashboard_clear, &self.dashboard_reveal] {
            let panel = self.clone();
            button.connect_clicked(move |_| {
                panel.filter_guard.set(true);
                panel.attention_kind.set(None);
                panel.attention_filter.set_selected(0);
                *panel.filters.borrow_mut() = TaskFilters::default();
                panel.provider_filter.set_selected(0);
                panel.workspace_filter.set_selected(0);
                panel.status_filter.set_selected(0);
                panel.dashboard_sort.set_selected(0);
                panel.filter_guard.set(false);
                panel
                    .dashboard_view
                    .set(crate::agent_task::DashboardView::All);
                panel.dashboard_query.set_text("");
                panel.apply_dashboard_filters();
            });
        }
        let panel = self.clone();
        self.compose_button.connect_clicked(move |_| {
            panel.emit(TaskPanelAction::ComposeProject);
        });
        let panel = self.clone();
        self.inspector_page_button.connect_clicked(move |_| {
            panel.show_page(INSPECTOR_PAGE);
        });
        let panel = self.clone();
        self.create_button.connect_clicked(move |_| {
            panel.emit(TaskPanelAction::CreateFromBlock);
        });
        let panel = self.clone();
        self.close_button.connect_clicked(move |_| {
            panel.emit(TaskPanelAction::Close);
        });

        let keys = gtk4::EventControllerKey::new();
        keys.set_name(Some("task-list-navigation"));
        let panel = self.clone();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            if modifiers.intersects(
                gtk4::gdk::ModifierType::CONTROL_MASK
                    | gtk4::gdk::ModifierType::ALT_MASK
                    | gtk4::gdk::ModifierType::SUPER_MASK,
            ) {
                return gtk4::glib::Propagation::Proceed;
            }
            if panel.navigate_task_list(key) {
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
        self.task_list.add_controller(keys);

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
            if panel.follow_up_guard.get() {
                return;
            }
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
            ActionKind::StartNative => TaskPanelAction::StartNative(id),
            ActionKind::StartTerminal => TaskPanelAction::StartTerminal(id),
            ActionKind::FocusTerminal => TaskPanelAction::FocusTerminal(id),
            ActionKind::FocusValidation => TaskPanelAction::FocusValidation(id),
            ActionKind::StopCodex => TaskPanelAction::StopCodex(id),
            ActionKind::FinishCodex => TaskPanelAction::FinishCodex(id),
            ActionKind::RunValidation => TaskPanelAction::RunValidation(id),
            ActionKind::Complete => TaskPanelAction::Complete(id),
            ActionKind::AcceptReviewed => TaskPanelAction::AcceptReviewed(id),
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
            let request = self.follow_up_receipts.borrow_mut().begin(id, text);
            if let Some(request) = request {
                self.refresh_follow_up_send();
                self.emit(TaskPanelAction::FollowUp(request));
            }
        }
    }

    pub(crate) fn resolve_follow_up(
        &self,
        request: TaskFollowUpRequest,
        result: Result<(), String>,
    ) {
        self.follow_up_receipts.borrow_mut().resolve(
            &mut self.follow_up_drafts.borrow_mut(),
            &request,
            result,
        );
        self.refresh_view();
    }

    /// Replace the composed panel state and repaint. The view diffs each push
    /// against what it currently renders and touches widgets only on real
    /// change, so an unchanged push is allocation-quiet; the domain remains
    /// the only owner of task state.
    pub(crate) fn sync(&self, sync: TasksPanelSync) {
        let choices = self.filters.borrow().workspace_choices(&sync.workspaces);
        if *self.workspace_choices.borrow() != choices {
            // Labels are bounded display text only. Ordinals distinguish collisions.
            let mut labels = vec!["All workspaces".to_owned()];
            labels.extend(choices.iter().enumerate().map(|(index, path)| {
                format!(
                    "{} · {}",
                    index + 1,
                    crate::review_text::visible_bounded(&path.to_string_lossy(), 160)
                )
            }));
            let strings = labels.iter().map(String::as_str).collect::<Vec<_>>();
            let selected = self
                .filters
                .borrow()
                .workspace
                .as_ref()
                .and_then(|path| choices.iter().position(|choice| choice == path))
                .map_or(0, |index| index as u32 + 1);
            self.filter_guard.set(true);
            *self.workspace_choices.borrow_mut() = choices;
            self.workspace_filter
                .set_model(Some(&gtk4::StringList::new(&strings)));
            self.workspace_filter.set_selected(selected);
            self.filter_guard.set(false);
        }
        let live = sync
            .dashboard
            .rows
            .iter()
            .map(|row| row.id)
            .collect::<std::collections::HashSet<_>>();
        self.row_widgets
            .borrow_mut()
            .retain(|id, _| live.contains(id));
        *self.unfiltered_state.borrow_mut() = sync;
        self.apply_dashboard_filters();
    }

    fn apply_dashboard_filters(&self) {
        let mut sync = self.unfiltered_state.borrow().clone();
        let mut dashboard = self.filters.borrow().scope(
            &sync.dashboard,
            &sync.workspaces,
            self.dashboard_query.text().as_str(),
        );
        let attention_counts = dashboard.attention_counts();
        let attention_labels = std::iter::once("All attention reasons".to_string())
            .chain(
                crate::agent_task::DashboardAttentionKind::ALL
                    .iter()
                    .map(|kind| {
                        let count = attention_counts.get(*kind);
                        format!("{} · {count}", kind.label())
                    }),
            )
            .collect::<Vec<_>>();
        let guard = self.filter_guard.replace(true);
        let labels_changed = self
            .attention_filter
            .model()
            .and_downcast::<gtk4::StringList>()
            .is_none_or(|list| {
                list.n_items() as usize != attention_labels.len()
                    || attention_labels.iter().enumerate().any(|(index, label)| {
                        list.string(index as u32).as_deref() != Some(label.as_str())
                    })
            });
        if labels_changed {
            self.attention_filter.set_model(Some(&gtk4::StringList::new(
                &attention_labels
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )));
        }
        self.attention_filter.set_selected(
            self.attention_kind
                .get()
                .and_then(|kind| {
                    crate::agent_task::DashboardAttentionKind::ALL
                        .iter()
                        .position(|candidate| *candidate == kind)
                })
                .map_or(0, |index| index as u32 + 1),
        );
        self.filter_guard.set(guard);
        let query_error =
            crate::agent_task::dashboard_query_error(self.dashboard_query.text().as_str());
        self.dashboard_query_error
            .set_visible(query_error.is_some());
        self.dashboard_query_error
            .set_label(query_error.as_deref().unwrap_or_default());
        if query_error.is_some() {
            self.dashboard_query.add_css_class("error");
        } else {
            self.dashboard_query.remove_css_class("error");
        }
        let status = self.filters.borrow().status;
        dashboard
            .rows
            .retain(|row| status.is_none_or(|status| row.status == status));
        dashboard.apply_view(
            self.dashboard_view.get(),
            self.attention_kind.get(),
            crate::agent_task::DashboardSort::ALL
                .get(self.dashboard_sort.selected() as usize)
                .copied()
                .unwrap_or_default(),
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
        let filters = self.filters.borrow();
        let active_scope = usize::from(filters.provider.is_some())
            + usize::from(filters.workspace.is_some())
            + usize::from(filters.status.is_some());
        let scope_label = if active_scope == 0 {
            "Provider, project and state".to_string()
        } else {
            format!("Filters · {active_scope} active")
        };
        self.scope_filters.set_label(Some(&scope_label));
        drop(filters);
        self.dashboard_summary.set_label(&format!(
            "{} shown · {} provider/workspace/search matches · {} need you · {} awaiting approval (counts before state/view)",
            dashboard.rows.len(),
            dashboard.counts.total,
            dashboard.counts.attention,
            dashboard.counts.pending_approval,
        ));
        self.dashboard_clear.set_visible(
            *self.filters.borrow() != TaskFilters::default()
                || self.attention_kind.get().is_some()
                || !self.dashboard_query.text().is_empty()
                || self.dashboard_sort.selected() != 0
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
            if *view == self.dashboard_view.get() && self.attention_kind.get().is_none() {
                button.add_css_class("suggested-action");
            } else {
                button.remove_css_class("suggested-action");
            }
        }
        self.dashboard_focus_project.set_sensitive(
            TaskFilters::selected_project(&dashboard, sync.selected, &sync.workspaces).is_some(),
        );
        self.previous_attention
            .set_sensitive(dashboard.previous_attention(sync.selected).is_some());
        self.next_attention
            .set_sensitive(dashboard.next_attention(sync.selected).is_some());
        sync.dashboard = dashboard;
        *self.state.borrow_mut() = sync;
        self.refresh_view();
    }

    fn refresh_view(&self) {
        let state = self.state.borrow();
        self.compose_button
            .set_sensitive(state.create_enabled && !state.pending_creation);
        self.create_button
            .set_sensitive(state.create_enabled && !state.pending_creation);
        let show_hint = !state.create_hint.is_empty();
        self.create_hint.set_visible(show_hint);
        if show_hint {
            self.create_hint.set_label(&state.create_hint);
        }

        self.sync_task_list(&state);

        let detail = state.detail.as_deref();
        let owner = detail.map(|detail| detail.id);
        if self.action_owner.replace(owner) != owner {
            for (_, button) in self.action_buttons.borrow().iter() {
                cancel_task_action_activation(button);
            }
            cancel_task_action_activation(&self.follow_up_send);
            self.detail_scroll.vadjustment().set_value(0.0);
        }
        self.next_step_card.set_visible(detail.is_some());
        if let Some(detail) = detail {
            self.next_step_title.set_label(detail.guidance.title);
            self.next_step_detail.set_label(detail.guidance.detail);
            self.next_step_detail
                .set_tooltip_text(Some(detail.guidance.detail));
            self.next_step_reason
                .set_visible(detail.guidance.reason.is_some());
            self.next_step_reason
                .set_label(detail.guidance.reason.as_deref().unwrap_or_default());
            self.next_step_reason
                .set_tooltip_text(detail.guidance.reason.as_deref());
        }
        for (kind, button) in self.action_buttons.borrow().iter() {
            let sensitive = detail.is_some_and(|detail| match kind {
                ActionKind::StartNative => detail.can_start_native,
                ActionKind::StartTerminal => detail.can_start_terminal,
                ActionKind::FocusTerminal => detail.can_focus_terminal,
                ActionKind::FocusValidation => detail.can_focus_validation,
                ActionKind::StopCodex => detail.can_stop,
                ActionKind::FinishCodex => detail.can_finish,
                ActionKind::RunValidation => detail.can_run_validation,
                ActionKind::Complete => detail.can_complete,
                ActionKind::AcceptReviewed => detail.can_accept_review,
                ActionKind::ReviewDiff => true,
                // Archiving is always safe to offer for a selected task.
                ActionKind::Archive => true,
            });
            button.set_sensitive(sensitive);
            let visible = detail.is_some_and(|detail| match kind {
                ActionKind::Complete => !detail.manual_review,
                ActionKind::AcceptReviewed => detail.manual_review,
                _ => true,
            });
            button.set_visible(visible);
            if let Some(holder) = button.parent().and_downcast::<gtk4::FlowBoxChild>() {
                holder.set_visible(visible);
            }
            if *kind == ActionKind::StartNative {
                if let Some(detail) = detail {
                    button.set_label(&detail.native_label);
                }
            }
        }
        self.actions_expander.set_visible(detail.is_some());
        self.detail_title.set_visible(detail.is_some());
        self.detail_status.set_visible(detail.is_some());
        self.page_stack.set_visible(detail.is_some());
        self.inspector_page_button.set_visible(detail.is_some());
        self.stream_page_button.set_visible(detail.is_some());
        self.diff_page_button.set_visible(detail.is_some());
        self.follow_up_row
            .set_visible(detail.is_some_and(|d| d.can_follow_up));
        if detail.is_none() {
            self.follow_up_hint.set_visible(false);
            self.follow_up_send.set_sensitive(false);
        }

        if let Some(detail) = detail {
            set_view_text(&self.inspector_view, &detail.inspector);
            self.detail_title.set_label(&detail.title);
            self.detail_status
                .set_label(&format!("{} · {}", detail.status_line, detail.branch));

            let stream_text = detail
                .stream
                .as_deref()
                .map(render_stream_text)
                .unwrap_or_else(|| "No native session snapshot for this task yet.".to_string());
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
            let receipts = self.follow_up_receipts.borrow();
            let error = receipts.error(detail.id);
            let hint = error.unwrap_or(&detail.follow_up_hint);
            self.follow_up_hint.set_visible(!hint.is_empty());
            self.follow_up_hint.set_label(hint);
            self.follow_up_hint.set_tooltip_text(error);
            if error.is_some() {
                self.follow_up_hint.add_css_class("error");
            } else {
                self.follow_up_hint.remove_css_class("error");
            }
            drop(receipts);
            let draft = self
                .follow_up_drafts
                .borrow()
                .get(&detail.id)
                .cloned()
                .unwrap_or_default();
            if self.follow_up_entry.text().as_str() != draft {
                let guard = self.follow_up_guard.replace(true);
                self.follow_up_entry.set_text(&draft);
                self.follow_up_guard.set(guard);
            }
        }
        drop(state);
        self.refresh_follow_up_send();
        self.sync_approvals();
    }

    fn refresh_primary_action(&self, detail: Option<&TaskDetailSync>) {
        let primary = detail.and_then(|detail| {
            primary_task_action(detail).filter(|(kind, _)| {
                if *kind != ActionKind::FinishCodex {
                    return true;
                }
                let has_draft = self
                    .follow_up_drafts
                    .borrow()
                    .get(&detail.id)
                    .is_some_and(|draft| !draft.trim().is_empty());
                !has_draft && !self.follow_up_receipts.borrow().is_pending(detail.id)
            })
        });
        let primary_target = detail
            .zip(primary.as_ref())
            .map(|(detail, (kind, _))| (detail.id, *kind));
        if self.primary_target.get() != primary_target {
            cancel_task_action_activation(&self.primary_action);
            self.primary_target.set(primary_target);
        }
        self.primary_action.set_visible(primary.is_some());
        self.primary_action.set_sensitive(primary.is_some());
        if let Some((_, label)) = primary {
            self.primary_action.set_label(&label);
            self.primary_action
                .update_property(&[gtk4::accessible::Property::Label(&label)]);
        }
    }

    fn refresh_follow_up_send(&self) {
        let state = self.state.borrow();
        self.refresh_primary_action(state.detail.as_deref());
        let sensitive = state.detail.as_deref().is_some_and(|detail| {
            let draft = self
                .follow_up_drafts
                .borrow()
                .get(&detail.id)
                .cloned()
                .unwrap_or_default();
            detail.can_follow_up
                && !self.follow_up_receipts.borrow().is_pending(detail.id)
                && native_follow_up_can_send(&draft, detail.completed_turns)
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
    fn navigate_task_list(&self, key: gtk4::gdk::Key) -> bool {
        use gtk4::gdk::Key;
        if !matches!(key, Key::Up | Key::Down | Key::Home | Key::End) {
            return false;
        }
        let len = self.state.borrow().rows.len();
        if len == 0 {
            return true;
        }
        let current = self
            .task_list
            .selected_row()
            .map(|row| row.index() as usize);
        let target = match key {
            Key::Home => 0,
            Key::End => len - 1,
            Key::Up => current.map_or(len - 1, |index| index.saturating_sub(1)),
            _ => current.map_or(0, |index| (index + 1).min(len - 1)),
        };
        if let Some(row) = self.task_list.row_at_index(target as i32) {
            self.task_list.select_row(Some(&row));
            row.grab_focus();
        }
        true
    }

    fn sync_task_list(&self, state: &TasksPanelSync) {
        let plan = plan_list_refresh(
            &self.rendered_rows.borrow(),
            self.rendered_selected.get(),
            &state.rows,
            state.selected,
        );
        if plan.rebuild_rows {
            self.selection_guard.set(true);
            let old_ids = self
                .rendered_rows
                .borrow()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>();
            let new_ids = state.rows.iter().map(|row| row.id).collect::<Vec<_>>();
            let mut cache = self.row_widgets.borrow_mut();
            let focused = cache
                .iter()
                .find_map(|(id, widgets)| widgets.row.has_focus().then_some(*id));
            // Cache is bounded by the current unfiltered snapshot, not task history.
            let live = self
                .unfiltered_state
                .borrow()
                .dashboard
                .rows
                .iter()
                .map(|row| row.id)
                .collect::<std::collections::HashSet<_>>();
            cache.retain(|id, _| live.contains(id));
            for row in &state.rows {
                if let Some(widgets) = cache.get_mut(&row.id) {
                    if widgets.snapshot != *row {
                        widgets.update(row);
                    }
                } else {
                    cache.insert(row.id, TaskRowWidgets::new(row));
                }
            }
            if old_ids != new_ids {
                // GTK clears the list's selected-row pointer on removal but
                // retains the cached row's selected bit. Clear it while attached
                // or reselecting the same widget after a move/reveal is a no-op.
                // The existing guard suppresses the synchronous selection signal.
                if let Some(selected) = self.task_list.selected_row() {
                    self.task_list.unselect_row(&selected);
                }
                while let Some(child) = self.task_list.first_child() {
                    self.task_list.remove(&child);
                }
                for id in &new_ids {
                    self.task_list.append(&cache[id].row);
                }
                // Do not steal focus from controls or a task now hidden by filters.
                if let Some(id) = focused.filter(|id| new_ids.contains(id)) {
                    cache[&id].row.grab_focus();
                }
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
// GTK's delayed keyboard activation survives a sensitivity toggle. Reparenting
// cancels that pending activation before a button can acquire another task.
fn cancel_task_action_activation(button: &gtk4::Button) {
    button.set_sensitive(false);
    if let Some(holder) = button.parent().and_downcast::<gtk4::FlowBoxChild>() {
        holder.set_child(None::<&gtk4::Widget>);
        holder.set_child(Some(button));
    } else if let Some(parent) = button.parent().and_downcast::<gtk4::Box>() {
        let previous = button.prev_sibling();
        parent.remove(button);
        parent.insert_child_after(button, previous.as_ref());
    }
}

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
impl TasksPanel {
    fn baseline_full_rebuild(&self, state: &TasksPanelSync) {
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
    pub(super) fn dashboard_test_sync() -> TasksPanelSync {
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
            inspector: "Repository\n/tmp/project".into(),
            guidance: crate::agent_task::TaskGuidance::from_task(&tasks[0], Default::default()),
            validation_status: tasks[0].validation.status,
            native_label: "Start Codex".into(),
            manual_review: false,
            can_accept_review: false,
            stream: None,
            approvals: Vec::new(),
            completed_turns: 0,
            can_start_native: false,
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
    fn agent_management_v3_inspector_and_manual_review_keep_task_identity() {
        gtk4::init().expect("GTK display");
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        let panel = TasksPanel::build();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let window = gtk4::Window::builder()
            .default_width(520)
            .default_height(920)
            .build();
        window.set_child(Some(&panel.root));
        window.present();
        let drain = || {
            while gtk4::glib::MainContext::default().pending() {
                gtk4::glib::MainContext::default().iteration(false);
            }
        };
        let mut sync = dashboard_test_sync();
        let selected = sync.selected.unwrap();
        let detail = sync.detail.as_mut().unwrap();
        detail.manual_review = true;
        detail.can_accept_review = true;
        detail.native_label = "Start Claude".into();
        detail.inspector = "Provider\nClaude\n\nRepository\n/tmp/project\n\nGoal\nReview the work before accepting it".into();
        panel.sync(sync);
        panel.actions_expander.set_expanded(true);
        drain();
        let action_button = |kind| {
            panel
                .action_buttons
                .borrow()
                .iter()
                .find(|(candidate, _)| *candidate == kind)
                .unwrap()
                .1
                .clone()
        };
        let accept = action_button(ActionKind::AcceptReviewed);
        assert!(accept.is_visible() && accept.is_sensitive());
        assert!(!action_button(ActionKind::Complete).is_visible());
        assert_eq!(
            action_button(ActionKind::StartNative).label().as_deref(),
            Some("Start Claude")
        );
        panel.inspector_page_button.emit_clicked();
        drain();
        assert_eq!(
            panel.page_stack.visible_child_name().as_deref(),
            Some(INSPECTOR_PAGE)
        );
        assert!(!panel.inspector_view.is_editable());
        assert!(
            actions.borrow().is_empty(),
            "reading details does not operate a task"
        );
        if let Some(path) = std::env::var_os("JTERM_TASK_INSPECTOR_SCREENSHOT") {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
            while std::time::Instant::now() < deadline {
                drain();
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            assert!(std::process::Command::new("import")
                .args(["-window", "root"])
                .arg(path)
                .status()
                .unwrap()
                .success());
        }
        accept.emit_clicked();
        drain();
        assert_eq!(
            actions.borrow().as_slice(),
            &[TaskPanelAction::AcceptReviewed(selected)]
        );
        actions.borrow_mut().clear();
        accept.emit_by_name::<()>("activate", &[]);
        let mut replacement = dashboard_test_sync();
        replacement.detail.as_mut().unwrap().manual_review = true;
        replacement.detail.as_mut().unwrap().can_accept_review = true;
        panel.sync(replacement);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(320);
        while std::time::Instant::now() < deadline {
            drain();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            actions.borrow().is_empty(),
            "a delayed review action cannot accept a newly selected task"
        );
        window.close();
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
        row.status = crate::agent_task::TaskStatus::ReadyForReview;
        row.status_detail = Some("task reason".into());
        row.validation_status = crate::agent_task::TaskValidationStatus::Failed;
        row.validation_detail = Some("validation old".into());
        let row = sync
            .dashboard
            .rows
            .iter_mut()
            .find(|row| row.id == selected)
            .unwrap();
        row.status = crate::agent_task::TaskStatus::ReadyForReview;
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
    fn selected_project_focus_keeps_query_identity_and_rejects_hidden_selection() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let mut sync = dashboard_test_sync();
        let selected = sync.selected.unwrap();
        sync.workspaces = sync
            .rows
            .iter()
            .map(|row| {
                (
                    row.id,
                    std::path::PathBuf::from(if row.id == selected {
                        "/repo/selected"
                    } else {
                        "/repo/other"
                    }),
                )
            })
            .collect();
        panel.sync(sync);
        panel.dashboard_query.set_text("provider:codex");
        panel.provider_filter.set_selected(1);
        assert!(panel.dashboard_focus_project.is_sensitive());
        panel.dashboard_focus_project.emit_clicked();
        assert_eq!(
            panel.filters.borrow().workspace.as_deref(),
            Some(std::path::Path::new("/repo/selected"))
        );
        assert_eq!(panel.dashboard_query.text().as_str(), "provider:codex");
        assert_eq!(
            panel.filters.borrow().provider,
            Some(crate::agent_task::AgentProvider::Codex)
        );
        assert_eq!(panel.state.borrow().selected, Some(selected));
        assert_eq!(panel.state.borrow().rows.len(), 1);
        assert!(
            actions.borrow().is_empty(),
            "view focus cannot operate a task"
        );
        panel.dashboard_clear.emit_clicked();
        panel
            .dashboard_view
            .set(crate::agent_task::DashboardView::Finished);
        panel.apply_dashboard_filters();
        assert!(!panel.dashboard_focus_project.is_sensitive());
        panel.dashboard_focus_project.emit_clicked();
        assert!(panel.filters.borrow().workspace.is_none());
        assert_eq!(panel.state.borrow().selected, Some(selected));
        assert!(actions.borrow().is_empty());
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
        panel.dashboard_clear.emit_clicked();
        panel.dashboard_sort.set_selected(3);
        assert_eq!(panel.state.borrow().rows[0].title, "First task");
        assert!(actions.borrow().is_empty(), "sorting never operates a task");
        let attention = sync.rows.iter().find(|row| row.needs_attention).unwrap().id;
        panel.next_attention.emit_clicked();
        assert_eq!(
            actions.borrow().as_slice(),
            &[TaskPanelAction::Select(attention)]
        );
        actions.borrow_mut().clear();
        panel.previous_attention.emit_clicked();
        assert_eq!(
            actions.borrow().as_slice(),
            &[TaskPanelAction::Select(attention)]
        );
        actions.borrow_mut().clear();
        panel.dashboard_query.set_text("First");
        assert!(!panel.previous_attention.is_sensitive());
        assert!(!panel.next_attention.is_sensitive());
        panel.previous_attention.emit_clicked();
        panel.next_attention.emit_clicked();
        assert!(
            actions.borrow().is_empty(),
            "next-attention cannot select a hidden task"
        );
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
    fn thousand_task_sync() -> TasksPanelSync {
        let template = dashboard_test_sync();
        let mut sync = TasksPanelSync::default();
        for index in 0..1000 {
            let id = TaskId::new();
            let mut row = template.rows[0].clone();
            row.id = id;
            row.title = "Same task".into();
            row.provider = crate::agent_task::AgentProvider::ALL[index % 4];
            row.status = crate::agent_task::TaskStatus::Working;
            row.has_active_agent_stream = true;
            let mut dashboard = template.dashboard.rows[0].clone();
            dashboard.id = id;
            dashboard.title.clone_from(&row.title);
            dashboard.provider = row.provider;
            dashboard.status = row.status;
            dashboard.needs_attention = false;
            dashboard.activity.has_active_agent_stream = true;
            dashboard.updated_at_ms = 42;
            sync.rows.push(row);
            sync.dashboard.rows.push(dashboard);
            sync.workspaces
                .insert(id, format!("/fixture/workspace-{}", index % 3).into());
        }
        sync.selected = Some(sync.rows[0].id);
        sync
    }

    fn send_list_key(panel: &TasksPanel, key: gtk4::gdk::Key) {
        let controllers = panel.task_list.observe_controllers();
        let keys = (0..controllers.n_items())
            .find_map(|index| {
                controllers
                    .item(index)
                    .and_then(|item| item.downcast::<gtk4::EventControllerKey>().ok())
                    .filter(|controller| {
                        controller.name().as_deref() == Some("task-list-navigation")
                    })
            })
            .unwrap();
        let stopped = keys.emit_by_name::<bool>(
            "key-pressed",
            &[&key, &0u32, &gtk4::gdk::ModifierType::empty()],
        );
        assert!(stopped);
    }

    #[test]
    #[ignore = "requires a native GTK display"]
    fn task_filters_keep_identity_hidden_updates_and_keyboard_routing() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let window = gtk4::Window::new();
        let title = format!(
            "forge-f07-controller-{}-{}",
            std::process::id(),
            TaskId::new()
        );
        window.set_title(Some(&title));
        window.set_default_size(420, 800);
        window.set_child(Some(&panel.root));
        window.present();
        let mut sync = thousand_task_sync();
        let selected = sync.selected.unwrap();
        panel.sync(sync.clone());
        let first_widget = panel.row_widgets.borrow()[&selected].row.clone();
        panel.provider_filter.set_selected(2); // Claude hides selected Codex.
        assert_eq!(panel.state.borrow().selected, Some(selected));
        assert!(panel.dashboard_reveal.is_visible());
        assert!(panel.state.borrow().detail.is_none());
        assert_eq!(panel.state.borrow().rows.len(), 250);
        panel.workspace_filter.set_selected(1);
        assert!(panel
            .state
            .borrow()
            .rows
            .iter()
            .all(|row| sync.workspaces[&row.id].as_path()
                == std::path::Path::new("/fixture/workspace-0")));
        panel.status_filter.set_selected(8); // Failed.
        assert!(panel.state.borrow().rows.is_empty());
        assert!(panel
            .dashboard_summary
            .label()
            .contains("counts before state/view"));
        // Hidden native activity still arrives through the authoritative snapshot.
        sync.rows[0].status = crate::agent_task::TaskStatus::Failed;
        sync.rows[0].status_detail = Some("Hidden task changed".into());
        sync.dashboard.rows[0].status = crate::agent_task::TaskStatus::Failed;
        sync.dashboard.rows[0].status_detail = Some("Hidden task changed".into());
        sync.dashboard.rows[0].needs_attention = true;
        panel.sync(sync.clone());
        assert_eq!(
            panel.unfiltered_state.borrow().rows[0].status,
            crate::agent_task::TaskStatus::Failed
        );
        assert!(
            actions.borrow().is_empty(),
            "filters and hidden updates must never control tasks"
        );
        panel.dashboard_reveal.emit_clicked();
        assert_eq!(panel.state.borrow().selected, Some(selected));
        assert_eq!(panel.row_widgets.borrow()[&selected].row, first_widget);
        assert!(panel.row_widgets.borrow()[&selected]
            .status
            .label()
            .contains("Failed"));
        assert_eq!(panel.provider_filter.selected(), 0);
        assert_eq!(panel.workspace_filter.selected(), 0);
        assert_eq!(panel.status_filter.selected(), 0);
        // Real application callbacks can synchronously push a new snapshot.
        let reentrant_panel = panel.clone();
        let sink = actions.clone();
        panel.connect_action(move |action| {
            sink.borrow_mut().push(action.clone());
            if let TaskPanelAction::Select(id) = action {
                let mut next = reentrant_panel.unfiltered_state.borrow().clone();
                next.selected = Some(id);
                reentrant_panel.sync(next);
            }
        });
        // has_focus requires an active native toplevel, not only a present request.
        wait_for_gtk(|| window.is_mapped());
        let pattern = format!("^{title}$");
        let ids = x11_input(&["search", "--sync", "--onlyvisible", "--name", &pattern]);
        let ids = ids.lines().collect::<Vec<_>>();
        assert_eq!(ids.len(), 1);
        x11_input(&["windowfocus", "--sync", ids[0]]);
        wait_for_gtk(|| window.is_active());
        panel.task_list.grab_focus();
        send_list_key(&panel, gtk4::gdk::Key::End);
        let expected = panel.state.borrow().rows.last().unwrap().id;
        assert!(
            matches!(actions.borrow().last(), Some(TaskPanelAction::Select(id)) if *id == expected)
        );
        let focused = panel.row_widgets.borrow()[&expected].row.clone();
        assert!(focused.has_focus());
        let action_count = actions.borrow().len();
        // Reordering while a same-name task has focus keeps the exact widget.
        sync.selected = Some(expected);
        sync.dashboard
            .rows
            .iter_mut()
            .find(|row| row.id == expected)
            .unwrap()
            .updated_at_ms = 500;
        panel.sync(sync.clone());
        assert!(focused.has_focus());
        assert_eq!(
            actions.borrow().len(),
            action_count,
            "programmatic reconciliation emits no selection"
        );
        send_list_key(&panel, gtk4::gdk::Key::Home);
        let first = panel.state.borrow().rows[0].id;
        assert!(
            matches!(actions.borrow().last(), Some(TaskPanelAction::Select(id)) if *id == first)
        );
        send_list_key(&panel, gtk4::gdk::Key::Down);
        let second = panel.state.borrow().rows[1].id;
        assert!(
            matches!(actions.borrow().last(), Some(TaskPanelAction::Select(id)) if *id == second)
        );
        send_list_key(&panel, gtk4::gdk::Key::Up);
        assert_eq!(
            panel.task_list.selected_row().unwrap(),
            panel.row_widgets.borrow()[&first].row
        );
        // Archived domain rows remain persisted but must release view widgets.
        let archived = sync.rows[900].id;
        sync.rows[900].status = crate::agent_task::TaskStatus::Archived;
        sync.dashboard.rows.retain(|row| row.id != archived);
        sync.workspaces.remove(&archived);
        panel.sync(sync.clone());
        assert!(!panel.row_widgets.borrow().contains_key(&archived));
        // Removed hidden rows cannot accumulate in the view cache.
        panel.dashboard_query.set_text("no matches");
        let removed = sync.rows.pop().unwrap().id;
        sync.dashboard.rows.retain(|row| row.id != removed);
        sync.workspaces.remove(&removed);
        panel.sync(sync);
        assert!(!panel.row_widgets.borrow().contains_key(&removed));
        window.close();
    }

    #[test]
    #[ignore = "requires a native GTK display"]
    fn thousand_task_filter_and_update_native_benchmark() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let baseline = TasksPanel::build();
        let baseline_window = gtk4::Window::new();
        baseline_window.set_default_size(420, 800);
        baseline_window.set_child(Some(&baseline.root));
        baseline_window.present();
        let window = gtk4::Window::new();
        window.set_default_size(420, 800);
        window.set_child(Some(&panel.root));
        window.present();
        let mut sync = thousand_task_sync();
        let initial = std::time::Instant::now();
        panel.sync(sync.clone());
        let initial_ms = initial.elapsed().as_secs_f64() * 1000.0;
        let stable = panel
            .row_widgets
            .borrow()
            .iter()
            .map(|(id, widgets)| (*id, widgets.row.clone()))
            .collect::<HashMap<_, _>>();
        let mut baseline_state = panel.state.borrow().clone();
        baseline.baseline_full_rebuild(&baseline_state);
        let mut updates = Vec::new();
        let mut filters = Vec::new();
        let mut baseline_updates = Vec::new();
        let pulses = Rc::new(Cell::new(0));
        let context = gtk4::glib::MainContext::default();
        for iteration in 0..20 {
            let marker = format!("Update {iteration}");
            sync.rows[500].status_detail = Some(marker.clone());
            sync.dashboard.rows[500].status_detail = Some(marker.clone());
            let start = std::time::Instant::now();
            panel.sync(sync.clone());
            updates.push(start.elapsed().as_secs_f64() * 1000.0);
            baseline_state
                .rows
                .iter_mut()
                .find(|row| row.id == sync.rows[500].id)
                .unwrap()
                .status_detail = Some(marker);
            let start = std::time::Instant::now();
            baseline.baseline_full_rebuild(&baseline_state);
            baseline_updates.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = std::time::Instant::now();
            panel
                .provider_filter
                .set_selected((iteration % 4 + 1) as u32);
            panel.provider_filter.set_selected(0);
            filters.push(start.elapsed().as_secs_f64() * 1000.0);
            let pulse = pulses.clone();
            gtk4::glib::idle_add_local_once(move || pulse.set(pulse.get() + 1));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while pulses.get() <= iteration && std::time::Instant::now() < deadline {
                context.iteration(false);
            }
            assert_eq!(
                pulses.get(),
                iteration + 1,
                "GTK idle remains serviceable between bounded updates"
            );
            assert!(panel
                .row_widgets
                .borrow()
                .iter()
                .all(|(id, widgets)| stable[id] == widgets.row));
        }
        let p95 = |samples: &mut [f64]| {
            samples.sort_by(f64::total_cmp);
            samples[(samples.len() * 95).div_ceil(100) - 1]
        };
        let update_p95 = p95(&mut updates);
        let filter_p95 = p95(&mut filters);
        let baseline_p95 = p95(&mut baseline_updates);
        eprintln!("F07 native 1000 tasks: initial={initial_ms:.2}ms; snapshot-update p95={update_p95:.2}ms; provider filter round-trip p95={filter_p95:.2}ms; immutable b7dc full-row-refresh baseline p95={baseline_p95:.2}ms; samples=20; idle pulses={}", pulses.get());
        assert!(
            update_p95 < 500.0 && filter_p95 < 500.0,
            "1000-task interactions exceeded 500ms responsiveness guard"
        );
        baseline_window.close();
        window.close();
    }
    /// Every command is a direct official xdotool invocation on the isolated
    /// fixture display. GTK keeps dispatching while it runs; timeout/panic
    /// kills and reaps only the child we spawned.
    fn x11_input(args: &[&str]) -> String {
        struct InputChild(Option<std::process::Child>);
        impl Drop for InputChild {
            fn drop(&mut self) {
                if let Some(child) = &mut self.0 {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let mut child = InputChild(Some(
            std::process::Command::new("xdotool")
                .args(args)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("hosted fixture requires official xdotool"),
        ));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let context = gtk4::glib::MainContext::default();
        loop {
            if child.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "xdotool command timed out: {args:?}"
            );
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let output = child.0.take().unwrap().wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "xdotool {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn wait_for_gtk(mut predicate: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let context = gtk4::glib::MainContext::default();
        while !predicate() {
            assert!(
                std::time::Instant::now() < deadline,
                "GTK fixture condition timed out"
            );
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    #[ignore = "requires isolated X11 display and official xdotool"]
    fn task_filters_route_real_x11_keyboard_by_identity() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let window = gtk4::Window::new();
        let title = format!(
            "forge-f07-keyboard-{}-{}",
            std::process::id(),
            TaskId::new()
        );
        window.set_title(Some(&title));
        window.set_default_size(420, 800);
        window.set_child(Some(&panel.root));
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        let reentrant = panel.clone();
        panel.connect_action(move |action| {
            sink.borrow_mut().push(action.clone());
            if let TaskPanelAction::Select(id) = action {
                let mut next = reentrant.unfiltered_state.borrow().clone();
                next.selected = Some(id);
                reentrant.sync(next);
            }
        });
        panel.sync(thousand_task_sync());
        window.present();
        wait_for_gtk(|| window.is_mapped());
        let pattern = format!("^{title}$");
        let ids = x11_input(&["search", "--sync", "--onlyvisible", "--name", &pattern]);
        let ids = ids.lines().collect::<Vec<_>>();
        assert_eq!(
            ids.len(),
            1,
            "only the uniquely named fixture window may receive input"
        );
        let window_id = ids[0];
        x11_input(&["windowfocus", "--sync", window_id]);
        assert_eq!(x11_input(&["getwindowfocus"]).trim(), window_id);
        let mut initial = panel.unfiltered_state.borrow().clone();
        initial.selected = Some(panel.state.borrow().rows[0].id);
        panel.sync(initial);
        panel.task_list.row_at_index(0).unwrap().grab_focus();
        for key in ["End", "Home", "Down", "Up"] {
            let target = match key {
                "End" => 999,
                "Down" => 1,
                _ => 0,
            };
            let expected = panel.state.borrow().rows[target].id;
            actions.borrow_mut().clear();
            x11_input(&["key", "--clearmodifiers", key]);
            wait_for_gtk(|| {
                panel.state.borrow().selected == Some(expected)
                    && panel.row_widgets.borrow()[&expected].row.has_focus()
            });
            assert!(
                matches!(actions.borrow().as_slice(), [TaskPanelAction::Select(id)] if *id == expected)
            );
        }
        // After filtering, the same physical key reaches the current visible
        // identity, not an old index in the original 1000-task table.
        panel.provider_filter.set_selected(2);
        let mut filtered = panel.unfiltered_state.borrow().clone();
        filtered.selected = Some(panel.state.borrow().rows[0].id);
        panel.sync(filtered);
        panel.task_list.row_at_index(0).unwrap().grab_focus();
        let expected = panel.state.borrow().rows.last().unwrap().id;
        actions.borrow_mut().clear();
        x11_input(&["key", "--clearmodifiers", "End"]);
        wait_for_gtk(|| {
            panel.state.borrow().selected == Some(expected)
                && panel.row_widgets.borrow()[&expected].row.has_focus()
        });
        assert!(
            matches!(actions.borrow().as_slice(), [TaskPanelAction::Select(id)] if *id == expected)
        );
        assert_eq!(panel.state.borrow().rows.len(), 250);
        // Updating/reordering must not steal focus from search; a real key
        // still edits that control rather than navigating the task list.
        panel.dashboard_query.grab_focus();
        let selection = panel.state.borrow().selected;
        let sync = panel.unfiltered_state.borrow().clone();
        panel.sync(sync);
        x11_input(&["key", "--clearmodifiers", "s"]);
        wait_for_gtk(|| panel.dashboard_query.text() == "s");
        assert_eq!(panel.state.borrow().selected, selection);
        window.close();
    }
    #[test]
    #[ignore = "requires a native GTK display"]
    fn cached_selected_row_survives_no_focus_move_hide_and_reveal() {
        gtk4::init().expect("GTK display");
        let panel = TasksPanel::build();
        let actions = Rc::new(RefCell::new(Vec::new()));
        let sink = actions.clone();
        panel.connect_action(move |action| sink.borrow_mut().push(action));
        let mut sync = dashboard_test_sync();
        let selected = sync.selected.unwrap();
        for row in &mut sync.dashboard.rows {
            row.status = crate::agent_task::TaskStatus::Working;
            row.needs_attention = false;
            row.updated_at_ms = if row.id == selected { 20 } else { 10 };
        }
        // No window or focus operation: focus must not repair GTK selection as
        // a side effect and conceal stale selected bits on cached widgets.
        panel.sync(sync.clone());
        let cached = panel.row_widgets.borrow()[&selected].row.clone();
        assert_eq!(cached.index(), 0);
        assert_eq!(panel.task_list.selected_row(), Some(cached.clone()));
        assert!(cached.is_selected());
        assert!(!cached.has_focus());
        sync.dashboard
            .rows
            .iter_mut()
            .find(|row| row.id == selected)
            .unwrap()
            .updated_at_ms = 5;
        panel.sync(sync.clone());
        assert_eq!(cached.index(), 1, "the selected widget itself moved");
        assert_eq!(panel.row_widgets.borrow()[&selected].row, cached);
        assert_eq!(panel.state.borrow().selected, Some(selected));
        assert_eq!(
            panel.task_list.selected_row(),
            Some(cached.clone()),
            "TaskId and row flags alone do not prove GTK's selected-row pointer survived"
        );
        assert!(cached.is_selected());
        assert!(!cached.has_focus());
        panel.dashboard_query.set_text("Needs review");
        assert!(panel.task_list.selected_row().is_none());
        assert!(
            !cached.is_selected(),
            "hidden retained row must have its selected bit cleared"
        );
        assert!(cached.parent().is_none());
        assert_eq!(panel.state.borrow().selected, Some(selected));
        assert!(panel.state.borrow().detail.is_none());
        panel.dashboard_clear.emit_clicked();
        assert_eq!(panel.row_widgets.borrow()[&selected].row, cached);
        assert_eq!(panel.task_list.selected_row(), Some(cached.clone()));
        assert!(cached.is_selected());
        panel.sync(sync);
        assert_eq!(panel.task_list.selected_row(), Some(cached));
        assert!(
            actions.borrow().is_empty(),
            "structural reconciliation stages no domain actions"
        );
    }
}

#[cfg(test)]
#[path = "tasks_panel_control_tests.rs"]
mod control_tests;
