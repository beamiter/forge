//! Read-only, bounded inspection of retained Block records. This module has no
//! PTY or submission capability: neither opening, navigation nor copying can run.
use super::{BlockData, BlockOutcome, CompletionProvenance};
use gtk4::{glib, prelude::*};
use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    rc::Rc,
};

const MAX_RECORDS: usize = 64;
const MAX_COMMAND_BYTES: usize = 256 * 1024;
const MAX_FIELD_BYTES: usize = 16 * 1024;

const MAX_OUTPUT_REVIEW_CHOICES: usize = 256;

struct OutputReviewChoice {
    id: u64,
    command: String,
    context: String,
}

fn output_review_choices(
    records: &super::BackendRecords<'_>,
    selected: &HashSet<u64>,
) -> Result<(Vec<OutputReviewChoice>, usize), String> {
    if selected.len() > super::export::MAX_SELECTED_RECORDS {
        return Err("Select at most 64 records before opening output review.".into());
    }
    let mut total = 0usize;
    let mut choices = Vec::new();
    for record in records.iter().rev() {
        total += 1;
        // Keep an explicitly selected older command visible even when the
        // retained session has more candidates than the picker budget.
        if total > MAX_OUTPUT_REVIEW_CHOICES && !selected.contains(&record.id()) {
            continue;
        }
        let status = if record.is_background() {
            "background output".to_string()
        } else {
            record
                .exit_code()
                .map_or_else(|| "exit unknown".to_string(), |code| format!("exit {code}"))
        };
        let output = match record.output() {
            None => "output unavailable",
            Some(_) if super::export::output_is_partial(record) => "partial retained output",
            Some(_) => "retained output",
        };
        let cwd = record.cwd().map_or_else(
            || "directory unknown".to_string(),
            |cwd| jterm_core::review_input::safe_inline_display(cwd, 512),
        );
        choices.push(OutputReviewChoice {
            id: record.id(),
            command: if record.is_background() {
                "Background output".to_string()
            } else {
                jterm_core::review_input::safe_inline_display(record.command(), 512)
            },
            context: format!("#{id} · {status} · {output} · {cwd}", id = record.id()),
        });
    }
    choices.reverse();
    let present: HashSet<_> = choices.iter().map(|choice| choice.id).collect();
    let mut missing: Vec<_> = selected.difference(&present).copied().collect();
    missing.sort_unstable();
    // Keep retired selections explicit and removable. Reopening the picker
    // must not silently replace them or trap the user with a stale cursor.
    for id in missing.into_iter().rev() {
        choices.insert(
            0,
            OutputReviewChoice {
                id,
                command: "Selected record is no longer retained".into(),
                context: format!("#{id} · unavailable · deselect this record to continue"),
            },
        );
    }
    Ok((choices, total))
}

fn comparison_document(
    records: &super::BackendRecords<'_>,
    selected: &HashSet<u64>,
) -> Result<(String, String), String> {
    if selected.len() != 2 {
        return Err("Select exactly two completed commands to compare their output.".into());
    }
    let pair =
        super::export::selected_records(records, selected).map_err(|error| error.to_string())?;
    if pair.iter().any(|record| record.is_background()) {
        return Err("Choose two command records; background output has no command result.".into());
    }
    let before = pair[0];
    let after = pair[1];
    let before_output = before.output().ok_or_else(|| {
        format!(
            "Record #{} has no retained output snapshot. Nothing compared.",
            before.id()
        )
    })?;
    let after_output = after.output().ok_or_else(|| {
        format!(
            "Record #{} has no retained output snapshot. Nothing compared.",
            after.id()
        )
    })?;
    let compared = jterm_core::output_compare::compare(
        jterm_core::output_compare::OutputSnapshot {
            text: before_output,
            truncated: super::export::output_is_partial(before),
        },
        jterm_core::output_compare::OutputSnapshot {
            text: after_output,
            truncated: super::export::output_is_partial(after),
        },
    );
    let describe = |role: &str, record: super::BackendRecordRef<'_>| {
        let command = jterm_core::review_input::safe_multiline_display(record.command(), 4096);
        let cwd = record.cwd().map_or_else(
            || "unknown".to_string(),
            |cwd| jterm_core::review_input::safe_inline_display(cwd, 4096),
        );
        let status = record
            .exit_code()
            .map_or_else(|| "unknown".to_string(), |code| code.to_string());
        format!(
            "{role} · record #{}\n{command}\nDirectory: {cwd}\nExit: {status}\n",
            record.id()
        )
    };
    let text = format!(
        "{}\n{}\n{}\n\n{}",
        describe("Before (older)", before),
        describe("After (newer)", after),
        compared.summary(),
        compared.text,
    );
    Ok((compared.summary(), text))
}

struct OutputReviewControls {
    owner: std::rc::Weak<super::TermView>,
    selected: RefCell<HashSet<u64>>,
    status: glib::WeakRef<gtk4::Label>,
    result: glib::WeakRef<gtk4::TextView>,
    compare: glib::WeakRef<gtk4::Button>,
    exports: [glib::WeakRef<gtk4::Button>; 2],
}

impl OutputReviewControls {
    fn owner(&self) -> Result<Rc<super::TermView>, String> {
        self.owner
            .upgrade()
            .filter(|owner| owner.widget().root().is_some_and(|root| root.is_realized()))
            .ok_or_else(|| {
                "The source pane is no longer available. Reopen review from a retained pane.".into()
            })
    }

    fn message(&self, message: &str) {
        if let Some(status) = self.status.upgrade() {
            status.set_text(message);
        }
    }

    fn selection_changed(&self) {
        let count = self.selected.borrow().len();
        if let Some(compare) = self.compare.upgrade() {
            compare.set_sensitive(count == 2);
        }
        for export in &self.exports {
            if let Some(export) = export.upgrade() {
                export.set_sensitive((1..=super::export::MAX_SELECTED_RECORDS).contains(&count));
            }
        }
        // A report always belongs to one exact pair. Changing the selection
        // retires it immediately so it cannot be mistaken for the new pair.
        if let Some(result) = self.result.upgrade() {
            result.buffer().set_text("");
        }
        self.message(&format!(
            "{count} selected · choose exactly 2 to compare, or 1–64 to export."
        ));
    }

    fn compare(&self) {
        let comparison = self.owner().and_then(|owner| {
            comparison_document(&owner.render_backend.records(), &self.selected.borrow())
        });
        match comparison {
            Ok((summary, text)) => {
                if let Some(result) = self.result.upgrade() {
                    result.buffer().set_text(&text);
                }
                self.message(&summary);
            }
            Err(error) => {
                if let Some(result) = self.result.upgrade() {
                    result.buffer().set_text("");
                }
                self.message(&error);
            }
        }
    }

    fn export(&self, format: super::SessionExportFormat) {
        let result = self.owner().and_then(|owner| {
            owner
                .export_record_selection_to_file(&self.selected.borrow(), format)
                .map_err(|error| error.to_string())
        });
        match result {
            Ok(path) => self.message(&format!(
                "Exported {} selected records to {}",
                self.selected.borrow().len(),
                jterm_core::review_input::safe_inline_display(&path.to_string_lossy(), 4096),
            )),
            Err(error) => self.message(&error),
        }
    }
}

impl super::TermView {
    pub(crate) fn export_selected_records_or_review(
        self: &Rc<Self>,
        format: super::SessionExportFormat,
    ) -> Result<Option<std::path::PathBuf>, String> {
        let mut selected = self.selected_block_ids.borrow().clone();
        if selected.is_empty() {
            selected.extend(self.navigated_record_id.get());
        }
        if selected.is_empty() {
            self.show_command_output_review()?;
            return Ok(None);
        }
        self.export_record_selection_to_file(&selected, format)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    /// Backend-neutral, stable-ID review. The native window owns only small
    /// metadata and weak pane references; opening it cannot change terminal
    /// selection, scroll, PTY input, or the live grid's dimensions.
    pub(crate) fn show_command_output_review(self: &Rc<Self>) -> Result<(), String> {
        let parent = self
            .widget()
            .root()
            .and_downcast::<gtk4::Window>()
            .ok_or_else(|| "The source pane is no longer available.".to_string())?;
        let mut selected = self.selected_block_ids.borrow().clone();
        if selected.is_empty() {
            if let Some(id) = self.navigated_record_id.get() {
                selected.insert(id);
            }
        }
        let (choices, total) = output_review_choices(&self.render_backend.records(), &selected)?;
        if choices.is_empty() {
            return Err("No completed command records are retained in this pane yet.".into());
        }
        let window = gtk4::Window::builder()
            .title("Review command output")
            .transient_for(&parent)
            .modal(true)
            .destroy_with_parent(true)
            .default_width(parent.width().clamp(420, 860))
            .default_height(parent.height().clamp(420, 780))
            .build();
        crate::ui::reference_shell::style_workspace_dialog(&window);
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        root.set_margin_start(16);
        root.set_margin_end(16);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        let heading = gtk4::Label::new(Some("Review command output"));
        heading.add_css_class("title-2");
        heading.set_xalign(0.0);
        root.append(&heading);
        let scope = gtk4::Label::new(Some(&format!(
            "{total} retained records · showing up to {MAX_OUTPUT_REVIEW_CHOICES} recent records plus the current selection, oldest to newest. Comparisons stay frozen until you compare again; missing or partial output is identified. Nothing is inserted or run.",
        )));
        scope.set_wrap(true);
        scope.set_xalign(0.0);
        root.append(&scope);
        let filter = gtk4::SearchEntry::new();
        filter.set_placeholder_text(Some("Filter by command, directory, or record ID…"));
        root.append(&filter);
        let list = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        let list_scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .min_content_height(120)
            .max_content_height(240)
            .vexpand(true)
            .child(&list)
            .build();
        root.append(&list_scroll);
        let buttons = gtk4::FlowBox::new();
        buttons.set_selection_mode(gtk4::SelectionMode::None);
        buttons.set_min_children_per_line(1);
        buttons.set_max_children_per_line(3);
        buttons.set_row_spacing(4);
        buttons.set_column_spacing(4);
        let compare = gtk4::Button::with_label("Compare 2 outputs");
        compare.add_css_class("suggested-action");
        let markdown = gtk4::Button::with_label("Export selected Markdown");
        let json = gtk4::Button::with_label("Export selected JSON");
        for button in [&compare, &markdown, &json] {
            buttons.insert(button, -1);
        }
        root.append(&buttons);
        let status = gtk4::Label::new(None);
        status.set_wrap(true);
        status.set_selectable(true);
        status.set_xalign(0.0);
        status.set_accessible_role(gtk4::AccessibleRole::Status);
        root.append(&status);
        let result = read_only_text("");
        result.update_property(&[gtk4::accessible::Property::Label(
            "Selected command output comparison",
        )]);
        let result_scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .min_content_height(140)
            .vexpand(true)
            .child(&result)
            .build();
        root.append(&result_scroll);
        let controls = Rc::new(OutputReviewControls {
            owner: Rc::downgrade(self),
            selected: RefCell::new(selected),
            status: status.downgrade(),
            result: result.downgrade(),
            compare: compare.downgrade(),
            exports: [markdown.downgrade(), json.downgrade()],
        });
        let mut searchable = Vec::new();
        for choice in choices {
            let check = gtk4::CheckButton::new();
            let text = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
            for (value, dim) in [(&choice.command, false), (&choice.context, true)] {
                let label = gtk4::Label::new(Some(value));
                label.set_xalign(0.0);
                label.set_wrap(true);
                label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
                label.set_max_width_chars(72);
                if dim {
                    label.add_css_class("dim-label");
                }
                text.append(&label);
            }
            check.set_child(Some(&text));
            check.update_property(&[gtk4::accessible::Property::Label(&format!(
                "{} · {}",
                choice.command, choice.context
            ))]);
            check.set_active(controls.selected.borrow().contains(&choice.id));
            let controls = controls.clone();
            check.connect_toggled(move |check| {
                {
                    let mut selected = controls.selected.borrow_mut();
                    if check.is_active() {
                        selected.insert(choice.id);
                    } else {
                        selected.remove(&choice.id);
                    }
                }
                controls.selection_changed();
            });
            searchable.push((
                check.downgrade(),
                format!("{} {}", choice.command, choice.context).to_lowercase(),
            ));
            list.append(&check);
        }
        filter.connect_search_changed(move |filter| {
            let query = filter.text().to_lowercase();
            for (check, text) in &searchable {
                if let Some(check) = check.upgrade() {
                    check.set_visible(query.is_empty() || text.contains(&query));
                }
            }
        });
        {
            let controls = controls.clone();
            compare.connect_clicked(move |_| controls.compare());
        }
        for (button, format) in [
            (&markdown, super::SessionExportFormat::Markdown),
            (&json, super::SessionExportFormat::Json),
        ] {
            let controls = controls.clone();
            button.connect_clicked(move |_| controls.export(format));
        }
        controls.selection_changed();
        if controls.selected.borrow().len() == 2 {
            controls.compare();
        }
        let close = gtk4::Button::with_label("Close");
        close.set_halign(gtk4::Align::End);
        let weak = window.downgrade();
        close.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.close();
            }
        });
        root.append(&close);
        let keys = gtk4::EventControllerKey::new();
        let weak = window.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gtk4::gdk::Key::Escape {
                if let Some(window) = weak.upgrade() {
                    window.close();
                }
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(keys);
        window.set_child(Some(&root));
        window.present();
        filter.grab_focus();
        Ok(())
    }
}

#[derive(Debug)]
struct ReviewRecord {
    id: u64,
    command: String,
    description: String,
    copy_safe: bool,
    capture: &'static str,
}

#[derive(Debug)]
struct ReviewSnapshot {
    records: Vec<ReviewRecord>,
    selected_count: usize,
    incomplete: bool,
}

fn field(text: &str) -> String {
    jterm_core::review_input::safe_multiline_display(text, MAX_FIELD_BYTES)
}

impl ReviewSnapshot {
    fn capture(blocks: &VecDeque<BlockData>, selected: &HashSet<u64>) -> Self {
        let mut records = Vec::new();
        let mut bytes = 0usize;
        for block in blocks.iter().filter(|block| selected.contains(&block.id)) {
            if records.len() == MAX_RECORDS
                || block.cmd.len() > MAX_COMMAND_BYTES.saturating_sub(bytes)
            {
                break;
            }
            bytes += block.cmd.len();
            let background = block.is_background();
            let result = match BlockOutcome::classify(Some(&block.cmd), block.exit_code) {
                BlockOutcome::Background => "Background output (no command result)".to_string(),
                BlockOutcome::Success => "Success · exit 0".to_string(),
                BlockOutcome::Interrupted(code) if jterm_core::exit_status::is_job_stop(code) => {
                    format!("Suspended · exit {code}")
                }
                BlockOutcome::Interrupted(code) => format!("Interrupted · exit {code}"),
                BlockOutcome::Failure(code) => format!("Failed · exit {code}"),
                BlockOutcome::Unknown => "Unknown · no exit status retained".to_string(),
            };
            let duration = if background {
                "Not applicable".to_string()
            } else if block.timing_is_authoritative() {
                block
                    .duration_ms
                    .map_or_else(|| "Not recorded".to_string(), |ms| format!("{ms} ms"))
            } else {
                "Unavailable · completion timing is not authoritative".to_string()
            };
            let provenance = match CompletionProvenance::from(block.completion_provenance) {
                CompletionProvenance::ShellReported => "Shell-reported end marker",
                CompletionProvenance::BoundaryInferred => "Inferred from a prompt boundary",
                CompletionProvenance::JournalRecovered => "Recovered from execution journal",
                _ => "Unknown completion source",
            };
            let command_source = if background {
                "No command"
            } else if block.command_truncated {
                "Truncated command report · the complete command is unavailable"
            } else if block.command_exact {
                "Exact shell-reported command"
            } else {
                "Best-effort capture · not verified as the exact command"
            };
            let output_notice = block
                .output_notice
                .as_deref()
                .map(field)
                .unwrap_or_else(|| {
                    "No output loss recorded; completeness is not independently verified".into()
                });
            let description = format!(
                "Result\n{result}\n\nWorking directory\n{}\n\nDuration\n{duration}\n\nCommand capture\n{command_source}\n\nCompletion source\n{}\n{}\n\nRetained text\n{} bytes · {} lines\n{output_notice}\nImages and output stay in the original block.",
                block.cwd.as_deref().map(field).unwrap_or_else(|| "Not recorded".into()),
                if background { "Not applicable" } else { provenance },
                block.lifecycle_notice().map_or_else(String::new, |notice| field(&notice)),
                block.output.len(), block.line_count,
            );
            let display =
                jterm_core::review_input::safe_multiline_display(&block.cmd, MAX_COMMAND_BYTES);
            records.push(ReviewRecord {
                id: block.id,
                // Retain only command and small metadata; never clone output or
                // hold a FinishedBlock, texture, VTE or virtualized card alive.
                command: block.cmd.clone(),
                description,
                copy_safe: display == block.cmd,
                capture: command_source,
            });
        }
        Self {
            incomplete: records.len() != selected.len(),
            records,
            selected_count: selected.len(),
        }
    }

    fn summary(&self) -> String {
        if self.incomplete {
            format!("Showing {} of {} selected blocks. Review limit or unavailable records: select a smaller range. Copy is disabled.", self.records.len(), self.selected_count)
        } else {
            format!(
                "{} block{} · oldest to newest · read-only snapshot",
                self.records.len(),
                if self.records.len() == 1 { "" } else { "s" }
            )
        }
    }

    fn commands_preview(&self) -> String {
        let mut text = String::new();
        for (index, record) in self.records.iter().enumerate() {
            text.push_str(&format!(
                "{}. Block #{} · {}\n",
                index + 1,
                record.id,
                record.capture
            ));
            if record.command.trim().is_empty() {
                text.push_str("[Background output: no command]\n\n");
            } else {
                text.push_str(&jterm_core::review_input::safe_multiline_display(
                    &record.command,
                    MAX_COMMAND_BYTES,
                ));
                text.push_str("\n\n");
            }
        }
        text
    }

    fn copy_commands(&self, blocks: &VecDeque<BlockData>) -> Result<String, &'static str> {
        if self.incomplete {
            return Err("Review is incomplete. Select a smaller range; nothing copied.");
        }
        let mut text = String::new();
        for record in &self.records {
            let Some(current) = blocks.iter().find(|block| block.id == record.id) else {
                return Err("A reviewed block is no longer retained. Close and review again; nothing copied.");
            };
            if current.cmd != record.command {
                return Err("A reviewed command changed. Close and review again; nothing copied.");
            }
            if !record.copy_safe {
                return Err(
                    "A command contains hidden controls or display substitutions; nothing copied.",
                );
            }
            if record.command.trim().is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&record.command);
        }
        if text.is_empty() {
            Err("These blocks contain no commands; nothing copied.")
        } else {
            Ok(text)
        }
    }
}

fn read_only_text(text: &str) -> gtk4::TextView {
    let view = gtk4::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    view.buffer().set_text(text);
    view
}

/// A native transient inspector does not resize the terminal, change its PTY
/// grid, unvirtualize history, or move its scroll anchor. The caller retains a
/// weak slot, so repeated activation raises this window rather than stacking it.
pub(super) fn open(
    parent: &gtk4::Widget,
    blocks: Rc<RefCell<VecDeque<BlockData>>>,
    selected: &HashSet<u64>,
    active_id: u64,
    focus: &vte4::Terminal,
) -> Option<gtk4::Window> {
    let owner = parent.root()?.downcast::<gtk4::Window>().ok()?;
    // Only remember an invoking action inside this card, never a live prompt
    // that happened to retain focus during pointer/menu activation. Weak refs
    // must not keep virtualized history alive.
    let invoker = gtk4::prelude::GtkWindowExt::focus(&owner)
        .filter(|widget| widget == parent || widget.is_ancestor(parent))
        .unwrap_or_else(|| parent.clone())
        .downgrade();
    let card = parent.downgrade();
    let history_scroll = parent
        .ancestor(gtk4::ScrolledWindow::static_type())
        .and_downcast::<gtk4::ScrolledWindow>()
        .map(|scroll| scroll.downgrade());
    let snapshot = Rc::new(ReviewSnapshot::capture(&blocks.borrow(), selected));
    let window = gtk4::Window::builder()
        .title("Block review")
        .transient_for(&owner)
        .modal(true)
        .destroy_with_parent(true)
        .default_width(owner.width().clamp(320, 760))
        .default_height(owner.height().clamp(320, 680))
        .build();
    crate::ui::reference_shell::style_workspace_dialog(&window);
    window.add_css_class("block-review-window");
    let root = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    root.set_margin_start(16);
    root.set_margin_end(16);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    let heading = gtk4::Label::new(Some("Block review"));
    heading.add_css_class("title-2");
    heading.set_xalign(0.0);
    root.append(&heading);
    let summary = gtk4::Label::new(Some(&snapshot.summary()));
    summary.set_wrap(true);
    summary.set_xalign(0.0);
    summary.add_css_class("dim-label");
    root.append(&summary);

    let stack = gtk4::Stack::new();
    stack.set_vexpand(true);
    stack.set_hhomogeneous(false);
    stack.set_vhomogeneous(false);
    let switcher = gtk4::StackSwitcher::builder()
        .stack(&stack)
        .halign(gtk4::Align::Start)
        .build();
    root.append(&switcher);
    let details = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    let choices: Vec<String> = snapshot
        .records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let cmd = if record.command.trim().is_empty() {
                "Background output"
            } else {
                &record.command
            };
            format!(
                "{}. #{} · {}",
                index + 1,
                record.id,
                jterm_core::review_input::safe_inline_display(cmd, 64)
            )
        })
        .collect();
    let strings: Vec<&str> = choices.iter().map(String::as_str).collect();
    let picker = gtk4::DropDown::from_strings(&strings);
    picker.set_tooltip_text(Some("Review a selected block in terminal order"));
    picker.update_property(&[gtk4::accessible::Property::Label("Selected block")]);
    // A long command must not dictate the minimum width of this native pane.
    let factory = gtk4::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
        let label = gtk4::Label::new(None);
        label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        label.set_max_width_chars(24);
        label.set_xalign(0.0);
        item.set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk4::ListItem>().expect("list item");
        let label = item.child().and_downcast::<gtk4::Label>().expect("label");
        let value = item
            .item()
            .and_downcast::<gtk4::StringObject>()
            .expect("string");
        label.set_text(&value.string());
    });
    picker.set_factory(Some(&factory));
    picker.set_list_factory(Some(&factory));
    details.append(&picker);
    let detail_text = read_only_text(
        "The selected commands exceed the review budget. Close and select a smaller range.",
    );
    detail_text.set_monospace(false);
    detail_text.update_property(&[gtk4::accessible::Property::Label(
        "Command and capture details",
    )]);
    let detail_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&detail_text)
        .build();
    details.append(&detail_scroll);
    let update: Rc<dyn Fn(usize)> = {
        let snapshot = snapshot.clone();
        let buffer = detail_text.buffer();
        let heading_tag = buffer
            .create_tag(Some("review-heading"), &[("weight", &700i32)])
            .expect("heading tag");
        let command_tag = buffer
            .create_tag(Some("review-command"), &[("family", &"monospace")])
            .expect("command tag");
        Rc::new(move |index| {
            if let Some(record) = snapshot.records.get(index) {
                let cmd = if record.command.trim().is_empty() {
                    "[No command]".into()
                } else {
                    jterm_core::review_input::safe_multiline_display(
                        &record.command,
                        MAX_COMMAND_BYTES,
                    )
                };
                let prefix = format!("Command\n{cmd}\n\n");
                buffer.set_text(&format!("{prefix}{}", record.description));
                buffer.apply_tag(
                    &heading_tag,
                    &buffer.start_iter(),
                    &buffer.iter_at_offset(7),
                );
                buffer.apply_tag(
                    &command_tag,
                    &buffer.iter_at_offset(8),
                    &buffer.iter_at_offset(8 + cmd.chars().count() as i32),
                );
                for heading in [
                    "Result",
                    "Working directory",
                    "Duration",
                    "Command capture",
                    "Completion source",
                    "Retained text",
                ] {
                    if let Some(byte) = record.description.find(&format!("{heading}\n")) {
                        let offset =
                            prefix.chars().count() + record.description[..byte].chars().count();
                        buffer.apply_tag(
                            &heading_tag,
                            &buffer.iter_at_offset(offset as i32),
                            &buffer.iter_at_offset((offset + heading.chars().count()) as i32),
                        );
                    }
                }
            }
        })
    };
    let active = snapshot
        .records
        .iter()
        .position(|record| record.id == active_id)
        .unwrap_or(0);
    picker.set_selected(active as u32);
    update(active);
    picker.connect_selected_notify(move |picker| update(picker.selected() as usize));
    stack.add_titled(&details, Some("details"), "Details");
    let preview = read_only_text(&snapshot.commands_preview());
    preview.update_property(&[gtk4::accessible::Property::Label(
        "Commands in oldest to newest order",
    )]);
    let preview_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&preview)
        .build();
    stack.add_titled(&preview_scroll, Some("commands"), "Command order");
    root.append(&stack);
    let feedback = gtk4::Label::new(Some(
        if snapshot.records.iter().any(|record| !record.copy_safe) {
            "Hidden controls are shown safely. Copy is disabled because displayed and original commands differ."
        } else {
            "Copy preserves command order and never inserts or runs anything."
        },
    ));
    feedback.set_wrap(true);
    feedback.set_xalign(0.0);
    feedback.add_css_class("dim-label");
    root.append(&feedback);
    let footer = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let copy = gtk4::Button::with_label("Copy commands");
    copy.set_sensitive(
        !snapshot.incomplete
            && snapshot.records.iter().all(|record| record.copy_safe)
            && snapshot
                .records
                .iter()
                .any(|record| !record.command.trim().is_empty()),
    );
    let feedback_for_copy = feedback.clone();
    copy.connect_clicked(
        move |button| match snapshot.copy_commands(&blocks.borrow()) {
            Ok(text) => {
                button.clipboard().set_text(&text);
                feedback_for_copy.set_text("Commands copied. Nothing inserted or run.");
            }
            Err(reason) => {
                feedback_for_copy.set_text(reason);
                button.set_sensitive(false);
            }
        },
    );
    footer.append(&copy);
    let close = gtk4::Button::with_label("Close");
    close.set_hexpand(true);
    close.set_halign(gtk4::Align::End);
    let weak = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = weak.upgrade() {
            window.close();
        }
    });
    footer.append(&close);
    root.append(&footer);
    window.set_child(Some(&root));
    let keys = gtk4::EventControllerKey::new();
    let weak = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk4::gdk::Key::Escape {
            if let Some(window) = weak.upgrade() {
                window.close();
            }
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(keys);
    let focus = focus.downgrade();
    let owner = owner.downgrade();
    window.connect_close_request(move |_| {
        let focus = focus.clone();
        let owner = owner.clone();
        let invoker = invoker.clone();
        let card = card.clone();
        // GTK may scroll a focused descendant into view when the owner becomes
        // active again. Capture before closing restores native owner focus.
        let scroll_position = history_scroll
            .as_ref()
            .and_then(|scroll| scroll.upgrade())
            .map(|scroll| {
                let horizontal = scroll.hadjustment();
                let vertical = scroll.vadjustment();
                (
                    horizontal.downgrade(),
                    horizontal.value(),
                    vertical.downgrade(),
                    vertical.value(),
                )
            });
        glib::idle_add_local_once(move || {
            if let Some(owner) = owner.upgrade().filter(|owner| owner.is_visible()) {
                owner.present();
                // Menu items disappear on activation, so prefer their retained
                // history card before falling back to the live terminal.
                let target = invoker
                    .upgrade()
                    .filter(|widget| widget.is_mapped())
                    .or_else(|| card.upgrade().filter(|widget| widget.is_mapped()));
                if let Some(target) = target {
                    target.grab_focus();
                    if let Some((horizontal, x, vertical, y)) = scroll_position {
                        if let Some(horizontal) = horizontal.upgrade() {
                            horizontal.set_value(x);
                        }
                        if let Some(vertical) = vertical.upgrade() {
                            vertical.set_value(y);
                        }
                    }
                } else if let Some(focus) = focus.upgrade().filter(|focus| focus.is_mapped()) {
                    crate::terminal::focus_terminal(&focus);
                }
            }
        });
        glib::Propagation::Proceed
    });
    window.present();
    close.grab_focus();
    Some(window)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(id: u64, command: &str) -> BlockData {
        BlockData {
            id,
            prompt: "$ ".into(),
            cmd: command.into(),
            cmd_markup: None,
            output: String::new(),
            exit_code: None,
            lifecycle_schema: super::super::blocks::BLOCK_LIFECYCLE_SCHEMA,
            completion_provenance: CompletionProvenance::Unknown.into(),
            start_mark_seen: false,
            estimated_height: 100,
            line_count: 0,
            start_time_ms: None,
            end_time_ms: None,
            duration_ms: None,
            cwd: None,
            cols: 80,
            command_exact: false,
            command_truncated: false,
            output_notice: None,
        }
    }

    #[test]
    fn output_comparison_uses_temporal_order_metadata_and_real_retained_changes() {
        let mut before = record(80, "cargo check");
        before.output = "building\nold failure\n".into();
        before.cwd = Some("/project/older".into());
        before.exit_code = Some(1);
        let mut after = record(3, "cargo check");
        after.output = "building\nfinished\n".into();
        after.cwd = Some("/project/newer".into());
        after.exit_code = Some(0);
        let blocks = RefCell::new(VecDeque::from([before, record(4, "unselected"), after]));
        let records = super::super::BackendRecords::Blocks(blocks.borrow());
        let (summary, document) = comparison_document(&records, &HashSet::from([3, 80])).unwrap();
        assert!(summary.contains("+1 added / -1 removed / 1 unchanged"));
        assert!(document.contains("Before (older) · record #80"));
        assert!(document.contains("After (newer) · record #3"));
        assert!(document.contains("Directory: /project/older\nExit: 1"));
        assert!(document.contains("Directory: /project/newer\nExit: 0"));
        assert!(document.contains("-old failure"));
        assert!(document.contains("+finished"));
        assert!(!document.contains("unselected"));
        assert!(comparison_document(&records, &HashSet::from([3])).is_err());
        assert!(comparison_document(&records, &HashSet::from([3, 99])).is_err());
    }

    #[test]
    fn output_comparison_propagates_capture_limits_and_accepts_available_empty_output() {
        let mut partial = record(1, "before");
        partial.output_notice = Some(super::super::FINISHED_OUTPUT_NOT_RETAINED.into());
        let blocks = RefCell::new(VecDeque::from([partial, record(2, "after")]));
        let records = super::super::BackendRecords::Blocks(blocks.borrow());
        let (summary, _) = comparison_document(&records, &HashSet::from([1, 2])).unwrap();
        assert!(summary.contains("+0 added / -0 removed / 0 unchanged"));
        assert!(summary.contains("partial captured output"));
    }

    #[test]
    fn unified_comparison_never_substitutes_empty_text_for_a_missing_snapshot() {
        let mut store = super::super::UnifiedZoneStore::new();
        for id in [9, 2] {
            store
                .records
                .push_back(super::super::CompletedCommandRecord {
                    id,
                    cmd: format!("command {id}"),
                    exit_code: Some(0),
                    start_time_ms: None,
                    end_time_ms: None,
                    duration_ms: None,
                    cwd: None,
                    is_background: false,
                    completion_provenance: CompletionProvenance::Unknown,
                    command_source: super::super::CommandTextSource::Screen,
                    start_mark_seen: false,
                });
        }
        store.insert_snapshot(
            2,
            super::super::ZoneOutputSnapshot {
                plain: String::new(),
                truncated: false,
            },
        );
        let store = RefCell::new(store);
        let records = super::super::BackendRecords::Metadata(store.borrow());
        let error = comparison_document(&records, &HashSet::from([2, 9])).unwrap_err();
        assert!(error.contains("Record #9 has no retained output snapshot"));
    }

    #[test]
    fn output_picker_keeps_selected_older_and_retired_ids_without_neighbour_fallback() {
        let blocks: VecDeque<_> = (1..=300).map(|id| record(id, "same command")).collect();
        let blocks = RefCell::new(blocks);
        let records = super::super::BackendRecords::Blocks(blocks.borrow());
        let (choices, total) = output_review_choices(&records, &HashSet::from([1, 999])).unwrap();
        assert_eq!(total, 300);
        assert_eq!(choices.len(), MAX_OUTPUT_REVIEW_CHOICES + 2);
        assert_eq!(choices[0].id, 999);
        assert!(choices[0].context.contains("unavailable"));
        assert_eq!(choices[1].id, 1);
        assert_eq!(choices[2].id, 45);
        assert_eq!(choices.last().unwrap().id, 300);
        assert!(!choices.iter().any(|choice| choice.id == 2));
    }

    #[test]
    #[ignore = "requires an isolated GTK/VTE display; run explicitly under Xvfb"]
    fn selected_output_review_is_read_only_and_owner_bound() {
        use super::super::find::{palette_activation_test_feed, palette_activation_test_view};
        use crate::config::TerminalMode;
        use std::time::{Duration, Instant};

        fn settle() {
            let context = glib::MainContext::default();
            let until = Instant::now() + Duration::from_millis(100);
            while Instant::now() < until {
                for _ in 0..128 {
                    if !context.iteration(false) {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        fn descendants(root: &gtk4::Widget) -> Vec<gtk4::Widget> {
            let mut out = vec![root.clone()];
            let mut index = 0;
            while index < out.len() {
                let mut child = out[index].first_child();
                while let Some(widget) = child {
                    child = widget.next_sibling();
                    out.push(widget);
                }
                index += 1;
            }
            out
        }

        gtk4::init().unwrap();
        for mode in [TerminalMode::Block, TerminalMode::Unified] {
            let view = palette_activation_test_view(mode);
            let owner = gtk4::Window::builder()
                .default_width(800)
                .default_height(680)
                .child(&view.widget())
                .build();
            owner.present();
            palette_activation_test_feed(&view, b"\x1b]133;A\x07$ \x1b]133;B\x07");
            settle();
            for (command, output) in [
                ("check-before", "old result"),
                ("check-after", "new result"),
            ] {
                palette_activation_test_feed(&view, format!(
                    "{command}\r\n\x1b]133;C;cmdline={command}\x07{output}\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07"
                ).as_bytes());
                settle();
            }
            let records: Vec<_> = view
                .render_backend
                .records()
                .iter()
                .filter(|record| !record.is_background())
                .map(|record| record.id())
                .collect();
            assert_eq!(records.len(), 2);
            let before_selection = view.selected_block_ids.borrow().clone();
            let before_unread = view.unread_count.get();
            let before_grid = view.pty.test_slave_winsize();
            let before_scroll = view.active_vte.vadjustment().unwrap().value();
            view.pty.drain_test_slave(Duration::from_millis(30));
            view.show_command_output_review().unwrap();
            settle();
            let review = gtk4::Window::list_toplevels()
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk4::Window>().ok())
                .find(|window| window.title().as_deref() == Some("Review command output"))
                .unwrap();
            let widgets = descendants(review.upcast_ref());
            let checks: Vec<_> = widgets
                .iter()
                .filter_map(|widget| widget.clone().downcast::<gtk4::CheckButton>().ok())
                .collect();
            assert_eq!(checks.len(), 2);
            for check in checks {
                check.set_active(true);
            }
            let compare = widgets
                .iter()
                .filter_map(|widget| widget.clone().downcast::<gtk4::Button>().ok())
                .find(|button| button.label().as_deref() == Some("Compare 2 outputs"))
                .unwrap();
            assert!(compare.is_sensitive());
            compare.emit_clicked();
            let text = widgets
                .iter()
                .find_map(|widget| widget.clone().downcast::<gtk4::TextView>().ok())
                .unwrap();
            assert!(!text.is_editable());
            let buffer = text.buffer();
            let displayed = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            assert!(displayed.contains("-old result"));
            assert!(displayed.contains("+new result"));
            assert_eq!(*view.selected_block_ids.borrow(), before_selection);
            assert_eq!(view.unread_count.get(), before_unread);
            assert_eq!(view.pty.test_slave_winsize(), before_grid);
            assert_eq!(
                view.active_vte.vadjustment().unwrap().value(),
                before_scroll
            );
            assert!(view
                .pty
                .drain_test_slave(Duration::from_millis(30))
                .is_empty());
            owner.set_child(gtk4::Widget::NONE);
            compare.emit_clicked();
            assert_eq!(
                buffer.char_count(),
                0,
                "a detached source retires its report"
            );
            review.close();
            owner.close();
            settle();
        }
    }
    #[test]
    #[ignore = "requires DISPLAY"]
    fn closing_review_preserves_scrolled_history_and_invoking_control() {
        gtk4::init().unwrap();
        let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        let invoke = gtk4::Button::with_label("Review selection");
        card.append(&invoke);
        let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        spacer.set_height_request(1600);
        card.append(&spacer);
        let live = vte4::Terminal::new();
        card.append(&live);
        let scroll = gtk4::ScrolledWindow::builder().child(&card).build();
        let owner = gtk4::Window::builder()
            .default_width(360)
            .default_height(400)
            .child(&scroll)
            .build();
        owner.present();
        let settle = || {
            let context = glib::MainContext::default();
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_millis(160) {
                while context.iteration(false) {}
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        settle();
        invoke.grab_focus();
        let records = Rc::new(RefCell::new(VecDeque::from([record(1, "true")])));
        for _ in 0..2 {
            scroll.vadjustment().set_value(500.0);
            settle();
            let position = scroll.vadjustment().value();
            assert!(position > 0.0);
            let review = open(
                card.upcast_ref(),
                records.clone(),
                &HashSet::from([1]),
                1,
                &live,
            )
            .unwrap();
            settle();
            review.close();
            settle();
            assert!(invoke.has_focus());
            assert_eq!(scroll.vadjustment().value(), position);
        }
        owner.close();
    }
    #[test]
    fn empty_and_evicted_selection_cannot_copy_or_expand_with_new_history() {
        let mut records = VecDeque::from([record(1, "echo one")]);
        let empty = ReviewSnapshot::capture(&records, &HashSet::new());
        assert!(empty.records.is_empty());
        assert!(empty.copy_commands(&records).is_err());
        let stale = ReviewSnapshot::capture(&records, &HashSet::from([99]));
        assert!(stale.incomplete);
        assert!(stale.copy_commands(&records).is_err());
        let snapshot = ReviewSnapshot::capture(&records, &HashSet::from([1]));
        records.push_back(record(2, "echo new"));
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.copy_commands(&records).unwrap(), "echo one");
    }
    #[test]
    fn review_preserves_terminal_order_and_unicode_without_background_commands() {
        let records = VecDeque::from([
            record(9, "printf '你好🌿'"),
            record(2, ""),
            record(5, "printf 'two\nlines'"),
        ]);
        let review = ReviewSnapshot::capture(&records, &HashSet::from([5, 2, 9]));
        assert_eq!(
            review.records.iter().map(|r| r.id).collect::<Vec<_>>(),
            [9, 2, 5]
        );
        assert_eq!(
            review.copy_commands(&records).unwrap(),
            "printf '你好🌿'\nprintf 'two\nlines'"
        );
        assert!(review
            .commands_preview()
            .contains("[Background output: no command]"));
    }
    #[test]
    fn review_does_not_invent_success_timing_cwd_or_capture_completeness() {
        let mut data = record(1, "false");
        data.duration_ms = Some(42);
        let review = ReviewSnapshot::capture(&VecDeque::from([data]), &HashSet::from([1]));
        let details = &review.records[0].description;
        assert!(details.contains("Unknown · no exit status retained"));
        assert!(details.contains("Working directory\nNot recorded"));
        assert!(details.contains("completion timing is not authoritative"));
        assert!(!details.contains("42 ms"));
        assert!(details.contains("completeness is not independently verified"));
    }
    #[test]
    fn review_job_control_stops_match_the_suspended_card_status() {
        for code in 147..=150 {
            let mut data = record(1, "sleep 10");
            data.exit_code = Some(code);
            let review = ReviewSnapshot::capture(&VecDeque::from([data]), &HashSet::from([1]));
            let details = &review.records[0].description;
            assert!(details.contains(&format!("Suspended · exit {code}")));
            assert!(!details.contains("Failed"));
        }
    }
    #[test]
    fn review_exposes_exactness_truncation_and_retained_output_without_copying_it() {
        let mut data = record(1, "build");
        data.exit_code = Some(7);
        data.command_exact = true;
        data.command_truncated = true;
        data.output = "x".repeat(8 * 1024 * 1024);
        data.output_notice = Some("Earlier output not retained".into());
        data.completion_provenance = CompletionProvenance::ShellReported.into();
        data.start_mark_seen = true;
        data.duration_ms = Some(1200);
        let review = ReviewSnapshot::capture(&VecDeque::from([data]), &HashSet::from([1]));
        let details = &review.records[0].description;
        assert!(details.contains("Failed · exit 7"));
        assert!(details.contains("1200 ms"));
        assert!(details.contains("Truncated command report"));
        assert!(details.contains("8388608 bytes"));
        assert!(details.contains("Earlier output not retained"));
        assert!(details.len() < 1024);
    }
    #[test]
    fn review_limits_fail_closed_instead_of_copying_a_partial_selection() {
        let records: VecDeque<_> = (0..65).map(|id| record(id, "echo ok")).collect();
        let selected = records.iter().map(|r| r.id).collect();
        let review = ReviewSnapshot::capture(&records, &selected);
        assert_eq!(review.records.len(), MAX_RECORDS);
        assert!(review.summary().contains("64 of 65"));
        assert!(review.copy_commands(&records).is_err());
        let records = VecDeque::from([record(1, &"x".repeat(MAX_COMMAND_BYTES + 1))]);
        let review = ReviewSnapshot::capture(&records, &HashSet::from([1]));
        assert!(review.incomplete);
        assert!(review.records.is_empty());
    }
    #[test]
    fn review_copy_revalidates_all_ids_and_commands_before_publishing() {
        let mut records = VecDeque::from([record(1, "echo one"), record(2, "echo two")]);
        let review = ReviewSnapshot::capture(&records, &HashSet::from([1, 2]));
        records[1].cmd = "changed".into();
        assert!(review
            .copy_commands(&records)
            .unwrap_err()
            .contains("changed"));
        records.pop_front();
        assert!(review
            .copy_commands(&records)
            .unwrap_err()
            .contains("no longer retained"));
    }
    #[test]
    fn review_never_copies_hidden_control_or_bidi_payloads() {
        for command in ["echo \u{202e}txt", "echo ok\u{1b}[2J", "echo \0"] {
            let records = VecDeque::from([record(1, command)]);
            let review = ReviewSnapshot::capture(&records, &HashSet::from([1]));
            assert!(!review.records[0].copy_safe, "{command:?}");
            assert!(review.copy_commands(&records).is_err());
        }
    }
}
