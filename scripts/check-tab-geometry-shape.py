#!/usr/bin/env python3
"""Headless drift guard for the native production-shape geometry fixture.

This checks source correspondence, not native allocations or input delivery.
"""
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parents[1]


def compact(text):
    return re.sub(r"\s+", "", re.sub(r"//[^\n]*", "", text))


def section(source, start, end):
    assert source.count(start) == 1, start
    return source[source.index(start):source.index(end, source.index(start))]


def main():
    source = (ROOT / "src/main.rs").read_text()
    tabs = (ROOT / "src/ui/tabs.rs").read_text()
    resize = (ROOT / "src/ui/tab_strip.rs").read_text()
    fixture = (ROOT / "src/ui/tab_geometry_tests.rs").read_text()
    checks = []
    scroll = section(source, "let top_tab_scroll = ScrolledWindow::new();", "// Overlay the leading controls")
    checks.append(("scroller", scroll.replace("set_visible(false)", "set_visible(true)")))
    shape = section(tabs, "let strip_label = Label::new(Some(&label_text));", "// Keep an explicit handle to the title label.")
    shape = shape.replace("*strip_btn_label.borrow_mut() = Some(strip_label.clone());", "").replace("if remote.is_some()", "if status")
    checks.append(("new tab children", shape))
    handle = section(resize, "let handle = gtk4::Box::new(gtk4::Orientation::Vertical, 0);", "unsafe {\n            button.set_data::<gtk4::Box>(TAB_WIDTH_HANDLE_DATA")
    checks.append(("resize handle", handle.replace("content.append", "strip_box.append")))
    css = section(source, '".tab-strip-btn {', '",\n        ));')[1:]
    checks.append(("production tab CSS", css))
    for name, fragment in checks:
        assert compact(fragment) in compact(fixture), f"Fixture drift: {name}"
    # Both moved and fresh tabs must retain identical layout-bearing title/status
    # properties and child order. The fixture combines visibility permutations.
    moved = section(tabs, 'let strip_label = Label::new(Some(if private_title.get()', '// Header and strip rename remain in lockstep.')
    for line in [
        'strip_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);',
        'strip_label.set_hexpand(true);', 'strip_label.set_xalign(0.0);',
        'process_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);',
        'process_label.set_max_width_chars(15);',
        'let strip_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);',
    ]:
        assert compact(line) in compact(moved), line
        assert compact(line) in compact(shape), line
    order = ['&conn_dot', '&strip_label', '&process_label', '&pin_icon']
    for text in [moved, shape]:
        positions = [text.index(f'strip_box.append({child});') for child in order]
        assert positions == sorted(positions)
    layout = (ROOT / 'src/ui/layout.rs').read_text()
    for production, native in [
        ('self.tab_strip.set_width_request(1);', 'strip.set_width_request(1);'),
        ('self.tab_strip.set_orientation(Orientation::Horizontal);', 'strip.set_orientation(gtk::Orientation::Horizontal);'),
        ('self.tab_strip.set_valign(gtk4::Align::Center);', 'strip.set_valign(gtk::Align::Center);'),
        ('self.tab_strip.set_hexpand(true);', 'strip.set_hexpand(true);'),
        ('self.tab_strip.set_vexpand(false);', 'strip.set_vexpand(false);'),
        ('btn.set_hexpand(false);', 'strip_btn.set_hexpand(false);'),
        ('btn.set_width_request(self.config.borrow().tab_width as i32);', 'strip_btn.set_width_request(width);'),
    ]:
        assert compact(production) in compact(layout), production
        assert compact(native) in compact(fixture), native
    for production, native in [
        ('top_bar.set_measure_overlay(&top_tab_scroll, true);', 'top.set_measure_overlay(&scroll, true);'),
        ('top_bar.set_height_request(40);', 'top.set_height_request(40);'),
        ('top_bar.set_hexpand(true);', 'top.set_hexpand(true);'),
        ('top_bar_handle.add_css_class("forge-font-chrome");', 'handle.add_css_class("forge-font-chrome");'),
    ]:
        assert compact(production) in compact(source), production
        assert compact(native) in compact(fixture), native
    assert 'ui::install_top_tab_geometry_reveal(&top_tab_scroll);' in source
    assert 'super::tab_strip::install_top_tab_geometry_reveal(&scroll);' in fixture
    assert 'super::tab_strip::install_tab_reveal_on_map(button);' in tabs
    assert 'super::tab_strip::sync_strip_buttons_active(&strip, (count - 1) as u32);' in fixture
    assert 'font_groups_stay_independent_across_native_panes_and_late_rows' in (ROOT / 'src/block_view/font_sync_tests.rs').read_text()
    print('PASS: scroller, full production CSS, fresh/moved tab shape, resize handle, placement and overlay source contracts')


if __name__ == '__main__':
    main()
