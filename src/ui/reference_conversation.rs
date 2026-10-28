//! Native, data-driven conversation components. No terminal output is parsed
//! into invented agent activity. The shared acceptance data is explicitly
//! opt-in and uses these same widgets as future observed agent messages.
use gtk4::prelude::*;
use gtk4::Orientation;
use jterm_core::developer_workspace as token;
use jterm_core::reference_conversation::{
    CodeDiff, Conversation, DiffKind, Inspector, Message, Part, Role, Step, StepState,
    FIXTURE_DATE_SEPARATOR, FIXTURE_LABEL, METADATA_ICON_KEYS,
};

fn text(value: &str, class: &str) -> gtk4::Label {
    let label = gtk4::Label::new(Some(value));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
    label.set_tooltip_text(Some(value));
    label.add_css_class(class);
    label
}

fn step_icon(state: StepState) -> (&'static str, &'static str) {
    match state {
        StepState::Complete => ("✓", "reference-step-complete"),
        StepState::Running => ("◉", "reference-step-running"),
        StepState::Queued => ("○", "reference-step-queued"),
    }
}

fn step_row(step: &Step, details: bool) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 8);
    row.add_css_class("reference-rich-step");
    if details {
        row.add_css_class("reference-current-step");
    }
    row.set_height_request(token::TASK_ROW_HEIGHT as i32);
    let (symbol, class) = step_icon(step.state);
    row.add_css_class(class);
    row.append(&text(symbol, "reference-step-symbol"));
    let content = gtk4::Box::new(Orientation::Vertical, 3);
    content.set_hexpand(true);
    content.append(&text(&step.title, "reference-step-title"));
    if details {
        content.append(&text(&step.detail, "reference-step-detail"));
    }
    row.append(&content);
    if let Some(elapsed) = &step.elapsed {
        if elapsed != "Fixture" {
            let elapsed = text(elapsed, "reference-step-elapsed");
            elapsed.set_wrap(false);
            row.append(&elapsed);
        }
    }
    row
}

fn diff_widget(diff: &CodeDiff) -> gtk4::Box {
    let root = gtk4::Box::new(Orientation::Vertical, 0);
    root.add_css_class("reference-rich-diff");
    let header = gtk4::Box::new(Orientation::Horizontal, 6);
    header.add_css_class("reference-code-header");
    header.set_height_request(token::DIFF_HEADER_HEIGHT as i32);
    header.append(&super::reference_shell::vector_icon(
        jterm_core::reference_icons::Icon::Files,
        14,
    ));
    let path = text(&diff.path, "reference-code-path");
    path.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
    path.set_wrap(false);
    path.set_width_chars(1);
    path.set_hexpand(true);
    header.append(&path);
    let language = text(&diff.language, "reference-code-language");
    language.set_wrap(false);
    header.append(&language);
    let added = text(&format!("+{}", diff.added), "reference-diff-added");
    added.set_wrap(false);
    header.append(&added);
    let removed = text(&format!("−{}", diff.removed), "reference-diff-removed");
    removed.set_wrap(false);
    header.append(&removed);
    let copy = gtk4::Button::with_label("Copy");
    copy.add_css_class("reference-code-action");
    copy.set_tooltip_text(Some("Copy this displayed diff"));
    let full_diff = diff
        .lines
        .iter()
        .map(|line| {
            format!(
                "{}{}",
                match line.kind {
                    DiffKind::Added => "+",
                    DiffKind::Removed => "-",
                    DiffKind::Context => " ",
                },
                line.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    copy.connect_clicked(move |button| button.clipboard().set_text(&full_diff));
    header.append(&copy);
    let apply = gtk4::Button::with_label("Apply");
    // An edit target and its authorization have not been connected. Even a
    // caller-provided can_apply flag alone cannot grant file-write authority.
    apply.set_sensitive(false);
    apply.set_tooltip_text(Some(
        "Apply is unavailable until a real authorized edit target is connected",
    ));
    apply.add_css_class("reference-code-action");
    header.append(&apply);
    root.append(&header);
    let lines = gtk4::Box::new(Orientation::Vertical, 0);
    lines.set_margin_top(token::DIFF_BODY_VERTICAL_PADDING as i32);
    lines.set_margin_bottom(token::DIFF_BODY_VERTICAL_PADDING as i32);
    for line in &diff.lines {
        let row = gtk4::Box::new(Orientation::Horizontal, 0);
        row.set_height_request(token::DIFF_LINE_HEIGHT as i32);
        row.add_css_class(match line.kind {
            DiffKind::Added => "reference-code-added",
            DiffKind::Removed => "reference-code-removed",
            DiffKind::Context => "reference-code-context",
        });
        for number in [line.old_line, line.new_line] {
            let number = text(
                &number.map(|value| value.to_string()).unwrap_or_default(),
                "reference-line-number",
            );
            number.set_width_request(token::DIFF_GUTTER_COLUMN_WIDTH as i32);
            number.set_xalign(1.0);
            number.set_wrap(false);
            row.append(&number);
        }
        let sign = text(
            match line.kind {
                DiffKind::Added => "+",
                DiffKind::Removed => "−",
                DiffKind::Context => " ",
            },
            "reference-code-sign",
        );
        sign.set_width_request(token::DIFF_SIGN_WIDTH as i32);
        sign.set_xalign(0.5);
        sign.set_wrap(false);
        row.append(&sign);
        let code = text(&line.text, "reference-code-line");
        code.set_wrap(false);
        code.set_selectable(true);
        row.append(&code);
        lines.append(&row);
    }
    let scroll = gtk4::ScrolledWindow::new();
    scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Never);
    scroll.set_propagate_natural_height(true);
    scroll.set_propagate_natural_width(false);
    scroll.set_max_content_width(1);
    scroll.set_child(Some(&lines));
    root.append(&scroll);
    root
}

fn terminal_widget(title: &str, value: &str) -> gtk4::Box {
    let root = gtk4::Box::new(Orientation::Vertical, 0);
    root.add_css_class("reference-rich-terminal");
    let header = text(title, "reference-terminal-title");
    root.append(&header);
    let body = text(value, "reference-terminal-body");
    body.set_selectable(true);
    root.append(&body);
    root
}

/// Header shared by structured messages and observed shell result records.
pub(crate) fn role_header(role: Role, author: &str, timing: Option<&str>) -> gtk4::Box {
    let row = gtk4::Box::new(Orientation::Horizontal, 6);
    row.add_css_class("reference-role-header");
    row.set_height_request(16);
    row.append(&super::reference_shell::vector_icon(
        if role == Role::Shell {
            jterm_core::reference_icons::Icon::Terminal
        } else {
            jterm_core::reference_icons::Icon::Agent
        },
        16,
    ));
    let author = text(author, "reference-message-author");
    author.set_wrap(false);
    author.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    author.set_width_chars(1);
    author.set_max_width_chars(12);
    row.append(&author);
    if let Some(timing) = timing {
        row.append(&text(timing, "reference-message-timing"));
    }
    row
}

fn message_widget(message: &Message) -> gtk4::Box {
    let root = gtk4::Box::new(Orientation::Horizontal, 8);
    root.add_css_class("reference-rich-message");
    let avatar = text(&message.avatar, "reference-message-avatar");
    avatar.set_wrap(false);
    avatar.set_halign(gtk4::Align::Center);
    avatar.set_valign(gtk4::Align::Start);
    let content = gtk4::Box::new(Orientation::Vertical, token::MESSAGE_PART_GAP as i32);
    content.set_hexpand(true);
    if message.role == Role::User {
        root.add_css_class("reference-user-message");
        root.set_halign(gtk4::Align::End);
        content.set_hexpand(false);
        content.set_width_request(token::USER_BUBBLE_WIDTH as i32);
        content.set_height_request(token::USER_BUBBLE_HEIGHT as i32);
        content.add_css_class("reference-user-bubble");
    } else {
        root.add_css_class("reference-assistant-message");
        root.append(&avatar);
        let header = gtk4::Box::new(Orientation::Horizontal, 6);
        header.append(&text(&message.author, "reference-message-author"));
        if let Some(timing) = &message.timing {
            header.append(&text(&format!("·  {timing}"), "reference-message-timing"));
        }
        content.append(&header);
    }
    for part in &message.parts {
        match part {
            Part::Heading(value) => content.append(&text(value, "reference-message-heading")),
            Part::Paragraph(value) => {
                let paragraph = text(value, "reference-message-paragraph");
                if message.role == Role::User {
                    paragraph.set_max_width_chars(42);
                }
                content.append(&paragraph);
            }
            Part::Metadata(values) => {
                let metadata = gtk4::FlowBox::new();
                metadata.set_selection_mode(gtk4::SelectionMode::None);
                metadata.set_homogeneous(false);
                metadata.set_column_spacing(token::METADATA_GAP as u32);
                metadata.set_row_spacing(token::METADATA_GAP as u32);
                metadata.set_halign(gtk4::Align::Start);
                metadata.set_max_children_per_line(8);
                for (index, value) in values.iter().enumerate() {
                    let chip = gtk4::Box::new(Orientation::Horizontal, 4);
                    chip.add_css_class("reference-message-chip");
                    chip.set_height_request(token::METADATA_PILL_HEIGHT as i32);
                    chip.append(&super::reference_shell::vector_icon(
                        jterm_core::reference_icons::Icon::from_key(
                            METADATA_ICON_KEYS[index % METADATA_ICON_KEYS.len()],
                        ),
                        12,
                    ));
                    let caption = text(value, "reference-chip-text");
                    caption.set_wrap(false);
                    chip.append(&caption);
                    if index == 5 {
                        chip.add_css_class("reference-chip-success");
                    }
                    metadata.insert(&chip, -1);
                }
                content.append(&metadata);
            }
            Part::Steps(steps) => {
                let list = gtk4::Box::new(Orientation::Vertical, 1);
                list.add_css_class("reference-rich-steps");
                for step in steps {
                    list.append(&step_row(step, false));
                }
                content.append(&list);
            }
            Part::Diff(diff) => content.append(&diff_widget(diff)),
            Part::Terminal { title, text } => content.append(&terminal_widget(title, text)),
        }
    }
    root.append(&content);
    if message.role == Role::User {
        root.append(&avatar);
    }
    root
}

pub(crate) fn conversation(model: &Conversation) -> gtk4::Box {
    let root = gtk4::Box::new(Orientation::Vertical, 21);
    root.add_css_class("reference-rich-conversation");
    root.set_hexpand(true);
    // The same fixture label is always visible in the central header. Keep
    // it on this component's tooltip too, without moving the reference rows.
    if model.is_fixture {
        root.set_tooltip_text(Some(FIXTURE_LABEL));
        let date = text(FIXTURE_DATE_SEPARATOR, "reference-date-separator");
        date.set_xalign(0.5);
        date.set_height_request(9);
        root.append(&date);
    }
    for message in &model.messages {
        root.append(&message_widget(message));
    }
    root
}

pub(crate) fn inspector(model: &Inspector, is_fixture: bool) -> gtk4::Box {
    let root = gtk4::Box::new(Orientation::Vertical, 2);
    root.add_css_class("reference-agent-card");
    root.add_css_class("reference-rich-inspector");
    root.set_vexpand(true);
    let heading = gtk4::Box::new(Orientation::Horizontal, 6);
    heading.add_css_class("reference-inspector-agent-heading");
    heading.set_height_request(22);
    heading.set_margin_top(9);
    heading.append(&inspector_icon(
        jterm_core::reference_icons::Icon::Agent,
        22,
    ));
    let title = text(&model.title, "reference-card-title");
    title.set_wrap(false);
    title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    title.set_width_chars(1);
    title.set_hexpand(true);
    heading.append(&title);
    let badge = text(&model.badge, "reference-neutral-badge");
    badge.set_wrap(false);
    badge.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    heading.append(&badge);
    root.append(&heading);
    root.append(&text(&model.subtitle, "reference-muted"));
    // An idle workspace has no task progress to report. The same component
    // renders observed task progress and the explicitly labeled fixture.
    if model.total > 0 {
        let progress_row = gtk4::Box::new(Orientation::Horizontal, 6);
        progress_row.set_height_request(12);
        progress_row.set_margin_top(18);
        let progress = gtk4::ProgressBar::new();
        progress.add_css_class("reference-inspector-progress");
        progress.set_hexpand(true);
        progress.set_valign(gtk4::Align::Center);
        progress.set_fraction(model.completed as f64 / model.total as f64);
        progress_row.append(&progress);
        progress_row.append(&text(
            &format!("{} / {}", model.completed, model.total),
            "reference-muted",
        ));
        root.append(&progress_row);
    }
    let metadata = gtk4::Grid::new();
    metadata.set_column_spacing(14);
    metadata.set_row_spacing(3);
    metadata.set_margin_top(23);
    metadata.add_css_class("reference-inspector-metadata");
    for (row, (name, value)) in model.metadata.iter().enumerate() {
        let key = text(name, "reference-muted");
        key.add_css_class("reference-inspector-metadata-label");
        key.set_height_request(token::INSPECTOR_METADATA_ROW_HEIGHT as i32);
        metadata.attach(&key, 0, row as i32, 1, 1);
        let value = text(value, "reference-metadata");
        value.add_css_class("reference-inspector-metadata-value");
        value.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        value.set_wrap(false);
        value.set_width_chars(1);
        value.set_hexpand(true);
        value.set_xalign(1.0);
        metadata.attach(&value, 1, row as i32, 1, 1);
    }
    root.append(&metadata);
    let current_heading = text("CURRENT STEP", "reference-section-label");
    current_heading.add_css_class("reference-inspector-current-heading");
    current_heading.set_margin_top(21);
    current_heading.set_margin_bottom(12);
    root.append(&current_heading);
    let current = if let Some(step) = &model.current_step {
        step_row(step, true)
    } else {
        let card = inspector_empty_card(
            "Select a task to see its current step",
            jterm_core::reference_icons::Icon::History,
        );
        card.add_css_class("reference-current-step");
        card
    };
    current.set_height_request(59);
    current.add_css_class("reference-inspector-current");
    root.append(&current);
    let queue_heading = text("TASK QUEUE", "reference-section-label");
    queue_heading.add_css_class("reference-inspector-queue-heading");
    queue_heading.set_margin_top(22);
    queue_heading.set_margin_bottom(12);
    root.append(&queue_heading);
    let queue = gtk4::Box::new(Orientation::Vertical, token::INSPECTOR_QUEUE_GAP as i32);
    queue.add_css_class("reference-inspector-queue");
    if model.queue.is_empty() {
        let empty =
            inspector_empty_card("No queued tasks", jterm_core::reference_icons::Icon::Files);
        empty.set_height_request(58);
        queue.append(&empty);
    }
    for step in &model.queue {
        let row = step_row(step, false);
        row.add_css_class("reference-inspector-queue-row");
        row.set_height_request(token::INSPECTOR_QUEUE_ROW_HEIGHT as i32);
        queue.append(&row);
    }
    root.append(&queue);
    let fill = gtk4::Box::new(Orientation::Vertical, 0);
    fill.set_vexpand(true);
    root.append(&fill);
    let terminal_section = gtk4::Box::new(Orientation::Vertical, 8);
    terminal_section.add_css_class("reference-inspector-terminal-section");
    // Only the inter-section fill grows. The centered empty output's child
    // must not make this whole section share the spare vertical space.
    terminal_section.set_vexpand(false);
    let output_heading = gtk4::Box::new(Orientation::Horizontal, 6);
    output_heading.add_css_class("reference-inspector-output-heading");
    output_heading.set_height_request(12);
    output_heading.append(&super::reference_shell::vector_icon(
        jterm_core::reference_icons::Icon::Terminal,
        12,
    ));
    let title = text("Terminal output", "reference-section-label");
    title.set_hexpand(true);
    output_heading.append(&title);
    output_heading.append(&text(
        if is_fixture { "Fixture" } else { "No task" },
        "reference-muted",
    ));
    terminal_section.append(&output_heading);
    let output = gtk4::Box::new(Orientation::Vertical, 0);
    output.add_css_class("reference-rich-terminal");
    output.add_css_class("reference-inspector-output");
    let body = text(&model.output, "reference-terminal-body");
    body.set_selectable(true);
    if model.total == 0 && !is_fixture {
        output.add_css_class("reference-inspector-output-empty");
        output.set_valign(gtk4::Align::Fill);
        let content = gtk4::Box::new(Orientation::Vertical, 8);
        content.set_valign(gtk4::Align::Center);
        content.set_vexpand(true);
        let icon =
            super::reference_shell::vector_icon(jterm_core::reference_icons::Icon::Terminal, 22);
        icon.set_halign(gtk4::Align::Center);
        content.append(&icon);
        body.set_xalign(0.5);
        content.append(&body);
        output.append(&content);
    } else {
        output.append(&body);
    }
    output.set_height_request(199);
    terminal_section.append(&output);
    root.append(&terminal_section);
    root
}

fn inspector_icon(icon: jterm_core::reference_icons::Icon, size: i32) -> gtk4::CenterBox {
    let tile = gtk4::CenterBox::new();
    tile.add_css_class("reference-inspector-icon-tile");
    tile.set_size_request(size, size);
    tile.set_valign(gtk4::Align::Center);
    tile.set_center_widget(Some(&super::reference_shell::vector_icon(icon, 14)));
    tile
}

fn inspector_empty_card(message: &str, icon: jterm_core::reference_icons::Icon) -> gtk4::Box {
    let card = gtk4::Box::new(Orientation::Horizontal, 12);
    card.add_css_class("reference-inspector-empty-card");
    card.append(&inspector_icon(icon, 28));
    let label = text(message, "reference-muted");
    label.set_hexpand(true);
    card.append(&label);
    card
}
