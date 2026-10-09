#!/usr/bin/env python3
"""Run actual find-bar controller/routing regressions with Python 3 and rustc.

The current search.rs and switch-page search arm are inserted verbatim (apart
from import adaptation) into deterministic UI/terminal boundary fixtures.
No display, Cargo cache, network, or shell process is required.
"""
from pathlib import Path
import os
import hashlib
import re
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]


def build_harness():
    source = (ROOT / "src/ui/search.rs").read_text()
    source = "\n".join(line for line in source.splitlines() if not line.startswith(("//!", "use adw::", "use gtk4::glib;", "use libadwaita", "use vte4::TerminalExt;")))
    main = (ROOT / "src/main.rs").read_text()
    marker = "if ui_for_switch.search_bar.is_search_mode() {"
    assert main.count(marker) == 1
    branch = main[main.index(marker):]
    branch = branch[:branch.index("} else if") + 1]
    fixture = (ROOT / "scripts/search_lifecycle_harness.rs").read_text()
    for marker, value in [("source", source), ("switch", branch)]:
        placeholder = f"// @search-lifecycle:{marker}"
        assert fixture.count(placeholder) == 1
        fixture = fixture.replace(placeholder, value)
    return fixture


def main():
    with tempfile.TemporaryDirectory(prefix="forge-search-lifecycle-") as directory:
        source = Path(directory) / "search_lifecycle.rs"
        binary = Path(directory) / "search_lifecycle"
        source.write_text(build_harness())
        subprocess.run([os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test", "-C", "debuginfo=0", str(source), "-o", str(binary)], check=True)
        data = binary.read_bytes()
        print(f"Fixture ELF: {len(data)} bytes, SHA256 {hashlib.sha256(data).hexdigest()}", flush=True)
        subprocess.run([str(binary), "--test-threads=1"], check=True)


if __name__ == "__main__":
    main()
