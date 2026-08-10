//! Per-pane status header and the drag-to-rearrange gesture built on it.
//!
//! Both terminal backends put one of these at the top of their root box, so a
//! pane leaf carries its own chrome and the `Paned` split tree keeps holding
//! exactly one widget per pane. Nothing about splitting, closing, zooming, or
//! session snapshots has to know the strip exists.
//!
//! The strip stays hidden while a tab holds a single pane: the tab strip and
//! window title already name it, and the row would only cost a terminal line.
//! Once a tab is split it shows each pane's number, title, working directory
//! and running command, and doubles as the handle for swapping two panes.

use gtk4::prelude::*;
use gtk4::{gdk, glib};

use super::pane_dnd::{
    split_drop_zone, tab_payload_can_split, PaneDragPayload, SplitDropZone, TabDragPayload,
};

/// Style rules for the header strip, appended to the app's static CSS.
pub(crate) const PANE_HEADER_CSS: &str = "
    .pane-header {
        padding: 1px 6px;
        border-bottom: 1px solid alpha(currentColor, 0.15);
        background-color: alpha(currentColor, 0.06);
    }
    .pane-header.pane-header-focused {
        background-color: alpha(currentColor, 0.16);
        border-bottom-color: alpha(currentColor, 0.5);
    }
    .pane-header label { font-size: 0.82em; }
    .pane-header-index { font-weight: bold; opacity: 0.9; }
    .pane-header-title { font-weight: bold; }
    .pane-header-cwd { opacity: 0.6; }
    .pane-header-command { font-weight: 600; }
    .pane-drop-target { outline: 2px solid alpha(currentColor, 0.8); outline-offset: -2px; }
    .pane-tab-drop-left { box-shadow: inset 10px 0 alpha(currentColor, 0.5); }
    .pane-tab-drop-right { box-shadow: inset -10px 0 alpha(currentColor, 0.5); }
    .pane-tab-drop-up { box-shadow: inset 0 10px alpha(currentColor, 0.5); }
    .pane-tab-drop-down { box-shadow: inset 0 -10px alpha(currentColor, 0.5); }
    .pane-to-tab-drop-target { outline: 2px solid alpha(currentColor, 0.8); outline-offset: -2px; }
";

/// One pane's status strip.
pub(crate) struct PaneHeader {
    root: gtk4::Box,
    index: gtk4::Label,
    title: gtk4::Label,
    cwd: gtk4::Label,
    command: gtk4::Label,
}

impl PaneHeader {
    /// Build the strip, hidden. Tabs start with a single pane, which needs no
    /// header at all.
    pub(crate) fn new() -> Self {
        let index = gtk4::Label::new(None);
        index.add_css_class("pane-header-index");

        let title = gtk4::Label::new(None);
        title.add_css_class("pane-header-title");
        title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        title.set_xalign(0.0);

        let cwd = gtk4::Label::new(None);
        cwd.add_css_class("pane-header-cwd");
        cwd.set_ellipsize(gtk4::pango::EllipsizeMode::Middle);
        cwd.set_xalign(0.0);

        let command = gtk4::Label::new(None);
        command.add_css_class("pane-header-command");
        command.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        command.set_xalign(0.0);

        let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        root.add_css_class("pane-header");
        root.append(&index);
        root.append(&title);
        root.append(&cwd);
        root.append(&command);
        root.set_cursor_from_name(Some("grab"));
        root.set_tooltip_text(Some(
            "Drag onto another pane to swap, or onto the tab bar to detach",
        ));
        root.set_visible(false);

        PaneHeader {
            root,
            index,
            title,
            cwd,
            command,
        }
    }

    pub(crate) fn widget(&self) -> &gtk4::Box {
        &self.root
    }

    /// Show the strip only while its tab is split.
    pub(crate) fn set_header_visible(&self, visible: bool) {
        self.root.set_visible(visible);
    }

    pub(crate) fn set_focused(&self, focused: bool) {
        if focused {
            self.root.add_css_class("pane-header-focused");
        } else {
            self.root.remove_css_class("pane-header-focused");
        }
    }

    /// Fill in the strip. Empty fields are hidden rather than left blank, so a
    /// narrow pane spends its width on the fields that say something.
    pub(crate) fn set_status(
        &self,
        position: usize,
        title: &str,
        cwd: Option<&str>,
        command: Option<&str>,
    ) {
        self.index.set_text(&(position + 1).to_string());
        let title = crate::review_input::safe_inline_display(title, 512);
        self.title.set_text(&title);
        match cwd {
            // The title is usually the directory's last component; repeating
            // the whole path only earns its space when it differs.
            Some(cwd) if cwd != title.as_str() => {
                let cwd = crate::review_input::safe_inline_display(cwd, 4 * 1024);
                self.cwd.set_text(&cwd);
                self.cwd.set_visible(true);
            }
            _ => self.cwd.set_visible(false),
        }
        match command {
            Some(command) => {
                let command = crate::review_input::safe_inline_display(command, 512);
                self.command.set_text(&format!("▶ {command}"));
                self.command.set_visible(true);
            }
            None => self.command.set_visible(false),
        }
    }

    /// Make the strip a drag source carrying `session_id`, the pane's stable
    /// identity. Indices and tab numbers both shift while a drag is in flight.
    pub(crate) fn install_drag_source(&self, session_id: impl Fn() -> Option<String> + 'static) {
        let source = gtk4::DragSource::new();
        source.set_actions(gdk::DragAction::MOVE);
        source.connect_prepare(move |_, _, _| {
            let payload = PaneDragPayload(session_id()?);
            Some(gdk::ContentProvider::for_value(&payload.to_value()))
        });
        self.root.add_controller(source);
    }
}

impl Default for PaneHeader {
    fn default() -> Self {
        Self::new()
    }
}

/// Accept pane drops anywhere inside `leaf_root`, calling `on_drop` with the
/// dragged pane's session id.
///
/// The highlight closures hold `leaf_root` weakly. A strong capture would make
/// the widget own a controller that owns the widget, and GTK would never free
/// the pane — taking its PTY and terminal buffer with it.
pub(crate) fn install_pane_drop_target(
    leaf_root: &gtk4::Widget,
    on_drop: impl Fn(&str) -> bool + 'static,
) {
    fn set_highlight(root: &glib::WeakRef<gtk4::Widget>, on: bool) {
        if let Some(root) = root.upgrade() {
            if on {
                root.add_css_class("pane-drop-target");
            } else {
                root.remove_css_class("pane-drop-target");
            }
        }
    }

    let target = gtk4::DropTarget::new(PaneDragPayload::static_type(), gdk::DragAction::MOVE);

    let highlighted = leaf_root.downgrade();
    target.connect_enter(move |_, _, _| {
        set_highlight(&highlighted, true);
        gdk::DragAction::MOVE
    });
    let highlighted = leaf_root.downgrade();
    target.connect_leave(move |_| set_highlight(&highlighted, false));
    let highlighted = leaf_root.downgrade();
    target.connect_drop(move |_, value, _, _| {
        set_highlight(&highlighted, false);
        match value.get::<PaneDragPayload>() {
            Ok(dragged) => on_drop(&dragged.0),
            Err(_) => false,
        }
    });
    leaf_root.add_controller(target);
}

fn clear_tab_split_drop_classes(root: &gtk4::Widget) {
    for class in [
        "pane-tab-drop-left",
        "pane-tab-drop-right",
        "pane-tab-drop-up",
        "pane-tab-drop-down",
    ] {
        root.remove_css_class(class);
    }
}

fn set_tab_split_drop_class(root: &gtk4::Widget, zone: Option<SplitDropZone>) {
    clear_tab_split_drop_classes(root);
    let class = match zone {
        Some(SplitDropZone::Left) => "pane-tab-drop-left",
        Some(SplitDropZone::Right) => "pane-tab-drop-right",
        Some(SplitDropZone::Up) => "pane-tab-drop-up",
        Some(SplitDropZone::Down) => "pane-tab-drop-down",
        None => return,
    };
    root.add_css_class(class);
}

/// Accept a single-pane tab over the four outer drop zones of one live pane.
///
/// The target controller is distinct from the pane-swap controller, and both
/// payloads are private boxed GTypes, so neither path can be mistaken for a VTE
/// text drop. The callback performs live identity resolution and the structural
/// transaction; this layer owns only pointer geometry and transient feedback.
pub(crate) fn install_tab_split_drop_target(
    leaf_root: &gtk4::Widget,
    on_drop: impl Fn(&str, SplitDropZone) -> bool + 'static,
) {
    let target = gtk4::DropTarget::new(TabDragPayload::static_type(), gdk::DragAction::MOVE);
    target.set_preload(true);

    let highlighted = leaf_root.downgrade();
    target.connect_motion(move |target, x, y| {
        let eligible = target
            .value()
            .and_then(|value| value.get::<TabDragPayload>().ok())
            .is_some_and(|payload| tab_payload_can_split(&payload));
        let zone = highlighted.upgrade().and_then(|root| {
            let zone = eligible
                .then(|| split_drop_zone(root.width(), root.height(), x, y))
                .flatten();
            set_tab_split_drop_class(&root, zone);
            zone
        });
        if zone.is_some() {
            gdk::DragAction::MOVE
        } else {
            gdk::DragAction::empty()
        }
    });

    let highlighted = leaf_root.downgrade();
    target.connect_leave(move |_| {
        if let Some(root) = highlighted.upgrade() {
            clear_tab_split_drop_classes(&root);
        }
    });

    let highlighted = leaf_root.downgrade();
    target.connect_drop(move |_, value, x, y| {
        let Some(root) = highlighted.upgrade() else {
            return false;
        };
        clear_tab_split_drop_classes(&root);
        let Some(zone) = split_drop_zone(root.width(), root.height(), x, y) else {
            return false;
        };
        let Ok(payload) = value.get::<TabDragPayload>() else {
            return false;
        };
        let Some(session_id) = payload.pane_session_id.as_deref() else {
            return false;
        };
        on_drop(session_id, zone)
    });
    leaf_root.add_controller(target);
}

/// Where a pane sits inside the split tree.
enum PaneSlot {
    Start(gtk4::Paned),
    End(gtk4::Paned),
}

impl PaneSlot {
    fn of(widget: &gtk4::Widget) -> Option<Self> {
        let paned = widget.parent()?.downcast::<gtk4::Paned>().ok()?;
        if paned.start_child().as_ref() == Some(widget) {
            Some(PaneSlot::Start(paned))
        } else if paned.end_child().as_ref() == Some(widget) {
            Some(PaneSlot::End(paned))
        } else {
            None
        }
    }

    fn clear(&self) {
        match self {
            PaneSlot::Start(paned) => paned.set_start_child(None::<&gtk4::Widget>),
            PaneSlot::End(paned) => paned.set_end_child(None::<&gtk4::Widget>),
        }
    }

    fn fill(&self, widget: &gtk4::Widget) {
        match self {
            PaneSlot::Start(paned) => paned.set_start_child(Some(widget)),
            PaneSlot::End(paned) => paned.set_end_child(Some(widget)),
        }
    }
}

/// Exchange two panes' positions in the split tree, leaving the tree shape and
/// every divider position exactly as the user arranged them.
///
/// Both slots are cleared before either is refilled: handing GTK a widget that
/// still has a parent would leave the tree half-updated. This is a temporary
/// reparent, so the leaves' `PaneLeaf` qdata must stay attached throughout.
pub(crate) fn swap_pane_widgets(a: &gtk4::Widget, b: &gtk4::Widget) -> bool {
    if a == b {
        return false;
    }
    let (Some(a_slot), Some(b_slot)) = (PaneSlot::of(a), PaneSlot::of(b)) else {
        return false;
    };
    a_slot.clear();
    b_slot.clear();
    a_slot.fill(b);
    b_slot.fill(a);
    true
}

/// Working directory with `$HOME` collapsed to `~`, for the header.
pub(crate) fn abbreviate_home(path: &str) -> String {
    match glib::home_dir().to_str() {
        Some(home) => abbreviate_prefix(path, home),
        None => path.to_string(),
    }
}

/// The substitution itself, with `home` supplied rather than read from the
/// environment so it is testable without mutating process-wide state.
fn abbreviate_prefix(path: &str, home: &str) -> String {
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    // Only at a component boundary: `/home/user2` merely shares a prefix with
    // `/home/user` and is a different directory.
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Header title for one pane: its OSC title, else its directory's last
/// component, else a positional fallback.
pub(crate) fn pane_header_title(
    osc_title: Option<&str>,
    cwd: Option<&str>,
    position: usize,
) -> String {
    if let Some(title) = osc_title.map(str::trim) {
        if !title.is_empty() {
            return title.to_string();
        }
    }
    cwd.map(abbreviate_home)
        .filter(|cwd| !cwd.is_empty())
        .map(|cwd| {
            // `~` and `/` have no last component worth showing on their own.
            std::path::Path::new(&cwd)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .unwrap_or(cwd)
        })
        .unwrap_or_else(|| format!("Pane {}", position + 1))
}

#[cfg(test)]
mod tests {
    use super::{abbreviate_prefix, pane_header_title};

    #[test]
    fn home_is_abbreviated_only_at_a_component_boundary() {
        assert_eq!(abbreviate_prefix("/home/user", "/home/user"), "~");
        assert_eq!(abbreviate_prefix("/home/user/src", "/home/user"), "~/src");
        // A sibling directory that merely shares the prefix must stay intact.
        assert_eq!(
            abbreviate_prefix("/home/user2/src", "/home/user"),
            "/home/user2/src"
        );
        assert_eq!(abbreviate_prefix("/etc", "/home/user"), "/etc");
        assert_eq!(abbreviate_prefix("/etc", ""), "/etc");
    }

    #[test]
    fn title_prefers_osc_then_directory_then_position() {
        assert_eq!(
            pane_header_title(Some("vim README"), Some("/tmp"), 0),
            "vim README"
        );
        // Whitespace-only OSC titles must not blank the header.
        assert_eq!(pane_header_title(Some("   "), Some("/tmp/work"), 0), "work");
        assert_eq!(pane_header_title(None, Some("/tmp/work"), 0), "work");
        // A path with no last component keeps whatever it does have.
        assert_eq!(pane_header_title(None, Some("/"), 0), "/");
        assert_eq!(pane_header_title(None, None, 2), "Pane 3");
        assert_eq!(pane_header_title(Some(""), None, 0), "Pane 1");
    }
}
