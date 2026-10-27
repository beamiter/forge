//! Read-only exact-match preview; deliberately independent of native Find.
use super::UiState;
use crate::block_view::{
    search_preview::{self, Matches, Snapshot, Worker},
    TermView,
};
use adw::prelude::*;
use gtk4::glib;
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

/// Paint an exact range without creating a selection. The
/// display text is the same immutable string searched by the worker.
fn paint_hit(
    text: &gtk4::TextView,
    tag: &gtk4::TextTag,
    doc: &search_preview::Document,
    hit: &search_preview::Hit,
    replace_document: bool,
) -> bool {
    if hit.bytes.start >= hit.bytes.end
        || doc.text.get(hit.bytes.clone()).is_none()
        || hit.chars.start < 0
        || hit.chars.start >= hit.chars.end
    {
        return false;
    }
    let buffer = text.buffer();
    if replace_document {
        buffer.set_text(&doc.text);
    }
    if hit.chars.end > buffer.char_count() {
        return false;
    }
    let (begin, end) = buffer.bounds();
    buffer.remove_tag(tag, &begin, &end);
    let start = buffer.iter_at_offset(hit.chars.start);
    let end = buffer.iter_at_offset(hit.chars.end);
    buffer.apply_tag(tag, &start, &end);
    // GTK validates line heights in an idle. A private mark lets scrolling
    // wait for that layout after set_text; it is not either selection mark.
    let mark = buffer
        .mark("exact-preview-scroll")
        .unwrap_or_else(|| buffer.create_mark(Some("exact-preview-scroll"), &start, true));
    buffer.move_mark(&mark, &start);
    text.scroll_to_mark(&mark, 0.1, false, 0.0, 0.0);
    true
}

/// Retire the private navigation mark as soon as a query/pane/dialog retires.
/// The empty buffer also removes any old range while a new worker is pending.
fn clear_preview_buffer(text: &gtk4::TextView) {
    let buffer = text.buffer();
    if let Some(mark) = buffer.mark("exact-preview-scroll") {
        buffer.delete_mark(&mark);
    }
    buffer.set_text("");
}

struct PreviewControls {
    snapshot: Arc<Snapshot>,
    worker: Worker,
    owner: Weak<TermView>,
    closed: Cell<bool>,
    generation: Cell<u64>,
    matches: RefCell<Option<Matches>>,
    current: Cell<usize>,
    displayed_document: Cell<Option<usize>>,
    text: glib::WeakRef<gtk4::TextView>,
    status: glib::WeakRef<gtk4::Label>,
    location: glib::WeakRef<gtk4::Label>,
    previous: glib::WeakRef<gtk4::Button>,
    next: glib::WeakRef<gtk4::Button>,
    tag: gtk4::TextTag,
    #[cfg(test)]
    poll_calls: Cell<usize>,
    #[cfg(test)]
    rejected_replies: Cell<usize>,
}
impl PreviewControls {
    fn clear(&self, message: &str) {
        self.matches.borrow_mut().take();
        self.displayed_document.set(None);
        if let Some(text) = self.text.upgrade() {
            clear_preview_buffer(&text);
        }
        if let Some(status) = self.status.upgrade() {
            status.set_text(message);
        }
        if let Some(location) = self.location.upgrade() {
            location.set_text("");
        }
        for button in [&self.previous, &self.next] {
            if let Some(button) = button.upgrade() {
                button.set_sensitive(false);
            }
        }
    }
    fn owner_present(&self) -> bool {
        // A destroyed window may still root retained child objects. A root
        // unrealize conservatively retires the preview; hidden panes in a
        // realized window remain valid frozen sources. Reopen after retirement.
        !self.closed.get()
            && self
                .owner
                .upgrade()
                .is_some_and(|owner| owner.widget().root().is_some_and(|root| root.is_realized()))
    }
    fn submit(&self, query: &str, use_regex: bool) {
        self.clear("Searching this frozen snapshot…");
        self.generation.set(0);
        if !self.owner_present() {
            self.worker.stop();
            self.clear("Source pane unavailable. Reopen the preview from an available pane.");
            return;
        }
        match self.worker.submit(query, use_regex) {
            Ok(generation) => self.generation.set(generation),
            Err(error) => self.clear(error.message()),
        }
    }
    fn render(&self) {
        if !self.owner_present() {
            self.clear("Source pane unavailable. Reopen the preview from an available pane.");
            return;
        }
        let matches_borrow = self.matches.borrow();
        let Some(matches) = matches_borrow.as_ref() else {
            return;
        };
        let Some(hit) = matches.hits.get(self.current.get()) else {
            return;
        };
        let doc = &self.snapshot.documents[hit.document];
        let Some(text) = self.text.upgrade() else {
            return;
        };
        if !paint_hit(
            &text,
            &self.tag,
            doc,
            hit,
            self.displayed_document.get() != Some(hit.document),
        ) {
            drop(matches_borrow);
            self.clear("Preview range no longer belongs to this snapshot.");
            return;
        }
        self.displayed_document.set(Some(hit.document));
        if let Some(status) = self.status.upgrade() {
            status.set_text(&format!(
                "Preview: {} of {}",
                self.current.get() + 1,
                matches.hits.len()
            ));
        }
        if let Some(location) = self.location.upgrade() {
            location.set_text(&format!(
                "Record {} · {}{}",
                doc.record_id,
                doc.field.label(),
                if doc.retained_only {
                    " · Partial retained text (may be a tail or prefix)"
                } else {
                    ""
                }
            ));
        }
        for button in [&self.previous, &self.next] {
            if let Some(button) = button.upgrade() {
                button.set_sensitive(true);
            }
        }
    }
    fn navigate(&self, backwards: bool) {
        let next = self.matches.borrow().as_ref().and_then(|matches| {
            search_preview::step(self.current.get(), matches.hits.len(), backwards)
        });
        if let Some(next) = next {
            self.current.set(next);
            self.render();
        }
    }
    fn poll(&self) -> glib::ControlFlow {
        #[cfg(test)]
        self.poll_calls.set(self.poll_calls.get() + 1);
        if self.closed.get() || self.text.upgrade().is_none() {
            self.worker.stop();
            return glib::ControlFlow::Break;
        }
        if !self.owner_present() {
            self.worker.stop();
            self.clear("Source pane unavailable. Reopen the preview from an available pane.");
            return glib::ControlFlow::Break;
        }
        if let Some(reply) = self.worker.take_reply() {
            if search_preview::reply_is_current(
                &reply,
                self.snapshot.id,
                self.generation.get(),
                self.owner_present(),
            ) {
                match reply.result {
                    Ok(matches) if matches.hits.is_empty() => {
                        self.clear("No matches in this snapshot.")
                    }
                    Ok(matches) => {
                        *self.matches.borrow_mut() = Some(matches);
                        self.current.set(0);
                        self.render();
                    }
                    Err(error) => self.clear(error.message()),
                }
            } else {
                #[cfg(test)]
                self.rejected_replies.set(self.rejected_replies.get() + 1);
            }
        }
        glib::ControlFlow::Continue
    }
}

impl UiState {
    /// Capture only the selected pane's retained completed records. The pane
    /// is held weakly after capture: another pane can never replace its owner.
    pub(crate) fn show_exact_search_preview(&self) {
        let Some(owner) = self.search_targets().1 else {
            self.toast_overlay.add_toast(adw::Toast::new(
                "Search preview requires retained completed records.",
            ));
            return;
        };
        let original = self
            .search_entry
            .chars(0, (search_preview::QUERY_LIMIT + 3) as i32);
        if let Err(error) = present_exact_preview(&self.window, owner, original.as_str()) {
            self.toast_overlay
                .add_toast(adw::Toast::new(error.message()));
        }
    }
}

/// The single production dialog path, shared by the UI entry and native tests.
/// The caller chooses the owner once; this never resolves a later active pane.
fn present_exact_preview(
    parent: &impl IsA<gtk4::Widget>,
    owner: Rc<TermView>,
    original: &str,
) -> Result<(adw::Dialog, Rc<PreviewControls>), search_preview::PreviewError> {
    let snapshot = Arc::new(owner.capture_search_preview()?);
    let worker = Worker::new(snapshot.clone())?;
    let dialog = adw::Dialog::builder()
        .title("Retained text search preview")
        .content_width(760)
        .content_height(560)
        .build();
    let body = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    body.set_margin_start(16);
    body.set_margin_end(16);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    let scope = gtk4::Label::new(Some(&format!("Frozen completed-record text only; live terminal and unavailable history are excluded. {} prompts and {} outputs are unavailable. Partial output may retain only a tail or prefix. Close and reopen to refresh.", snapshot.missing_prompts, snapshot.missing_outputs)));
    scope.set_wrap(true);
    scope.set_xalign(0.0);
    body.append(&scope);
    let help = gtk4::Label::new(Some("Case-insensitive, non-overlapping matches within each line and field. Regex uses Rust syntax; empty-match patterns, lookaround and backreferences are unsupported. Controls are shown as escapes and searched as displayed. Visual wrapping does not change matches."));
    help.set_wrap(true);
    help.set_xalign(0.0);
    help.add_css_class("dim-label");
    body.append(&help);
    let entry = gtk4::SearchEntry::new();
    // One sentinel character beyond the byte limit prevents oversized
    // pastes from silently becoming a valid shorter query. Regardless of
    // delegate availability, chars() below bounds every GTK->Rust copy.
    if let Some(delegate) = entry
        .delegate()
        .and_then(|entry| entry.downcast::<gtk4::Text>().ok())
    {
        delegate.set_max_length((search_preview::QUERY_LIMIT + 1) as i32);
    }
    entry.set_hexpand(true);
    entry.set_placeholder_text(Some("Search this frozen snapshot…"));
    let regex = gtk4::CheckButton::with_label("Regex");
    let previous = gtk4::Button::with_label("Previous");
    let next = gtk4::Button::with_label("Next");
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    row.append(&entry);
    row.append(&regex);
    row.append(&previous);
    row.append(&next);
    body.append(&row);
    let status = gtk4::Label::new(None);
    status.set_xalign(0.0);
    status.set_accessible_role(gtk4::AccessibleRole::Status);
    body.append(&status);
    let location = gtk4::Label::new(None);
    location.set_wrap(true);
    location.set_xalign(0.0);
    body.append(&location);
    let text = gtk4::TextView::new();
    text.set_editable(false);
    text.set_cursor_visible(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk4::WrapMode::WordChar);
    let tag = gtk4::TextTag::builder()
        .name("exact-preview-current")
        .background("#e5a50a")
        .foreground("#000000")
        .build();
    text.buffer().tag_table().add(&tag);
    body.append(
        &gtk4::ScrolledWindow::builder()
            .vexpand(true)
            .child(&text)
            .build(),
    );
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    let controls = Rc::new(PreviewControls {
        snapshot,
        worker,
        owner: Rc::downgrade(&owner),
        closed: Cell::new(false),
        generation: Cell::new(0),
        matches: RefCell::new(None),
        current: Cell::new(0),
        displayed_document: Cell::new(None),
        text: text.downgrade(),
        status: status.downgrade(),
        location: location.downgrade(),
        previous: previous.downgrade(),
        next: next.downgrade(),
        tag,
        #[cfg(test)]
        poll_calls: Cell::new(0),
        #[cfg(test)]
        rejected_replies: Cell::new(0),
    });
    // Exact preview starts with its own controls; changing them never calls
    // the native Find controller or changes the original entry.
    // Allow two slash delimiters plus one oversize sentinel. Reading a
    // prefix of at most 8195 scalars allocates at most 32780 UTF-8 bytes;
    // no unbounded text() copy precedes the worker's byte-limit check.
    let (query, use_regex) =
        if original.starts_with('/') && original.ends_with('/') && original.len() > 2 {
            (&original[1..original.len() - 1], true)
        } else {
            (original, false)
        };
    if query.len() <= search_preview::QUERY_LIMIT {
        entry.set_text(query);
    }
    regex.set_active(use_regex);
    let controls_changed = controls.clone();
    let regex_weak = regex.downgrade();
    entry.connect_changed(move |entry| {
        if let Some(regex) = regex_weak.upgrade() {
            controls_changed.submit(
                entry
                    .chars(0, (search_preview::QUERY_LIMIT + 1) as i32)
                    .as_str(),
                regex.is_active(),
            );
        }
    });
    let controls_regex = controls.clone();
    let entry_weak = entry.downgrade();
    regex.connect_toggled(move |regex| {
        if let Some(entry) = entry_weak.upgrade() {
            controls_regex.submit(
                entry
                    .chars(0, (search_preview::QUERY_LIMIT + 1) as i32)
                    .as_str(),
                regex.is_active(),
            );
        }
    });
    let controls_previous = controls.clone();
    previous.connect_clicked(move |_| controls_previous.navigate(true));
    let controls_next = controls.clone();
    next.connect_clicked(move |_| controls_next.navigate(false));
    let controls_closed = controls.clone();
    dialog.connect_closed(move |_| {
        controls_closed.closed.set(true);
        controls_closed.worker.stop();
        controls_closed.clear("");
    });
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let controls_keys = controls.clone();
    let dialog_weak = dialog.downgrade();
    keys.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gtk4::gdk::Key::Escape {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.force_close();
            }
            return true.into();
        }
        if key == gtk4::gdk::Key::Return || key == gtk4::gdk::Key::KP_Enter {
            controls_keys.navigate(modifiers.contains(gtk4::gdk::ModifierType::SHIFT_MASK));
            return true.into();
        }
        false.into()
    });
    dialog.add_controller(keys);
    controls.submit(query, use_regex);
    let controls_poll = controls.clone();
    glib::timeout_add_local(Duration::from_millis(25), move || controls_poll.poll());
    // Set the wrapper as the initial focus target before Adwaita maps it.
    // Grabbing focus immediately after present caches its GtkText delegate;
    // Adwaita's later map tick would grab that delegate directly and select
    // the seed, publishing it to PRIMARY. SearchEntry's own focus override
    // deliberately does not select, so let the initial map use that wrapper.
    dialog.set_focus(Some(&entry));
    let entry_on_map = entry.downgrade();
    dialog.connect_map(move |dialog| {
        let Some(entry) = entry_on_map.upgrade() else {
            return;
        };
        let focus = dialog.property::<Option<gtk4::Widget>>("focus-widget");
        if focus.as_ref().is_some_and(|focus| {
            focus == entry.upcast_ref::<gtk4::Widget>() || focus.is_ancestor(&entry)
        }) {
            // Remapping repeats Adwaita's focus tick. Preserve search-input
            // focus through its non-selecting wrapper, not its cached delegate.
            dialog.set_focus(Some(&entry));
        }
    });
    dialog.present(Some(parent));
    Ok((dialog, controls))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn private_mark_count(buffer: &gtk4::TextBuffer) -> usize {
        (0..=buffer.char_count())
            .map(|offset| {
                buffer
                    .iter_at_offset(offset)
                    .marks()
                    .iter()
                    .filter(|mark| mark.name().as_deref() == Some("exact-preview-scroll"))
                    .count()
            })
            .sum()
    }

    #[test]
    #[ignore = "requires hosted GTK display; never run in the restricted cloud executor"]
    fn exact_preview_tags_follow_unicode_ranges_without_native_selection() {
        gtk4::init().expect("GTK display required");
        let mut snapshot = Snapshot::new().unwrap();
        snapshot
            .push(
                7,
                search_preview::Field::Output,
                "界 hit 😀 hit e\u{301} hit\nwrap hit",
                true,
            )
            .unwrap();
        let matches = search_preview::search(&snapshot, "hit", false, || false).unwrap();
        assert_eq!(matches.hits.len(), 4);
        let text = gtk4::TextView::new();
        text.set_editable(false);
        text.set_wrap_mode(gtk4::WrapMode::WordChar);
        let tag = gtk4::TextTag::builder()
            .name("exact-preview-current")
            .background("#e5a50a")
            .build();
        text.buffer().tag_table().add(&tag);
        let window = gtk4::Window::builder()
            .default_width(180)
            .default_height(120)
            .child(&text)
            .build();
        window.present();
        for width in [180, 600, 120] {
            window.set_default_size(width, 120);
            let until = std::time::Instant::now() + Duration::from_millis(60);
            while std::time::Instant::now() < until {
                for _ in 0..128 {
                    if !glib::MainContext::default().iteration(false) {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(text.width() > 0, "preview text must be allocated");
            let mut ordinal = 0;
            for backwards in [false, true] {
                for _ in 0..matches.hits.len() {
                    let hit = &matches.hits[ordinal];
                    assert!(paint_hit(&text, &tag, &snapshot.documents[0], hit, true));
                    let buffer = text.buffer();
                    assert!(!buffer.has_selection());
                    let mark = buffer
                        .mark("exact-preview-scroll")
                        .expect("one private scroll mark");
                    assert_eq!(buffer.iter_at_mark(&mark).offset(), hit.chars.start);
                    assert_eq!(mark.buffer(), Some(buffer.clone()));
                    assert_eq!(private_mark_count(&buffer), 1);
                    let start = buffer.iter_at_offset(hit.chars.start);
                    let end = buffer.iter_at_offset(hit.chars.end);
                    assert_eq!(buffer.text(&start, &end, true).as_str(), "hit");
                    assert!(start.has_tag(&tag));
                    assert!(!end.has_tag(&tag));
                    for other in &matches.hits {
                        if other.bytes != hit.bytes {
                            assert!(!buffer.iter_at_offset(other.chars.start).has_tag(&tag));
                        }
                    }
                    ordinal = search_preview::step(ordinal, matches.hits.len(), backwards).unwrap();
                }
            }
        }
        // A rejected foreign range never becomes a GTK selection.
        let invalid = search_preview::Hit {
            document: 0,
            bytes: 1..2,
            chars: 0..1,
        };
        assert!(!paint_hit(
            &text,
            &tag,
            &snapshot.documents[0],
            &invalid,
            false
        ));
        assert!(!text.buffer().has_selection());
        // Switching fields repositions the same one-buffer mark. Clearing a
        // query deletes it; a later result must create a fresh private mark.
        let before = text.buffer().mark("exact-preview-scroll").unwrap();
        let mut other = Snapshot::new().unwrap();
        other
            .push(9, search_preview::Field::Command, "other hit\nhit", false)
            .unwrap();
        let other_matches = search_preview::search(&other, "hit", false, || false).unwrap();
        assert!(paint_hit(
            &text,
            &tag,
            &other.documents[0],
            &other_matches.hits[1],
            true
        ));
        let buffer = text.buffer();
        assert_eq!(buffer.mark("exact-preview-scroll"), Some(before.clone()));
        assert_eq!(
            buffer.iter_at_mark(&before).offset(),
            other_matches.hits[1].chars.start
        );
        assert_eq!(private_mark_count(&buffer), 1);
        clear_preview_buffer(&text);
        assert!(buffer.mark("exact-preview-scroll").is_none());
        assert!(before.buffer().is_none());
        assert_eq!(buffer.char_count(), 0);
        assert_eq!(private_mark_count(&buffer), 0);
        assert!(paint_hit(
            &text,
            &tag,
            &other.documents[0],
            &other_matches.hits[0],
            true
        ));
        let after = buffer.mark("exact-preview-scroll").unwrap();
        assert_ne!(before, after);
        assert_eq!(after.buffer(), Some(buffer.clone()));
        assert_eq!(
            buffer.iter_at_mark(&after).offset(),
            other_matches.hits[0].chars.start
        );
        assert_eq!(private_mark_count(&buffer), 1);
        assert!(!buffer.has_selection());
        window.close();
    }
}

#[cfg(test)]
#[path = "search_preview_dialog_tests.rs"]
mod dialog_tests;
