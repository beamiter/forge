//! Read-only access to history written before records had pane ownership.

use super::{
    absolute_history_path, legacy_zone_path, read_history_snapshot_reserved, LoadedHistory,
};
use crate::block_view::{
    known_output_notice, zone_history, FinishedBlock, WidgetPool, MAX_FINISHED_VTE_COLUMNS,
};
use crate::config::Config;
use crate::persistence::{self, PersistenceKey};
use gtk4::prelude::*;
use gtk4::{glib, Box as GtkBox, Label, Orientation, ScrolledWindow, Window};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Block,
    Unified,
}

impl Kind {
    fn path(self, config: &Config) -> Option<PathBuf> {
        let base = absolute_history_path(config.block_history_path.as_deref()?).ok()?;
        let path = match self {
            Self::Block => base,
            Self::Unified => legacy_zone_path(&base),
        };
        path.symlink_metadata().ok()?;
        Some(path)
    }

    fn title(self) -> &'static str {
        match self {
            Self::Block => "Shared Block History",
            Self::Unified => "Shared Unified History",
        }
    }
}

enum Archive {
    Blocks(Arc<LoadedHistory>),
    Zones(String, bool),
}

pub(crate) fn append_menu_items(
    menu: &GtkBox,
    popover: &gtk4::Popover,
    parent: &impl IsA<Window>,
    config: &Config,
) {
    for kind in [Kind::Block, Kind::Unified] {
        let Some(path) = kind.path(config) else {
            continue;
        };
        let item = gtk4::Button::with_label(kind.title());
        item.add_css_class("flat");
        let popover = popover.downgrade();
        let parent = parent.upcast_ref::<Window>().downgrade();
        let config = config.clone();
        item.connect_clicked(move |_| {
            if let (Some(popover), Some(parent)) = (popover.upgrade(), parent.upgrade()) {
                popover.popdown();
                open(&parent, kind, path.clone(), config.clone());
            }
        });
        menu.append(&item);
    }
}

pub(crate) fn open_configured(parent: &impl IsA<Window>, kind: Kind, config: &Config) -> bool {
    let Some(path) = kind.path(config) else {
        return false;
    };
    open(parent.upcast_ref(), kind, path, config.clone());
    true
}

fn open(parent: &Window, kind: Kind, path: PathBuf, config: Config) {
    let window = Window::builder()
        .title(kind.title())
        .default_width(760)
        .default_height(520)
        .modal(true)
        .destroy_with_parent(true)
        .transient_for(parent)
        .build();
    let content = GtkBox::new(Orientation::Vertical, 8);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    let description = Label::new(Some("Archived records with unknown pane ownership"));
    description.set_xalign(0.0);
    content.append(&description);
    let status = Label::new(Some("Loading history…"));
    status.set_xalign(0.0);
    status.set_wrap(true);
    content.append(&status);
    let close = gtk4::Button::with_label("Close archive");
    close.set_halign(gtk4::Align::End);
    let target = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = target.upgrade() {
            window.close();
        }
    });
    let archive = GtkBox::new(Orientation::Vertical, 8);
    archive.append(&content);
    archive.append(&close);
    close.set_margin_end(12);
    close.set_margin_bottom(12);
    content.set_vexpand(true);
    window.set_child(Some(&archive));

    let keys = gtk4::EventControllerKey::new();
    let target = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key != gtk4::gdk::Key::Escape {
            return glib::Propagation::Proceed;
        }
        if let Some(window) = target.upgrade() {
            window.close();
        }
        glib::Propagation::Stop
    });
    window.add_controller(keys);

    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let key = PersistenceKey::unique_for_path("legacy-history-read", &path);
    let compressed = config.block_history_compress;
    let read_limit = (config.lazy_load_threshold as usize).min(config.max_visible_blocks as usize);
    if let Err(error) = persistence::enqueue(key, "Read shared history archive", move || {
        let result = match kind {
            Kind::Block => {
                read_history_snapshot_reserved(&path, compressed, read_limit).map(Archive::Blocks)
            }
            Kind::Unified => zone_history::legacy_archive_text(&path)
                .map(|(text, limited)| Archive::Zones(text, limited)),
        };
        let _ = send.send(result);
        Ok(())
    }) {
        status.set_label(&format!("Unable to open the archive. {error}"));
    } else {
        let content = content.downgrade();
        glib::timeout_add_local(Duration::from_millis(20), move || {
            let Some(content) = content.upgrade() else {
                return glib::ControlFlow::Break;
            };
            match receive.try_recv() {
                Ok(Ok(Archive::Blocks(records))) => {
                    show_blocks(&content, &status, records, config.clone())
                }
                Ok(Ok(Archive::Zones(text, limited))) => {
                    if text.is_empty() {
                        status.set_label("No archived records.");
                    } else {
                        set_limit_notice(&status, limited);
                        let view = gtk4::TextView::builder()
                            .editable(false)
                            .monospace(true)
                            .build();
                        view.buffer().set_text(&text);
                        content.append(&archive_scroll(&view));
                    }
                }
                Ok(Err(error)) => status.set_label(&format!("Unable to read the archive. {error}")),
                Err(std::sync::mpsc::TryRecvError::Empty) => return glib::ControlFlow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    status.set_label("Unable to read the archive.");
                }
            }
            glib::ControlFlow::Break
        });
    }
    window.present();
}

fn set_limit_notice(status: &Label, limited: bool) {
    status.set_label("Showing the most recent readable records within the archive limits.");
    status.set_visible(limited);
}

fn archive_scroll(child: &impl IsA<gtk4::Widget>) -> ScrolledWindow {
    ScrolledWindow::builder().vexpand(true).child(child).build()
}

fn clear_card(content: &GtkBox) {
    if let Some(widget) = content.first_child().and_downcast::<GtkBox>() {
        content.remove(&widget);
        WidgetPool::teardown(&widget);
    }
}

fn show_blocks(content: &GtkBox, status: &Label, records: Arc<LoadedHistory>, config: Config) {
    let limited = records.target_revision.is_none() || records.blocks.len() < records.total_loaded;
    if records.blocks.is_empty() {
        status.set_label(if limited {
            "No readable records within the archive limits."
        } else {
            "No archived records."
        });
        return;
    }
    set_limit_notice(status, limited);
    let names: Vec<_> = records
        .blocks
        .iter()
        .map(|block| {
            if block.is_background() {
                "Background output".to_owned()
            } else {
                jterm_core::review_input::safe_inline_display(&block.cmd, 120)
            }
        })
        .collect();
    let labels: Vec<_> = names.iter().map(String::as_str).collect();
    let picker = gtk4::DropDown::from_strings(&labels);
    picker.update_property(&[gtk4::accessible::Property::Label("Archived command")]);
    picker.set_selected((labels.len() - 1) as u32);
    content.append(&picker);

    let card = GtkBox::new(Orientation::Vertical, 0);
    card.connect_unrealize(clear_card);
    content.append(&archive_scroll(&card));
    let show = move |index: u32| {
        clear_card(&card);
        let Some(record) = records.blocks.get(index as usize) else {
            return;
        };
        let cols = if record.cols == 0 {
            80
        } else {
            i64::from(record.cols).min(MAX_FINISHED_VTE_COLUMNS)
        };
        let finished = FinishedBlock::from_retained(record, &config, cols);
        finished.set_lifecycle(
            record.lifecycle_health(),
            record.lifecycle_notice().as_deref(),
        );
        finished.set_output_notice(
            record
                .output_notice
                .as_deref()
                .and_then(known_output_notice),
        );
        finished.header_row.set_tooltip_text(None);
        finished.action_box.set_visible(false);
        card.append(finished.widget());
    };
    show(picker.selected());
    picker.connect_selected_notify(move |picker| show(picker.selected()));
}
