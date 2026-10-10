#!/usr/bin/env python3
"""Run actual pane navigation/zoom source against deterministic tree boundaries."""
from pathlib import Path
import hashlib
import os
import re
import subprocess
import tempfile
import textwrap

ROOT = Path(__file__).resolve().parents[1]


def item(source, marker):
    assert source.count(marker) == 1, marker
    start = source.index(marker)
    line_start = source.rfind("\n", 0, start) + 1
    indent = source[line_start:start]
    opening = source.index("{", start)
    closing = re.search(r"^" + re.escape(indent) + r"}[ \t]*$", source[opening:], re.M)
    assert closing
    return textwrap.dedent(source[line_start:opening + closing.end()])


def build_harness():
    panes = (ROOT / "src/ui/panes.rs").read_text()
    tree = (ROOT / "src/ui/pane_tree_edit.rs").read_text()
    fixture = (ROOT / "scripts/pane_lifecycle_harness.rs").read_text()
    for marker, value in [
        ("cycle", item(panes, "pub(crate) fn cycle_pane_focus(")),
        ("swap", item(tree, "pub(crate) struct ZoomPageSwap")),
        ("restore", item(tree, "pub(crate) fn restore_zoomed_leaf(")),
        ("restore_preserving", item(tree, "pub(crate) fn restore_zoomed_leaf_preserving_selection(")),
        ("restore_with_selection", item(tree, "fn restore_zoomed_leaf_with_selection(")),
        ("selection", item(tree, "fn selection_after_replacement<")),
        ("apply_selection", item(tree, "fn restore_selection_after_replacement(")),
    ]:
        placeholder = f"// @pane-lifecycle:{marker}\n"
        assert fixture.count(placeholder) == 1
        fixture = fixture.replace(placeholder, value + "\n")
    return fixture


def main():
    with tempfile.TemporaryDirectory(prefix="forge-pane-lifecycle-") as directory:
        source = Path(directory) / "pane_lifecycle.rs"
        binary = Path(directory) / "pane_lifecycle"
        source.write_text(build_harness())
        subprocess.run([os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test", "-C", "debuginfo=0", str(source), "-o", str(binary)], check=True)
        data = binary.read_bytes()
        print(f"Fixture ELF: {len(data)} bytes, SHA256 {hashlib.sha256(data).hexdigest()}", flush=True)
        subprocess.run([str(binary), "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
