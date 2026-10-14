#!/usr/bin/env python3
"""Run actual find-bar controller/routing regressions with Python 3 and rustc.

The current search.rs, switch-page focus helpers and their production invocations
are inserted verbatim (apart from import adaptation) into deterministic
UI/terminal boundary fixtures. Extraction guards reject nested/relocated wiring.
No display, Cargo cache, network, or shell process is required.
"""
from pathlib import Path
import os
import hashlib
import re
import subprocess
import tempfile
import textwrap

ROOT = Path(__file__).resolve().parents[1]


def braced_item(source, signature):
    """Extract one rustfmt-formatted item without rewriting its implementation."""
    if source.count(signature) != 1:
        raise ValueError(f"expected one production item: {signature}")
    start = source.index(signature)
    line_start = source.rfind("\n", 0, start) + 1
    indentation = source[line_start:start]
    if indentation.strip():
        raise ValueError(f"production item must begin a source line: {signature}")
    opening = source.index("{", start)
    closing = re.search(r"^" + re.escape(indentation) + r"}[ \t]*$", source[opening:], re.M)
    if closing is None:
        raise ValueError(f"cannot find production item end: {signature}")
    return textwrap.dedent(source[line_start:opening + closing.end()])


def switch_callback(source):
    registrations = list(re.finditer(
        r"^(?P<indent>[ \t]*)notebook\.connect_switch_page\("
        r"move \|_, widget, page_num\| \{",
        source,
        re.M,
    ))
    if len(registrations) != 1:
        raise ValueError("expected one production switch-page callback")
    registration = registrations[0]
    registration_end = re.search(
        r"^" + re.escape(registration.group("indent")) + r"\}\);[ \t]*$",
        source[registration.end():],
        re.M,
    )
    if registration_end is None:
        raise ValueError("cannot find production switch-page callback end")
    return (
        registration,
        registration.end() + registration_end.start(),
        registration.end() + registration_end.end(),
    )


def focus_invocation(source):
    """Copy the actual callback arguments, including explicit destination routing."""
    registration, closing_start, _ = switch_callback(source)
    calls = list(re.finditer(r"^(?P<indent>[ \t]*)focus_switched_page\(", source, re.M))
    if len(calls) != 1:
        raise ValueError("expected one production focus_switched_page invocation")
    call = calls[0]
    if call.group("indent") != registration.group("indent") + "    ":
        raise ValueError("production focus invocation must be directly in switch-page callback")
    closing = re.search(
        r"^" + re.escape(call.group("indent")) + r"\);[ \t]*$",
        source[call.end():],
        re.M,
    )
    if closing is None:
        raise ValueError("cannot find production focus_switched_page invocation end")
    call_end = call.end() + closing.end()
    if not registration.end() <= call.start() < call_end <= closing_start:
        raise ValueError("production focus invocation escaped its switch-page callback")
    return textwrap.dedent(source[call.start():call_end])


def after_invocation(source):
    registration, _, callback_end = switch_callback(source)
    calls = list(re.finditer(
        r"^(?P<indent>[ \t]*)connect_tab_search_focus_after_switch\([^;]+;",
        source,
        re.M,
    ))
    if len(calls) != 1:
        raise ValueError("expected one production after-search focus registration")
    call = calls[0]
    if call.group("indent") != registration.group("indent"):
        raise ValueError("after-search focus registration must be at the callback's scope")
    if call.start() < callback_end or source[callback_end:call.start()].strip():
        raise ValueError("after-search focus registration must immediately follow switch-page callback")
    return textwrap.dedent(call.group())


def check_extraction_guards(source):
    """Keep a moved or conditional production call from becoming unconditional here."""
    registration, closing_start, _ = switch_callback(source)
    indent = registration.group("indent")
    focus = textwrap.indent(focus_invocation(source), indent + "    ")
    after = textwrap.indent(after_invocation(source), indent)
    conditional_focus = (
        indent + "    if false {\n"
        + textwrap.indent(focus, "    ") + "\n"
        + indent + "    }"
    )
    conditional_after = (
        indent + "if false {\n"
        + textwrap.indent(after, "    ") + "\n"
        + indent + "}"
    )
    without_after = source.replace(after, "", 1)
    # Move the after connector inside the original callback instead of after it.
    nested_after = (
        without_after[:closing_start] + textwrap.indent(after, "    ") + "\n"
        + without_after[closing_start:]
    )
    checks = [
        (focus_invocation, source.replace(focus, conditional_focus, 1), "conditional focus"),
        (after_invocation, source.replace(after, conditional_after, 1), "conditional registration"),
        (after_invocation, nested_after, "registration inside switch callback"),
        (after_invocation, without_after + "\nfn unused() {\n" + after + "\n}\n", "relocated registration"),
    ]
    for extract, mutated, label in checks:
        try:
            extract(mutated)
        except ValueError:
            continue
        raise AssertionError(f"extraction guard accepted {label}")


def build_harness():
    source = (ROOT / "src/ui/search.rs").read_text()
    source = "\n".join(line for line in source.splitlines() if not line.startswith(("//!", "use adw::", "use gtk4::glib;", "use libadwaita", "use vte4::TerminalExt;")))
    main = (ROOT / "src/main.rs").read_text()
    check_extraction_guards(main)
    fixture = (ROOT / "scripts/search_lifecycle_harness.rs").read_text()
    for marker, value in [
        ("source", source),
        ("focus-helper", braced_item(main, "pub(crate) fn focus_switched_page(")),
        ("after-helper", braced_item(main, "pub(crate) fn connect_tab_search_focus_after_switch(")),
        ("after-registration", after_invocation(main)),
        ("switch", focus_invocation(main)),
    ]:
        placeholder = f"// @search-lifecycle:{marker}"
        if fixture.count(placeholder) != 1:
            raise ValueError(f"expected one fixture marker: {placeholder}")
        fixture = fixture.replace(placeholder, value, 1)
    if "// @search-lifecycle:" in fixture:
        raise ValueError("unexpanded production marker in search lifecycle fixture")
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
