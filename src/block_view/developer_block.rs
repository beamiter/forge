//! Developer Block presentation: native record cards over one existing PTY.
//! The gutter never enters the terminal byte stream or its native selection.
use super::*;
use std::cell::RefCell;

pub(super) const HEADER_HEIGHT: i32 = 40;
pub(super) const CARD_GAP: i32 = 10;
pub(super) const GUTTER_WIDTH: i32 = 24;
pub(super) const GUTTER_GAP: i32 = 16;
pub(super) const OUTPUT_PADDING: i32 = 6;

pub(super) fn estimated_height(cell_height: i32, command_rows: i64, output_rows: i64) -> i32 {
    let output = if output_rows > 0 {
        super::alt_screen::finished_vte_height_px(output_rows, cell_height)
            .saturating_add(OUTPUT_PADDING * 2)
    } else {
        0
    };
    HEADER_HEIGHT
        .max(super::alt_screen::finished_vte_height_px(
            command_rows.max(1),
            cell_height,
        ))
        .saturating_add(output)
        .saturating_add(2 + CARD_GAP)
}

/// Logical source line starts in the same visual-row model used to fit the
/// native snapshot. Wrapped continuations deliberately have an empty gutter.
fn logical_starts(text: &str, cols: i64) -> (Vec<(i64, usize)>, i64) {
    // Match native render_bytes_into_finished_vte exactly: remove one raw
    // display boundary before replaying ANSI. Preserve all other blank rows.
    let normalized = super::blocks::output_display_text(text);
    let plain = super::ansi::strip_ansi(normalized);
    if plain.is_empty() {
        return (Vec::new(), 0);
    }
    let display = plain.as_str();
    let mut starts = Vec::new();
    let mut visual = 0;
    for (line, text) in display.split('\n').enumerate() {
        starts.push((visual, line + 1));
        visual += super::blocks::output_visual_row_count(text, cols).max(1);
    }
    (starts, visual)
}

pub(super) fn line_gutter(
    terminal: &Terminal,
    original: Rc<RefCell<String>>,
    shown: Rc<RefCell<Option<String>>>,
    generation: Rc<Cell<u64>>,
) -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::new();
    area.add_css_class("developer-output-gutter");
    area.set_content_width(GUTTER_WIDTH);
    area.set_margin_end(GUTTER_GAP);
    area.set_can_target(false);
    area.set_focusable(false);
    area.set_tooltip_text(Some("Logical output lines; soft-wrapped continuations have no number. Numbers are excluded from native selection and copied text."));
    let weak = terminal.downgrade();
    type Cache = Option<(u64, i64, Vec<(i64, usize)>, i64)>;
    let cache: RefCell<Cache> = RefCell::new(None);
    area.set_draw_func(move |area, cr, width, height| {
        let Some(vte) = weak.upgrade() else {
            return;
        };
        let cols = vte.column_count().max(1);
        let stamp = generation.get();
        let mut cache = cache.borrow_mut();
        if cache
            .as_ref()
            .is_none_or(|(old, old_cols, _, _)| *old != stamp || *old_cols != cols)
        {
            let full = original.borrow();
            let override_text = shown.borrow();
            let (starts, rows) =
                logical_starts(override_text.as_deref().unwrap_or(full.as_str()), cols);
            *cache = Some((stamp, cols, starts, rows));
        }
        let Some((_, _, starts, source_rows)) = cache.as_ref() else {
            return;
        };
        let Some(adj) = vte.vadjustment() else {
            return;
        };
        let cell = vte.char_height().max(1) as f64;
        let skipped = (*source_rows - adj.upper().ceil() as i64).max(0);
        let first = adj.value().floor() as i64 + skipped;
        let fraction = adj.value().fract();
        let color = area.color();
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            1.0,
        );
        let font = vte.font();
        let family = font
            .as_ref()
            .and_then(|f| f.family())
            .map(|f| f.to_string())
            .unwrap_or_else(|| "monospace".into());
        cr.select_font_face(
            family.split(',').next().unwrap_or("monospace"),
            gtk4::cairo::FontSlant::Normal,
            gtk4::cairo::FontWeight::Normal,
        );
        cr.set_font_size((cell * 0.6).max(8.0));
        let start = starts.partition_point(|(row, _)| *row < first);
        for (physical, line) in starts[start..].iter() {
            let y = ((*physical - first) as f64 - fraction) * cell;
            if y > f64::from(height) {
                break;
            }
            let text = line.to_string();
            if let Ok(extents) = cr.text_extents(&text) {
                cr.move_to(
                    (f64::from(width) - extents.x_advance() - 3.0).max(0.0),
                    y + cell * 0.76,
                );
                let _ = cr.show_text(&text);
            }
        }
    });
    if let Some(adj) = terminal.vadjustment() {
        let weak = area.downgrade();
        adj.connect_value_changed(move |_| {
            if let Some(area) = weak.upgrade() {
                area.queue_draw();
            }
        });
        let weak = area.downgrade();
        adj.connect_changed(move |_| {
            if let Some(area) = weak.upgrade() {
                area.queue_draw();
            }
        });
    }
    let weak = area.downgrade();
    terminal.connect_contents_changed(move |_| {
        if let Some(area) = weak.upgrade() {
            area.queue_draw();
        }
    });
    area
}

/// Default colors only; ANSI palette/SGR bytes, selection and copy stay native.
pub(super) fn terminal_colors(terminal: &Terminal) {
    terminal.set_color_foreground(&gtk4::gdk::RGBA::new(
        213.0 / 255.0,
        222.0 / 255.0,
        236.0 / 255.0,
        1.0,
    ));
    terminal.set_color_background(&gtk4::gdk::RGBA::new(
        14.0 / 255.0,
        22.0 / 255.0,
        33.0 / 255.0,
        1.0,
    ));
}

/// The current command's real lifecycle header. Its terminal remains the
/// original ActiveBlock; this component has no input and launches no process.
pub(super) struct RunningHeader {
    pub(super) root: gtk4::Box,
    title: gtk4::Label,
    elapsed: gtk4::Label,
    started: Rc<Cell<Option<std::time::Instant>>>,
}
impl RunningHeader {
    pub(super) fn new() -> Self {
        let root = gtk4::Box::new(Orientation::Horizontal, 8);
        root.add_css_class("developer-command-header");
        root.add_css_class("developer-running-header");
        root.set_height_request(HEADER_HEIGHT);
        root.set_margin_start(16);
        root.set_margin_end(12);
        root.set_visible(false);
        let icon = crate::ui::reference_shell::vector_icon(
            jterm_core::reference_icons::Icon::Terminal,
            24,
        );
        icon.add_css_class("developer-command-icon");
        icon.set_margin_end(8);
        root.append(&icon);
        let title = gtk4::Label::new(None);
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_width_chars(1);
        title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        title.add_css_class("developer-running-command");
        root.append(&title);
        let status = gtk4::Label::new(Some("Running"));
        status.add_css_class("developer-command-status");
        status.set_valign(gtk4::Align::Center);
        status.set_height_request(20);
        status.set_accessible_role(gtk4::AccessibleRole::Status);
        root.append(&status);
        let elapsed = gtk4::Label::new(None);
        elapsed.add_css_class("developer-command-elapsed");
        elapsed.set_valign(gtk4::Align::Center);
        elapsed.set_tooltip_text(Some("Elapsed since observed shell command start"));
        root.append(&elapsed);
        let started = Rc::new(Cell::new(None::<std::time::Instant>));
        let clock = started.clone();
        let weak = elapsed.downgrade();
        glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
            let Some(label) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if let Some(start) = clock.get() {
                let text = super::blocks::format_block_duration(
                    start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                );
                if label.text().as_str() != text {
                    label.set_text(&text);
                }
            }
            glib::ControlFlow::Continue
        });
        Self {
            root,
            title,
            elapsed,
            started,
        }
    }
    pub(super) fn begin(&self, command: &str) {
        let safe = jterm_core::review_input::safe_multiline_display(command, 4096);
        self.title.set_text(&safe);
        self.title.set_tooltip_text(Some(&safe));
        self.started.set(Some(std::time::Instant::now()));
        self.elapsed.set_text("0ms");
    }
    pub(super) fn finish(&self) {
        self.started.set(None);
        self.root.set_visible(false);
        if let Some(parent) = self.root.parent() {
            parent.remove_css_class("developer-running");
        }
    }
    pub(super) fn is_running_command(&self) -> bool {
        self.started.get().is_some() && !self.title.text().trim().is_empty()
    }
    pub(super) fn sync(&self, running: bool) {
        self.root
            .set_visible(running && self.started.get().is_some());
        if let Some(parent) = self.root.parent() {
            if running && self.started.get().is_some() {
                parent.add_css_class("developer-running");
            } else {
                parent.remove_css_class("developer-running");
            }
        }
    }
}

pub(super) fn live_toolbar() -> (gtk4::Box, gtk4::Label, gtk4::Button, gtk4::Button) {
    let root = gtk4::Box::new(Orientation::Horizontal, 8);
    root.add_css_class("developer-live-toolbar");
    root.set_height_request(36);
    let count = gtk4::Label::new(Some("Live · 0 commands"));
    count.set_hexpand(true);
    count.set_xalign(0.0);
    count.set_tooltip_text(Some(
        "Retained completed commands plus the current observed command. Background output is excluded.",
    ));
    root.append(&count);
    let search = gtk4::Button::new();
    search.add_css_class("flat");
    search.set_focus_on_click(false);
    search.set_child(Some(&crate::ui::reference_shell::vector_icon(
        jterm_core::reference_icons::Icon::Search,
        16,
    )));
    search.set_tooltip_text(Some("Search this canvas"));
    root.append(&search);
    let follow = gtk4::Button::new();
    follow.add_css_class("flat");
    follow.set_focus_on_click(false);
    follow.set_child(Some(&crate::ui::reference_shell::vector_icon(
        jterm_core::reference_icons::Icon::ChevronDown,
        16,
    )));
    follow.set_tooltip_text(Some("Jump to live terminal"));
    root.append(&follow);
    (root, count, search, follow)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gutter_numbers_logical_lines_and_leaves_wrap_continuations_empty() {
        assert_eq!(logical_starts("abcde\nz\n", 3), (vec![(0, 1), (2, 2)], 3));
        assert_eq!(
            logical_starts("\x1b[36mabcde\x1b[0m\nz\n", 3),
            (vec![(0, 1), (2, 2)], 3)
        );
        assert_eq!(logical_starts("界界\nz", 3), (vec![(0, 1), (2, 2)], 3));
    }
    #[test]
    fn gutter_reuses_native_boundaries_before_ansi_and_preserves_blank_lines() {
        assert_eq!(
            logical_starts("\r\nabcde\r\nz\r\n", 3),
            (vec![(0, 1), (2, 2)], 3)
        );
        assert_eq!(
            logical_starts("a\n\n\n", 80),
            (vec![(0, 1), (1, 2), (2, 3)], 3)
        );
        assert_eq!(
            logical_starts("\x1b[31m\r\nA\x1b[0m\r\n", 80),
            (vec![(0, 1), (1, 2)], 2)
        );
        assert_eq!(logical_starts("界界\n", 3), (vec![(0, 1)], 2));
    }
}
