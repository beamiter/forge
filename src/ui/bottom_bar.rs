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

use jterm_core::bottom_bar::{compose, Segment, SegmentKind, Snapshot, Tone};

use super::{PaneLeaf, UiState};
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
        if matches!(segment.kind, SegmentKind::Cwd | SegmentKind::Git) {
            label.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
            label.set_tooltip_text(Some(&segment.text));
        }
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
        // The pane that will run the commands this handler hears about, held
        // weakly: the handler is owned by that very view, so a strong handle
        // would keep its PTY and scrollback alive for the life of the process.
        let source = Rc::downgrade(view);
        view.connect_block_finished(move |_command, exit_code, _agent_generation, duration_ms| {
            unsafe {
                root.set_data::<(Option<i32>, Option<u64>)>(
                    LAST_BLOCK_STATUS_KEY,
                    (exit_code, duration_ms),
                );
            }
            // A finished command is the moment Git's answer can have moved, so
            // it is what drives the refresh — not the once-a-second repaint,
            // which now serves the cached answer for as long as it is fresh.
            //
            // The directory that moved is this pane's, which is emphatically
            // not `current_pane_leaf()`: one handler is installed per pane, and
            // a command finishing in a background tab or a sibling split fires
            // here while the focused pane sits in some unrelated repository.
            // Marking the focused pane instead would leave the pane that
            // actually changed serving its old branch and dirty flag for the
            // full `CACHE_TTL`, with no repaint left to correct it — the
            // once-a-second probe that used to paper over this is exactly what
            // the cache replaced.
            if let Some(cwd) = source
                .upgrade()
                .map(PaneLeaf::Block)
                .and_then(|leaf| ui.pane_working_directory(&leaf))
            {
                crate::git_meta::invalidate(std::path::Path::new(&cwd));
            }
            ui.refresh_bottom_bar();
        });
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
        // Never wait for Git on GTK's frame thread. The worker refreshes its
        // cache and the next status tick observes the completed value.
        let git = cwd
            .as_deref()
            .and_then(crate::git_meta::read_cached_and_refresh);
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

#[cfg(test)]
mod tests {
    use super::*;
    use libadwaita as adw;

    #[test]
    #[ignore = "requires an isolated GTK display"]
    fn long_status_segments_keep_compact_geometry_and_full_text() {
        adw::init().unwrap();
        gtk4::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        for (name, branch) in [
            ("short".to_string(), "main".to_string()),
            ("a".repeat(200), "main".to_string()),
            ("short".to_string(), "branch".repeat(40)),
            ("界".repeat(80), "分支".repeat(40)),
        ] {
            let cwd = PathBuf::from("/tmp").join(&name);
            let meta = jterm_core::git_meta::RepoMeta {
                branch: branch.clone(),
                dirty: false,
                ahead: None,
                behind: None,
            };
            let content = compose(&Snapshot {
                cwd: Some(&cwd),
                git: Some(&meta),
                cols: 80,
                rows: 24,
                tab_count: 2,
                ..Snapshot::default()
            });
            let (bar, left, right) = build_bottom_bar();
            set_segments(&left, &content.left);
            set_segments(&right, &content.right);
            let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            let filler = gtk4::Label::new(Some("inert status-bar fixture"));
            filler.set_vexpand(true);
            root.append(&filler);
            root.append(&bar);
            let window = adw::Window::builder()
                .default_width(500)
                .default_height(160)
                .content(&root)
                .build();
            window.present();
            let context = glib::MainContext::default();
            for _ in 0..50 {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            let minimum = bar.measure(gtk4::Orientation::Horizontal, -1).0;
            let right_bounds = right.compute_bounds(&window).unwrap();
            eprintln!(
                "cwd_bytes={} branch_bytes={} minimum={} window_width={} right_end={}",
                name.len(),
                branch.len(),
                minimum,
                window.width(),
                right_bounds.x() + right_bounds.width()
            );
            if name.len() == 200 {
                if let Some(path) = std::env::var_os("FORGE_STATUS_WIDTH_SCREENSHOT") {
                    let paintable = gtk4::WidgetPaintable::new(Some(&window));
                    let snapshot = gtk4::Snapshot::new();
                    paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
                    let renderer =
                        gtk4::gsk::Renderer::for_surface(&window.surface().unwrap()).unwrap();
                    renderer
                        .render_texture(snapshot.to_node().unwrap(), None)
                        .save_to_png(path)
                        .unwrap();
                    renderer.unrealize();
                }
            }
            assert!(
                minimum <= 500 && window.width() <= 500,
                "long status metadata must not widen a compact window"
            );
            assert!(
                right_bounds.x() >= 0.0
                    && right_bounds.x() + right_bounds.width() <= window.width() as f32,
                "grid and tab metadata must remain visible"
            );
            let mut child = left.first_child();
            let mut ellipsized = false;
            for segment in &content.left {
                let label = child.clone().unwrap().downcast::<gtk4::Label>().unwrap();
                assert_eq!(label.text().as_str(), segment.text);
                assert_eq!(label.tooltip_text().as_deref(), Some(segment.text.as_str()));
                ellipsized |= label.layout().is_ellipsized();
                child = child.unwrap().next_sibling();
            }
            assert_eq!(
                ellipsized,
                name.len() > 5 || branch.len() > 4,
                "only long rendered metadata should need ellipsization"
            );
            let mut child = right.first_child();
            for segment in &content.right {
                let label = child.clone().unwrap().downcast::<gtk4::Label>().unwrap();
                assert_eq!(label.text().as_str(), segment.text);
                assert!(!label.layout().is_ellipsized());
                child = child.unwrap().next_sibling();
            }
            window.close();
        }
    }
}
