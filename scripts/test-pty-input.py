#!/usr/bin/env python3
"""Test extracted production PTY admission and the pinned core filter on Linux.

Requires rustc and fetched pinned jterm_core/jagent checkouts (cargo fetch,
or set JTERM_CORE_SOURCE and JAGENT_SOURCE). No GTK build or replacement input filter is used.
FORGE_INPUT_SOURCE can point at another Forge tree for baseline comparisons.
"""
from pathlib import Path
import os
import re
import subprocess
import sys
import tempfile
import textwrap
import tty

ROOT = Path(os.environ.get("FORGE_INPUT_SOURCE", Path(__file__).resolve().parents[1]))


def item(source, signature):
    start = source.index(signature.strip())
    line = source.rfind("\n", 0, start) + 1
    indent = source[line:start]
    opening = source.index("{", start)
    end = re.search(r"^" + re.escape(indent) + r"}[ \t]*$", source[opening:], re.M)
    assert end, signature
    return textwrap.dedent(source[line:opening + end.end()])


def checkout(name, variable, cache=None):
    manifest = (ROOT / "Cargo.toml").read_text()
    revision = re.search(rf'{name} = .*rev = "([a-f0-9]+)"', manifest).group(1)
    explicit = os.environ.get(variable)
    home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    candidates = [Path(explicit)] if explicit else list(
        (cache or home / "git/checkouts").glob(f"{name}-*/*")
    )
    for candidate in candidates:
        result = subprocess.run(["git", "-C", str(candidate), "rev-parse", "HEAD"],
                                text=True, capture_output=True)
        if result.returncode == 0 and result.stdout.strip() == revision:
            return candidate
    sys.exit(f"Fetch pinned {name} {revision} or set {variable} to its checkout")


if sys.platform != "linux":
    sys.exit("This native PTY harness requires Linux")
core = checkout("jterm_core", "JTERM_CORE_SOURCE")
jagent = checkout("jagent", "JAGENT_SOURCE", core.parent.parent)
pty = (ROOT / "src/pty.rs").read_text()
source = Path(__file__).with_name("pty_input_harness.rs").read_text()
for name in ("PTY_INPUT_QUEUE_CAPACITY", "MAX_PTY_INPUT_MESSAGE_BYTES"):
    declaration = re.search(rf"(?:pub )?const {name}:.*;", pty).group(0)
    source = source.replace(f"/* {name} */", declaration)
source = source.replace("/* FUNCTIONS */", "\n".join(item(pty, signature) for signature in [
    "fn boundary_paste_policy(", "fn try_enqueue_input(",
    "fn filter_and_enqueue_input(", "fn filter_and_enqueue_input_with<T>(",
    "fn input_guard_with_frame(", "fn write_all_fd(",
]))
source = source.replace("/* TYPES */", "\n".join([
    "#[derive(Clone, Copy, Debug, PartialEq, Eq)]\n" + item(pty, "pub enum PtyWriteError {"),
    item(pty, "impl fmt::Display for PtyWriteError {"),
    "#[derive(Default)]\n" + item(pty, "struct InputDispatch {"),
]))
source = source.replace("/* METHODS */", "\n".join(item(pty, signature) for signature in [
    "    pub fn write_bytes(", "    fn write_bytes_with<T>(",
    "    pub fn shell_bracketed_paste(",
]))
# Include exactly the production core modules. Do not model their behavior.
source = source.replace("/* CORE */", "\n".join(
    f'#[path = "{core / "src" / (module + ".rs")}"] mod {module};'
    for module in ("pty_input", "review_input")
))
source = source.replace("/* JAGENT */", item(
    (jagent / "src/safety.rs").read_text(), "pub fn is_unsafe_invisible_char("))
master, slave = os.openpty()
try:
    tty.setraw(slave)
    with tempfile.TemporaryDirectory(prefix="forge-pty-input-") as temp:
        src = Path(temp) / "test.rs"
        src.write_text(source)
        binary = Path(temp) / "test"
        subprocess.run([os.environ.get("RUSTC", "rustc"), "--edition=2021", "--test",
                        str(src), "-o", str(binary)], check=True)
        env = dict(os.environ, FORGE_TEST_MASTER=str(master), FORGE_TEST_SLAVE=str(slave))
        subprocess.run([str(binary), "forge_input::", "--test-threads=1", "--nocapture"],
                       env=env, pass_fds=(master, slave), check=True, timeout=30)
finally:
    os.close(master)
    os.close(slave)
