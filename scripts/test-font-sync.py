#!/usr/bin/env python3
"""Execute the checkout's font-size helper against installed native Pango.

No Cargo dependencies or display are needed. The existing GTK display CI job
provides Pango; the FFI boundary calls the same parser/getters as gtk-rs. Optional
--root checks a sibling frontend's identical helper without modifying it.
"""
from pathlib import Path
import argparse
import ctypes.util
import hashlib
import json
import os
import re
import subprocess
import tempfile
import textwrap

ROOT = Path(__file__).resolve().parents[1]


def item(source, signature):
    assert source.count(signature) == 1, signature
    start = source.index(signature)
    line = source.rfind("\n", 0, start) + 1
    indent = source[line:start]
    opening = source.index("{", start)
    closing = re.search(r"^" + re.escape(indent) + r"}[ \t]*$", source[opening:], re.M)
    assert closing, signature
    return textwrap.dedent(source[line:opening + closing.end()])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    source = (args.root / "src/font.rs").read_text()
    helper = item(source, "pub(crate) fn effective_font_size_css(")
    config = (args.root / "src/config.rs").read_text()
    default = re.search(r'const DEFAULT_FONT_DESC: &str = ("[^"\n]*");', config).group(1)
    library = ctypes.util.find_library("pango-1.0")
    if not library:
        raise SystemExit("Native Pango runtime required; run in the GTK development environment")
    fixture = r'''
#![allow(dead_code)]
use std::ffi::{c_char, c_int, c_void, CStr, CString};
unsafe extern "C" {
    fn pango_font_description_from_string(desc: *const c_char) -> *mut c_void;
    fn pango_font_description_get_size(desc: *const c_void) -> c_int;
    fn pango_font_description_get_size_is_absolute(desc: *const c_void) -> c_int;
    fn pango_font_description_free(desc: *mut c_void);
    fn pango_font_description_set_family(desc: *mut c_void, family: *const c_char);
    fn pango_font_description_set_size(desc: *mut c_void, size: c_int);
    fn pango_font_description_set_absolute_size(desc: *mut c_void, size: f64);
    fn pango_font_description_to_string(desc: *const c_void) -> *mut c_char;
    fn g_free(ptr: *mut c_void);
}
struct FontDescription(*mut c_void);
impl FontDescription {
    fn from_string(desc: &str) -> Self {
        let desc = CString::new(desc).unwrap();
        let font = Self(unsafe { pango_font_description_from_string(desc.as_ptr()) });
        assert!(!font.0.is_null());
        font
    }
    fn size(&self) -> i32 { unsafe { pango_font_description_get_size(self.0) } }
    fn set_family(&mut self, family: &str) {
        let family = CString::new(family).unwrap();
        unsafe { pango_font_description_set_family(self.0, family.as_ptr()) }
    }
    fn set_size(&mut self, size: i32) { unsafe { pango_font_description_set_size(self.0, size) } }
    fn set_absolute_size(&mut self, size: f64) {
        unsafe { pango_font_description_set_absolute_size(self.0, size) }
    }
    fn to_string(&self) -> String {
        unsafe {
            let value = pango_font_description_to_string(self.0);
            let text = CStr::from_ptr(value).to_string_lossy().into_owned();
            g_free(value.cast());
            text
        }
    }
    fn is_size_absolute(&self) -> bool {
        unsafe { pango_font_description_get_size_is_absolute(self.0) != 0 }
    }
}
impl Drop for FontDescription {
    fn drop(&mut self) { unsafe { pango_font_description_free(self.0) } }
}
mod gtk { pub mod pango { pub const SCALE: i32 = 1024; } }
mod config { pub const DEFAULT_FONT_DESC: &str = @default@; }
@helper@
#[test]
fn native_pango_units_styles_fractions_and_zoom() {
    for (desc, scale, expected, unit) in [
        ("Monospace Bold Italic 12.5", 1.2, 15.0, "pt"),
        ("Monospace 17px", 1.5, 25.5, "px"),
        ("Monospace 12", 0.75, 9.0, "pt"),
        ("Monospace, Serif 13.5", 1.025, 13.8375, "pt"),
        ("Monospace 14", 0.1, 1.4, "pt"),
        ("Monospace 14", 10.0, 140.0, "pt"),
        ("Monospace", 1.0, 14.0, "pt"),
    ] {
        let value = effective_font_size_css(desc, scale);
        let size: f64 = value.strip_suffix(unit).unwrap().parse().unwrap();
        assert!((size - expected).abs() < 1e-9, "{desc}, {scale}: {value}");
    }
}
#[test]
fn native_pango_fallback_is_finite_and_reset_is_exact() {
    for scale in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
        assert_eq!(effective_font_size_css("Monospace 14", scale), "14pt");
    }
    assert_eq!(effective_font_size_css("Monospace 14", 1.0), "14pt");
}
'''.replace("@helper@", helper).replace("@default@", default)
    if args.root.resolve() == ROOT:
        for helper_name in ["window_font_css", "unscaled_font_size", "font_description_with_family", "font_description_with_size"]:
            fixture += "\n" + item(source, f"pub(crate) fn {helper_name}(")
        policy_path = json.dumps(str((ROOT / "src/font_zoom.rs").resolve()))
        fixture += f"\n#[path = {policy_path}] mod font_zoom;\n"
        for name in ["chrome_size_preserves_pango_units_and_fractional_zoom", "chrome_size_is_scoped_to_text_and_preserves_icon_geometry", "size_only_description_retains_units_and_fractional_points", "settings_font_size_changes_do_not_cross_group_boundaries", "settings_edits_preserve_units_fractional_sizes_and_style"]:
            fixture += "\n#[test]\n" + item(source, f"fn {name}(")
        controller = (ROOT / "src/ui/config_apply.rs").read_text()
        harness = (ROOT / "scripts/font_sync_harness.rs").read_text()
        for slot, signature in [
            ("apply-scale", "pub(crate) fn apply_font_scale("),
            ("apply-group-scale", "pub(crate) fn apply_font_group_scale("),
            ("set-scale-all", "pub(crate) fn set_font_scale_all("),
            ("apply-font-all", "pub(crate) fn apply_font_all("),
            ("claim-sweep", "fn claim_font_scale_sweep("),
        ]:
            marker = f"// @font-sync:{slot}"
            assert harness.count(marker) == 1
            harness = harness.replace(marker, item(controller, signature))
        settings = (ROOT / "src/ui/dialogs.rs").read_text()
        for slot, row in [("main-size", "font_size_row"), ("chrome-size", "chrome_size_row")]:
            signature = f'{row}.connect_notify_local(Some("value"), move |row, _| {{'
            assert settings.count(signature) == 1, signature
            body = settings.split(signature, 1)[1].split("\n        });", 1)[0]
            marker = f"// @font-sync:{slot}"
            assert harness.count(marker) == 1
            harness = harness.replace(marker, textwrap.dedent(body))
        fixture += "\n" + "\n".join(line for line in harness.splitlines() if not line.startswith("//!"))
        main_source = (ROOT / "src/main.rs").read_text()
        assert 'window.add_css_class("forge-window");' in main_source
        assert 'ui.apply_dynamic_css();' in main_source
        shell = (ROOT / "src/ui/reference_shell.rs").read_text()
        assert 'let canvas_header = ui::reference_shell::center_header(&top_tab_scroll);' in main_source
        assert 'right_col.append(&canvas_header);' in main_source
        assert 'header.append(top_tabs);' in shell
        assert 'top_tabs.add_css_class("forge-font-chrome");' in shell
        routing = re.sub(r"\s+", "", main_source)
        for root, group in [('canvas_header', 'Chrome'), ('notebook', 'Content'), ('search_bar', 'Content')]:
            assert f'({root}.clone().upcast::<gtk4::Widget>(),FontGroup::{group})' in routing
        assert '(right_col.clone().upcast::<gtk4::Widget>(),FontGroup::Content)' not in routing
        assert 'font_roots.push((composer,FontGroup::Content));' in routing
        assert 'top_tab_scroll.set_propagate_natural_width(false);' in main_source
        compact = re.sub(r"\s+", "", controller)
        assert 'crate::font::window_font_css(&config.font_desc,self.font_scale.get(),&config.chrome_font_size,config.chrome_font_scale,)' in compact
        assert '{font_css}' in controller
        css = (ROOT / "src/block_view/css.rs").read_text()
        assert 'crate::font::effective_font_size_css(&config.font_desc, config.default_font_scale)' in css
        for action in ['FontIncrease', 'FontDecrease', 'FontReset']:
            actions = (ROOT / "src/ui/actions.rs").read_text()
            arm = actions.split(f'Action::{action} => {{', 1)[1].split('\n            }', 1)[0]
            assert 'self.apply_font_scale(' in arm
        print("Startup, dynamic CSS, Block/Chat CSS, keyboard actions and top-bar measurement wiring verified", flush=True)

    with tempfile.TemporaryDirectory(prefix="font-sync-") as directory:
        path = Path(directory) / "font_sync.rs"
        binary = path.with_suffix("")
        path.write_text(fixture)
        subprocess.run([os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test", str(path),
                        "-l", f"dylib:+verbatim={library}", "-l", f"dylib:+verbatim={ctypes.util.find_library('glib-2.0')}", "-o", str(binary)], check=True)
        print(f"{args.root.name}: production helper + native {library}; "
              f"ELF SHA256 {hashlib.sha256(binary.read_bytes()).hexdigest()}", flush=True)
        subprocess.run([str(binary), "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
