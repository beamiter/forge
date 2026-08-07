//! Window-global bottom status bar.
//!
//! What the bar says — segment order, text, tones — is owned by
//! [`jterm_core::bottom_bar`], so all four jterms read alike. This file only
//! gathers the [`Snapshot`] from state the app already tracks (pane cwd,
//! cached git meta, block records, the notebook) and renders the composed
//! segments as one GTK label each. Layout and tone colors live in the app's
//! chrome CSS (`.bottom-bar`, `bb-*` classes).

use gtk4::glib;
use gtk4::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;
use vte4::TerminalExt as _;

use jterm_core::bottom_bar::{compose, Segment, Snapshot, Tone};

use super::UiState;
use crate::block_view::TermView;

/// GObject data key on a Block view's root: `(exit_code, duration_ms)` of the
/// last finished command. VTE panes keep no block records, so they carry no
/// entry and the bar's last-command segment stays absent.
const LAST_BLOCK_STATUS_KEY: &str = "bottom-bar-last-status";

/// Build the bar shell: `(bar, left group, right group)`. The caller appends
/// the bar below the content box so it spans sidebar and terminals alike.
pub(crate) fn build_bottom_bar() -> (gtk4::Box, gtk4::Box, gtk4::Box) {
    let bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    bar.add_css_class("bottom-bar");
    let left = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    left.set_hexpand(true);
    left.set_halign(gtk4::Align::Start);
    let right = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    right.set_halign(gtk4::Align::End);
    bar.append(&left);
    bar.append(&right);
    (bar, left, right)
}

fn tone_css_class(tone: Tone) -> &'static str {
    match tone {
        Tone::Normal => "bb-normal",
        Tone::Muted => "bb-muted",
        Tone::Positive => "bb-ok",
        Tone::Negative => "bb-err",
    }
}

fn set_segments(container: &gtk4::Box, segments: &[Segment]) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    for segment in segments {
        let label = gtk4::Label::new(Some(&segment.text));
        label.add_css_class(tone_css_class(segment.tone));
        container.append(&label);
    }
}

/// VTE reports grid dimensions as C longs; the snapshot wants u16 with 0
/// meaning unknown.
fn grid_dimension(count: i64) -> u16 {
    count.clamp(0, i64::from(u16::MAX)) as u16
}

impl UiState {
    /// Single owner of the bar's `visible()`, reading config — the
    /// bottom-bar analog of `sync_tab_bar_visibility`.
    pub(crate) fn sync_bottom_bar_visibility(&self) {
        let visible = self.config.borrow().bottom_bar;
        self.bottom_bar.set_visible(visible);
        self.refresh_bottom_bar();
    }

    /// Record a finished command's outcome on its pane and repaint. The data
    /// lives on the view's root widget so it moves with the pane across
    /// splits and tab detaches and dies with it.
    pub(crate) fn connect_bottom_bar_block_status(&self, view: &Rc<TermView>) {
        let ui = self.clone();
        let root = view.widget();
        view.connect_block_finished(
            move |_command, exit_code, _output, _agent_generation, duration_ms| {
                unsafe {
                    root.set_data::<(Option<i32>, Option<u64>)>(
                        LAST_BLOCK_STATUS_KEY,
                        (exit_code, duration_ms),
                    );
                }
                ui.refresh_bottom_bar();
            },
        );
    }

    /// Re-collect the focused pane's snapshot and repaint the bar. Cheap when
    /// nothing changed: identical content skips the label rebuild entirely.
    pub(crate) fn refresh_bottom_bar(&self) {
        if !self.bottom_bar.get_visible() {
            return;
        }
        let leaf = self.current_pane_leaf();
        let cwd = leaf
            .as_ref()
            .and_then(|leaf| self.pane_working_directory(leaf))
            .map(PathBuf::from);
        // The worker-cached probe returns stale-or-fresh within a bounded UI
        // wait; the local meta converts field-for-field into the family type.
        let git = cwd.as_deref().and_then(crate::git_meta::read).map(|meta| {
            jterm_core::git_meta::RepoMeta {
                branch: meta.branch,
                dirty: meta.dirty,
                ahead: meta.ahead,
                behind: meta.behind,
            }
        });
        let (last_exit, last_duration_ms) = leaf
            .as_ref()
            .and_then(|leaf| leaf.block_view())
            .and_then(|view| unsafe {
                view.widget()
                    .data::<(Option<i32>, Option<u64>)>(LAST_BLOCK_STATUS_KEY)
                    .map(|status| *status.as_ref())
            })
            .unwrap_or((None, None));
        let running = leaf.as_ref().is_some_and(|leaf| {
            leaf.foreground_process_name()
                .is_some_and(|name| !name.is_empty())
        });
        let (cols, rows) = leaf
            .as_ref()
            .map(|leaf| {
                let terminal = leaf.terminal();
                (
                    grid_dimension(terminal.column_count()),
                    grid_dimension(terminal.row_count()),
                )
            })
            .unwrap_or((0, 0));
        let home = glib::home_dir();

        let snapshot = Snapshot {
            cwd: cwd.as_deref(),
            home: (!home.as_os_str().is_empty()).then_some(home.as_path()),
            git: git.as_ref(),
            running,
            last_exit,
            last_duration_ms,
            cols,
            rows,
            tab_index: self.notebook.current_page().unwrap_or(0) as usize,
            tab_count: self.notebook.n_pages() as usize,
        };
        let content = compose(&snapshot);
        if *self.bottom_bar_content.borrow() == content {
            return;
        }
        set_segments(&self.bottom_bar_left, &content.left);
        set_segments(&self.bottom_bar_right, &content.right);
        *self.bottom_bar_content.borrow_mut() = content;
    }
}
