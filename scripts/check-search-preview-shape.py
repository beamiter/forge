#!/usr/bin/env python3
"""Read-only wiring guard; complements model and hosted native tag tests.

This is source-shape coverage, not a substitute for a GTK interaction test.
"""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[1]


def function(source, signature):
    assert source.count(signature) == 1, signature
    start = source.index(signature)
    end = source.index("\n    }", start) + len("\n    }")
    return source[start:end]


class PreviewWiring(unittest.TestCase):
    def test_button_is_connected_to_independent_dialog(self):
        main = (ROOT / "src/main.rs").read_text()
        self.assertIn('search_box.append(&search_preview_btn);', main)
        self.assertRegex(main, r'search_preview_btn\.connect_clicked\(move \|_\| \{\s*ui_for_search_preview\.show_exact_search_preview\(\);\s*\}\);')
        self.assertIn("present_exact_preview(&self.window, owner, original.as_str())", (ROOT / "src/ui/search_preview.rs").read_text())
        self.assertIn("mod search_preview;", (ROOT / "src/ui/mod.rs").read_text())
        self.assertIn("pub(crate) mod search_preview;", (ROOT / "src/block_view/mod.rs").read_text())

    def test_capture_uses_completed_records_and_propagates_limits(self):
        source = (ROOT / "src/block_view/find.rs").read_text()
        capture = function(source, "    pub(crate) fn capture_search_preview(")
        self.assertIn("self.render_backend.records()", capture)
        self.assertIn("records.len() > 4096", capture)
        self.assertIn("Snapshot::new()?", capture)
        for field in ["Prompt", "Command", "Output"]:
            self.assertIn("Field::" + field, capture)
        self.assertEqual(capture.count("snapshot.push("), 3)
        self.assertIn("snapshot.missing_prompts += 1", capture)
        self.assertIn("snapshot.missing_outputs += 1", capture)
        self.assertIn("block.command_truncated", capture)
        self.assertIn("block.output_notice.is_some()", capture)
        self.assertIn("output.truncated", capture)
        self.assertNotRegex(capture, r"active_vte|text_range|write_contents|search_find|select_all")

    def test_preview_cannot_select_terminal_or_navigate_current_pane(self):
        source = (ROOT / "src/ui/search_preview.rs").read_text()
        code = re.sub(r"//[^\n]*", "", source)
        self.assertNotRegex(code, r"search_find|search_set|select_range|select_all|clipboard|primary_clipboard|vte4")
        opening = function(code, "    pub(crate) fn show_exact_search_preview(")
        self.assertEqual(code.count("self.search_targets()"), 1)
        self.assertIn("let Some(owner) = self.search_targets().1 else {", opening)
        self.assertNotRegex(code, r"self\.current_(?:terminal|term_view)\(\)")
        # Resolve the captured Find owner once, without changing live Find or
        # falling back to a sibling when a pinned owner is unavailable.
        targets = function(
            (ROOT / "src/ui/search.rs").read_text(),
            "    pub(super) fn search_targets(",
        )
        targets = re.sub(r"//[^\n]*", "", targets)
        self.assertIn("let owner = self.search_owner.borrow();", targets)
        self.assertIn("owner.as_ref().filter(|owner| owner.pinned)", targets)
        self.assertIn(".and_then(Weak::upgrade)", targets)
        self.assertIn(".filter(|view| view.widget().is_mapped())", targets)
        self.assertRegex(targets, r"return match view \{\s*Some\(view\) => \(Some\(view\.vte\(\)\.clone\(\)\), Some\(view\)\),\s*None => \(None, None\),\s*\};")
        self.assertIn("(self.current_terminal(), self.current_term_view())", targets)
        self.assertNotRegex(targets, r"borrow_mut|search_find|search_set|select_range|select_all|clipboard|grab_focus|clear_find|search_apply")
        self.assertEqual(code.count("owner.capture_search_preview()?"), 1)
        self.assertIn("Arc::new(owner.capture_search_preview()?)", source)
        self.assertIn("owner: Rc::downgrade(&owner)", source)
        self.assertIn("root.is_realized()", source)
        self.assertIn("search_preview::reply_is_current(", source)
        self.assertIn("self.snapshot.id", source)
        self.assertIn("self.generation.get()", source)
        self.assertIn("buffer.apply_tag(tag, &start, &end)", source)
        self.assertIn("text.scroll_to_mark(&mark, 0.1, false, 0.0, 0.0)", source)
        self.assertIn("clear_preview_buffer(&text)", source)
        self.assertIn("buffer.delete_mark(&mark)", source)
        self.assertIn("text.set_editable(false)", source)
        self.assertIn("dialog.connect_closed", source)
        self.assertIn("controls_closed.worker.stop()", source)
        self.assertNotRegex(code, r"\b(?:entry|search_entry)\s*\.\s*text\s*\(")
        # The seed permits two / delimiters plus one oversize sentinel;
        # subsequent edits need only the sentinel. Byte limits still apply.
        self.assertIn(".chars(0, (search_preview::QUERY_LIMIT + 3) as i32)", opening)
        self.assertIn("original.starts_with('/') && original.ends_with('/')", source)
        self.assertIn("(&original[1..original.len() - 1], true)", source)
        self.assertIn("if query.len() <= search_preview::QUERY_LIMIT", source)
        self.assertIn("controls.submit(query, use_regex)", source)
        self.assertEqual(code.count(".chars(0, (search_preview::QUERY_LIMIT + 1) as i32)"), 2)
        self.assertIn("delegate.set_max_length((search_preview::QUERY_LIMIT + 1) as i32)", source)
        self.assertIn("entry.connect_changed", source)
        self.assertIn("regex.connect_toggled", source)
        self.assertIn("previous.connect_clicked", source)
        self.assertIn("next.connect_clicked", source)

    def test_single_worker_and_bounded_latest_request(self):
        source = (ROOT / "src/block_view/search_preview.rs").read_text()
        self.assertEqual(source.count(".spawn(move ||"), 1)
        self.assertIn("mpsc::sync_channel(1)", source)
        self.assertIn("Mutex<Option<Request>>", source)
        self.assertIn("Mutex<Option<Reply>>", source)
        self.assertIn("query.len() > QUERY_LIMIT", source)
        self.assertIn("raw_bytes > TEXT_LIMIT", source)
        self.assertIn("next > TEXT_LIMIT", source)
        self.assertIn("matches.hits.len() == HIT_LIMIT", source)
        self.assertIn(".size_limit(REGEX_LIMIT)", source)
        self.assertIn(".dfa_size_limit(REGEX_LIMIT)", source)
        self.assertIn(".split_inclusive('\\n')", source)
        self.assertIn("if snapshot.failed", source)


if __name__ == "__main__":
    unittest.main()
