#!/usr/bin/env python3
"""Run actual tab-range selection source against deterministic tree boundaries."""
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
    source = (ROOT / "src/ui/tab_strip.rs").read_text()
    fixture = (ROOT / "scripts/tab_selection_harness.rs").read_text()
    for marker, value in [
        ("clear", item(source, "pub(crate) fn clear_tab_selection(")),
        ("select", item(source, "pub(crate) fn select_tab_range(")),
    ]:
        placeholder = f"// @tab-selection:{marker}"
        assert fixture.count(placeholder) == 1
        fixture = fixture.replace(placeholder, value)
    return fixture


def main():
    with tempfile.TemporaryDirectory(prefix="forge-tab-selection-") as directory:
        source = Path(directory) / "tab_selection.rs"
        binary = Path(directory) / "tab_selection"
        source.write_text(build_harness())
        subprocess.run([os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test", "-C", "debuginfo=0", str(source), "-o", str(binary)], check=True)
        data = binary.read_bytes()
        print(f"Fixture ELF: {len(data)} bytes, SHA256 {hashlib.sha256(data).hexdigest()}", flush=True)
        subprocess.run([str(binary), "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
