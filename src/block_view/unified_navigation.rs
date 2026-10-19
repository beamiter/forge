//! Explicit navigation for the persistent terminal. Adjustment coordinates
//! come only from VTE and authenticated zone proofs, never metadata row guesses.
use super::UnifiedZoneStore;
use gtk4::{glib, prelude::*};
use std::{cell::Cell, rc::Rc};
use vte4::Terminal;

const NAVIGATION_EPSILON: f64 = 0.01;
const FOLLOW_SLACK_ROWS: f64 = 1.0;
const MAX_COPY_COMMAND_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandStep {
    Record(u64),
    Live,
    Empty,
    Oldest,
    Retired,
    Unavailable,
}

/// The cursor is an explicit record choice, not an inference from a scroll
/// value: several command heads can clamp to the same last-screen position.
pub(super) fn command_step(
    ids: &[u64],
    current: Option<u64>,
    reading: bool,
    forward: bool,
    value: f64,
    mut proven_value: impl FnMut(u64) -> Option<f64>,
) -> CommandStep {
    if let Some(current) = current {
        let Some(index) = ids.iter().position(|id| *id == current) else {
            return CommandStep::Retired;
        };
        return if forward {
            ids.get(index + 1)
                .copied()
                .map_or(CommandStep::Live, CommandStep::Record)
        } else {
            index
                .checked_sub(1)
                .map_or(CommandStep::Oldest, |index| CommandStep::Record(ids[index]))
        };
    }
    if !reading {
        return if forward {
            CommandStep::Live
        } else {
            ids.last()
                .copied()
                .map_or(CommandStep::Empty, CommandStep::Record)
        };
    }
    if ids.is_empty() {
        return CommandStep::Empty;
    }
    if !value.is_finite() {
        return CommandStep::Unavailable;
    }
    let mut candidate: Option<(u64, f64)> = None;
    let mut latest_proven_value = None;
    for &id in ids {
        let Some(target) = proven_value(id).filter(|target| target.is_finite()) else {
            continue;
        };
        if Some(&id) == ids.last() {
            latest_proven_value = Some(target);
        }
        let on_side = if forward {
            target > value + NAVIGATION_EPSILON
        } else {
            target < value - NAVIGATION_EPSILON
        };
        if !on_side {
            continue;
        }
        let nearer = candidate.is_none_or(|(_, previous)| {
            if forward {
                // Keep the oldest record in a clamped same-value group.
                target < previous
            } else {
                // Previous chooses the newest record in an equal-value group.
                target >= previous
            }
        });
        if nearer {
            candidate = Some((id, target));
        }
    }
    match candidate {
        Some((id, _)) => CommandStep::Record(id),
        None if forward
            && latest_proven_value.is_some_and(|target| target <= value + NAVIGATION_EPSILON) =>
        {
            CommandStep::Live
        }
        None => CommandStep::Unavailable,
    }
}

pub(super) fn is_reading(lower: f64, upper: f64, page: f64, value: f64) -> Option<bool> {
    if ![lower, upper, page, value].iter().all(|n| n.is_finite()) || upper < lower || page <= 0.0 {
        return None;
    }
    Some(value < (upper - page).max(lower) - FOLLOW_SLACK_ROWS)
}

fn sync_reading(
    adjustment: &gtk4::Adjustment,
    reading: &Cell<bool>,
    unread: &Cell<u32>,
) -> Option<bool> {
    let away = is_reading(
        adjustment.lower(),
        adjustment.upper(),
        adjustment.page_size(),
        adjustment.value(),
    )?;
    reading.set(away);
    if !away {
        unread.set(0);
    }
    Some(away)
}

pub(super) fn copyable_command(command: &str) -> bool {
    !command.trim().is_empty()
        && command.len() <= MAX_COPY_COMMAND_BYTES
        && jterm_core::review_input::safe_multiline_display(command, MAX_COPY_COMMAND_BYTES)
            == command
}

/// Rc data contains no strong VTE, backend or TermView reference. Native
/// signals schedule one idle refresh through a Weak navigation strip.
pub(super) struct NavigationBindings {
    pub(super) terminal: Terminal,
    pub(super) fullscreen: Rc<Cell<bool>>,
    pub(super) reading: Rc<Cell<bool>>,
    pub(super) unread: Rc<Cell<u32>>,
    pub(super) cursor: Rc<Cell<Option<u64>>>,
    pub(super) programmatic: Rc<Cell<bool>>,
    pub(super) records: Rc<std::cell::RefCell<UnifiedZoneStore>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MetadataKey {
    len: usize,
    first_id: Option<u64>,
    last_id: Option<u64>,
    current_id: Option<u64>,
}

pub(super) struct NavigationBar {
    pub(super) root: gtk4::Box,
    pub(super) previous: gtk4::Button,
    pub(super) next: gtk4::Button,
    pub(super) copy: gtk4::Button,
    pub(super) find: gtk4::Button,
    pub(super) browse: gtk4::Button,
    pub(super) live: gtk4::Button,
    pub(super) status: gtk4::Label,
    refresh_pending: Cell<bool>,
    suspended: Cell<bool>,
    refresh: std::cell::RefCell<Option<Rc<dyn Fn()>>>,
    metadata_key: Cell<Option<MetadataKey>>,
    command_count: Cell<usize>,
    current_description: std::cell::RefCell<Option<String>>,
}

fn control(icon: &str, label: &str) -> gtk4::Button {
    let button = gtk4::Button::from_icon_name(icon);
    button.add_css_class("flat");
    button.set_focus_on_click(false);
    button.set_focusable(true);
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk4::accessible::Property::Label(label)]);
    button
}

impl NavigationBar {
    pub(super) fn new() -> Rc<Self> {
        let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
        root.add_css_class("unified-navigation");
        root.set_hexpand(true);
        root.set_focusable(false);
        root.update_property(&[gtk4::accessible::Property::Label(
            "Unified command navigation",
        )]);
        let previous = control("go-up-symbolic", "Previous command");
        let next = control(
            "go-down-symbolic",
            "Next command; return to live after the newest command",
        );
        let copy = control("edit-copy-symbolic", "Copy current command");
        let find = control("edit-find-symbolic", "Find in this terminal");
        let browse = control(
            "view-list-symbolic",
            "Browse completed commands in this pane",
        );
        let live = control(
            "go-bottom-symbolic",
            "Return to live output and focus the terminal",
        );
        let status = gtk4::Label::new(Some("Live"));
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        status.set_width_chars(1);
        status.set_margin_start(4);
        status.set_margin_end(4);
        previous.set_sensitive(false);
        next.set_sensitive(false);
        copy.set_sensitive(false);
        live.set_sensitive(false);
        root.append(&previous);
        root.append(&next);
        root.append(&status);
        for button in [&copy, &find, &browse, &live] {
            root.append(button);
        }
        Rc::new(Self {
            root,
            previous,
            next,
            copy,
            find,
            browse,
            live,
            status,
            refresh_pending: Cell::new(false),
            suspended: Cell::new(false),
            refresh: std::cell::RefCell::new(None),
            metadata_key: Cell::new(None),
            command_count: Cell::new(0),
            current_description: std::cell::RefCell::new(None),
        })
    }

    pub(super) fn schedule_refresh(self: &Rc<Self>) {
        if self.refresh_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(bar) = weak.upgrade() else { return };
            bar.refresh_pending.set(false);
            let refresh = bar.refresh.borrow().clone();
            if let Some(refresh) = refresh {
                refresh();
            }
        });
    }

    pub(super) fn is_suspended(&self) -> bool {
        self.suspended.get()
    }

    pub(super) fn set_suspended(self: &Rc<Self>, suspended: bool) {
        self.suspended.set(suspended);
        self.root.set_visible(!suspended);
        self.schedule_refresh();
    }

    pub(super) fn observe(self: &Rc<Self>, bindings: NavigationBindings) {
        let NavigationBindings {
            terminal,
            fullscreen,
            reading,
            unread,
            cursor,
            programmatic,
            records,
        } = bindings;
        let weak = Rc::downgrade(self);
        let weak_terminal = terminal.downgrade();
        let fullscreen_for_refresh = fullscreen.clone();
        let reading_for_refresh = reading.clone();
        let unread_for_refresh = unread.clone();
        let cursor_for_refresh = cursor.clone();
        *self.refresh.borrow_mut() = Some(Rc::new(move || {
            let (Some(bar), Some(terminal)) = (weak.upgrade(), weak_terminal.upgrade()) else {
                return;
            };
            let fullscreen = fullscreen_for_refresh.get() || bar.suspended.get();
            bar.root.set_visible(!fullscreen);
            if fullscreen {
                return;
            }
            let ready = terminal
                .vadjustment()
                .and_then(|adjustment| {
                    sync_reading(&adjustment, &reading_for_refresh, &unread_for_refresh)
                })
                .is_some();
            let records = records.borrow();
            let current = cursor_for_refresh.get();
            let key = MetadataKey {
                len: records.records.len(),
                first_id: records.records.front().map(|record| record.id),
                last_id: records.records.back().map(|record| record.id),
                current_id: current,
            };
            // Output and adjustment ticks only update scalar state. Inspect
            // the bounded metadata table when its identity or cursor changes.
            if bar.metadata_key.replace(Some(key)) != Some(key) {
                let count = records
                    .records
                    .iter()
                    .filter(|record| !record.is_background)
                    .count();
                bar.command_count.set(count);
                let current_record =
                    current.and_then(|id| records.records.iter().find(|record| record.id == id));
                let description = current_record
                    .map(|record| jterm_core::review_input::safe_inline_display(&record.cmd, 180));
                bar.copy.set_sensitive(current_record.is_some_and(|record| {
                    !record.is_background
                        && record.command_source != super::CommandTextSource::ScreenAfterTruncation
                        && copyable_command(&record.cmd)
                }));
                bar.copy
                    .set_tooltip_text(Some(&description.as_ref().map_or_else(
                        || "Navigate to a retained command before copying it".to_string(),
                        |command| format!("Copy current command: {command}"),
                    )));
                *bar.current_description.borrow_mut() = description;
            }
            let reading = reading_for_refresh.get();
            let unread = unread_for_refresh.get();
            let count = bar.command_count.get();
            let mut label = if !ready {
                "Preparing terminal".to_string()
            } else if reading {
                "Reading history".to_string()
            } else {
                "Live".to_string()
            };
            if unread > 0 {
                label.push_str(&format!(" · {unread} unread"));
            }
            if let Some(command) = bar.current_description.borrow().as_deref() {
                label.push_str(" · ");
                label.push_str(command);
            } else if current.is_some() {
                label.push_str(" · command no longer retained");
            } else {
                label.push_str(&format!(
                    " · {count} command{}",
                    if count == 1 { "" } else { "s" }
                ));
            }
            if bar.status.text().as_str() != label {
                bar.status.set_text(&label);
                bar.status.set_tooltip_text(Some(&label));
                bar.status
                    .update_property(&[gtk4::accessible::Property::Label(&label)]);
            }
            bar.previous
                .set_sensitive(ready && (count > 0 || current.is_some()));
            bar.next
                .set_sensitive(ready && (reading || current.is_some()));
            bar.live
                .set_sensitive(ready && (reading || current.is_some() || unread > 0));
        }));
        if let Some(adjustment) = terminal.vadjustment() {
            let fullscreen_for_range = fullscreen.clone();
            let reading_for_range = reading.clone();
            let unread_for_range = unread.clone();
            let last_value = Cell::new(adjustment.value());
            let weak = Rc::downgrade(self);
            adjustment.connect_value_changed(move |adjustment| {
                let Some(bar) = weak.upgrade() else { return };
                let value = adjustment.value();
                let previous = last_value.replace(value);
                if !fullscreen.get() && !bar.suspended.get() {
                    if value.is_finite()
                        && (value - previous).abs() > NAVIGATION_EPSILON
                        && !programmatic.get()
                    {
                        cursor.set(None);
                    }
                    sync_reading(adjustment, &reading, &unread);
                }
                bar.schedule_refresh();
            });
            let weak = Rc::downgrade(self);
            adjustment.connect_changed(move |adjustment| {
                if let Some(bar) = weak.upgrade() {
                    // Range can grow without moving the native viewport. A
                    // completion in this same event must see the new state;
                    // only label/metadata work waits for the coalesced idle.
                    if !fullscreen_for_range.get() && !bar.suspended.get() {
                        sync_reading(adjustment, &reading_for_range, &unread_for_range);
                    }
                    bar.schedule_refresh();
                }
            });
        }
        let weak = Rc::downgrade(self);
        terminal.connect_map(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.schedule_refresh();
            }
        });
        self.schedule_refresh();
    }
}

impl super::TermView {
    pub(super) fn release_unified_navigation_cursor(&self) {
        if let Some(bar) = &self.unified_navigation {
            self.navigated_record_id.set(None);
            bar.schedule_refresh();
        }
    }

    pub(crate) fn unified_navigation_available(&self) -> bool {
        self.unified_navigation.is_some()
            && !self.fullscreen.get()
            && self.bstate.get() != super::BlockState::AltScreen
    }

    /// The caller binds each operation to this pane's weak identity. Buttons
    /// never dispatch an application action by guessing from keyboard focus.
    pub(crate) fn connect_unified_navigation(
        &self,
        callback: impl Fn(crate::keybindings::Action) + 'static,
    ) {
        use crate::keybindings::Action;
        let Some(bar) = &self.unified_navigation else {
            return;
        };
        let callback = Rc::new(callback);
        for (button, action) in [
            (&bar.previous, Action::PreviousCommand),
            (&bar.next, Action::NextCommand),
            (&bar.copy, Action::CopyCurrentCommand),
            (&bar.find, Action::ToggleSearch),
            (&bar.browse, Action::CrossBlockSearch),
            (&bar.live, Action::ReturnToLive),
        ] {
            let callback = callback.clone();
            button.connect_clicked(move |_| callback(action));
        }
    }

    pub(crate) fn navigate_command(
        &self,
        forward: bool,
    ) -> Result<super::RecordNavigationResult, &'static str> {
        let Some(bar) = &self.unified_navigation else {
            return Err("Command navigation is available in Unified mode.");
        };
        if self.fullscreen.get() || self.bstate.get() == super::BlockState::AltScreen {
            return Err("Exit the full-screen app before navigating command history.");
        }
        let ids: Vec<_> = self
            .render_backend
            .records()
            .iter()
            .filter(|record| !record.is_background())
            .map(|record| record.id())
            .collect();
        let value = self
            .active_vte
            .vadjustment()
            .map_or(f64::NAN, |adjustment| adjustment.value());
        let plan = command_step(
            &ids,
            self.navigated_record_id.get(),
            self.user_scrolled_up.get(),
            forward,
            value,
            |id| self.render_backend.record_scroll_value(id),
        );
        let result = match plan {
            CommandStep::Record(id) => Ok(self.navigate_to_record_id(id, false)),
            CommandStep::Live => {
                self.return_to_live();
                Ok(super::RecordNavigationResult::Navigated)
            }
            CommandStep::Empty => Err("No completed commands yet. Browse becomes available as commands finish."),
            CommandStep::Oldest => Err("This is the oldest retained command."),
            CommandStep::Retired => {
                self.navigated_record_id.set(None);
                Err("The current command is no longer retained. Browse the remaining commands.")
            }
            CommandStep::Unavailable => Err("No exact command location in that direction. Use Browse to inspect retained commands."),
        };
        bar.schedule_refresh();
        result
    }

    /// A presentation-only jump: no input bytes, selection edits, command
    /// replay, or shell redraw. VTE resumes its own native bottom following.
    pub(crate) fn return_to_live(&self) -> bool {
        if !self.reveal_unified_live_input() {
            return false;
        }
        if self.root.is_mapped() {
            super::focus_terminal(&self.active_vte);
        }
        true
    }

    /// Tab activation reveals the native input without taking focus away
    /// from a global Find entry. Only an explicit Return to live focuses it.
    pub(super) fn reveal_unified_live_input(&self) -> bool {
        if self.fullscreen.get() || self.bstate.get() == super::BlockState::AltScreen {
            return false;
        }
        self.navigated_record_id.set(None);
        self.user_scrolled_up.set(false);
        self.unread_count.set(0);
        self.jump_fab.set_visible(false);
        let previous = self.programmatic_scroll.replace(true);
        if let Some(adjustment) = self.active_vte.vadjustment() {
            adjustment
                .set_value((adjustment.upper() - adjustment.page_size()).max(adjustment.lower()));
        }
        self.programmatic_scroll.set(previous);
        if let Some(bar) = &self.unified_navigation {
            bar.schedule_refresh();
        } else {
            super::reveal_live_input_now(
                &self.scroll_debouncer,
                &self.unread_count,
                &self.jump_fab,
                &self.block_list,
                &self.block_scroll,
            );
        }
        true
    }

    pub(crate) fn copy_current_command(&self) -> Result<(), &'static str> {
        if self.unified_navigation.is_none()
            || self.fullscreen.get()
            || self.bstate.get() == super::BlockState::AltScreen
        {
            return Err("Navigate to a command in Unified mode before copying it.");
        }
        let id = self
            .navigated_record_id
            .get()
            .ok_or("Navigate to a command before copying it.")?;
        let command = {
            let records = self.render_backend.records();
            let record = records
                .iter()
                .find(|record| record.id() == id)
                .ok_or("This command is no longer retained; nothing was copied.")?;
            let command = record.command();
            if matches!(record, super::BackendRecordRef::Metadata { record, .. }
                if record.command_source == super::CommandTextSource::ScreenAfterTruncation)
            {
                return Err("Only a truncated command was retained; nothing was copied.");
            }
            if record.is_background() || !copyable_command(command) {
                return Err("This command is empty, too large, or contains hidden controls; nothing was copied.");
            }
            command.to_owned()
        };
        self.active_vte.clipboard().set_text(&command);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_is_ordered_and_preserves_same_value_record_identity() {
        let ids = [2, 7, 11];
        assert_eq!(
            command_step(&ids, None, false, false, 90.0, |_| None),
            CommandStep::Record(11)
        );
        assert_eq!(
            command_step(&ids, Some(2), false, true, 90.0, |_| Some(90.0)),
            CommandStep::Record(7)
        );
        assert_eq!(
            command_step(&ids, Some(7), false, true, 90.0, |_| Some(90.0)),
            CommandStep::Record(11)
        );
        assert_eq!(
            command_step(&ids, Some(11), false, true, 90.0, |_| None),
            CommandStep::Live
        );
        assert_eq!(
            command_step(&ids, Some(2), true, false, 0.0, |_| None),
            CommandStep::Oldest
        );
        assert_eq!(
            command_step(&ids, Some(3), true, false, 0.0, |_| None),
            CommandStep::Retired
        );
    }

    #[test]
    fn reading_navigation_uses_only_proven_directional_heads_and_stable_ties() {
        let ids = [1, 2, 3, 4, 5];
        let proof = |id| match id {
            1 => Some(10.0),
            2 | 3 => Some(40.0),
            4 | 5 => Some(90.0),
            _ => None,
        };
        assert_eq!(
            command_step(&ids, None, true, true, 70.0, proof),
            CommandStep::Record(4)
        );
        assert_eq!(
            command_step(&ids, None, true, false, 70.0, proof),
            CommandStep::Record(3)
        );
        assert_eq!(
            command_step(&ids, None, true, false, 40.0, proof),
            CommandStep::Record(1)
        );
        assert_eq!(
            command_step(&ids, None, true, true, 40.0, proof),
            CommandStep::Record(4)
        );
        assert_eq!(
            command_step(&ids, None, true, true, 70.0, |_| None),
            CommandStep::Unavailable
        );
        assert_eq!(
            command_step(&ids, None, true, false, f64::NAN, proof),
            CommandStep::Unavailable
        );
        assert_eq!(
            command_step(&ids, None, true, true, 95.0, proof),
            CommandStep::Live,
            "the latest command's proven head is already before the viewport"
        );
        assert_eq!(
            command_step(&ids, None, true, true, 95.0, |id| {
                if id == 5 {
                    None
                } else {
                    proof(id)
                }
            }),
            CommandStep::Unavailable,
            "an older proof cannot prove that the viewport passed the latest command"
        );
    }

    #[test]
    fn reading_observes_range_changes_and_rejects_invalid_geometry() {
        assert_eq!(is_reading(0.0, 100.0, 20.0, 80.0), Some(false));
        assert_eq!(is_reading(0.0, 140.0, 20.0, 80.0), Some(true));
        assert_eq!(is_reading(0.0, 100.0, 20.0, 79.6), Some(false));
        assert_eq!(is_reading(10.0, 5.0, 2.0, 10.0), None);
        assert_eq!(is_reading(0.0, 100.0, 0.0, 20.0), None);
        assert_eq!(is_reading(0.0, f64::INFINITY, 20.0, 0.0), None);
    }

    #[test]
    fn command_copy_keeps_exact_multiline_text_and_rejects_hidden_controls() {
        assert!(copyable_command("printf one\nprintf 二"));
        assert!(!copyable_command(" "));
        assert!(!copyable_command("echo\u{202e}hidden"));
        assert!(!copyable_command("echo\x1b[31mred"));
        assert!(!copyable_command(&"x".repeat(MAX_COPY_COMMAND_BYTES + 1)));
    }
}
