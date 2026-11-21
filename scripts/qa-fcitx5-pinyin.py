#!/usr/bin/env python3
"""Private real Fcitx5 + GtkText canary, not Forge terminal GUI coverage."""

import ast
from contextlib import contextmanager
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time


def load_hosted():
    p = Path(__file__).with_name("qa-fcitx5-hosted.py")
    spec = importlib.util.spec_from_file_location("hosted", p)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


hosted = load_hosted()
Unavailable = hosted.owned.Unavailable
TITLE = "^Forge Fcitx5 Pinyin QA$"
KEYS = ("ctrl+space", "n", "i", "h", "a", "o", "1")
EVENTS = {"FOCUS ON", "FOCUS OFF", "PREEDIT ACTIVE", "PREEDIT CLEAR",
          "TEXT EMPTY", "TEXT EXACT", "TEXT OTHER"}


def install_profile(env):
    config = Path(env["XDG_CONFIG_HOME"]) / "fcitx5"
    config.mkdir(mode=0o700)
    (config / "profile").write_text("""[Groups/0]
Name=QA
Default Layout=us
DefaultIM=pinyin

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[Groups/0/Items/1]
Name=pinyin
Layout=

[GroupOrder]
0=QA
""")
    (config / "config").write_text("[Hotkey/TriggerKeys]\n0=Control+space\n")


def observe(path):
    with path.open("rb") as f:
        raw = f.read(8193)
    if len(raw) > 8192:
        raise Unavailable("GtkText witness exceeded the evidence budget")
    *lines, _ = raw.split(b"\n")
    try:
        values = [line.decode("ascii") for line in lines]
    except UnicodeDecodeError:
        raise Unavailable("GtkText witness protocol invalid") from None
    if any(value not in EVENTS for value in values):
        raise Unavailable("GtkText witness protocol invalid")
    return values


def evidence(values):
    text = [x for x in values if x.startswith("TEXT ")]
    if text not in (["TEXT EMPTY"], ["TEXT EMPTY", "TEXT EXACT"]):
        raise Unavailable("GtkText retained unexpected or duplicate committed text")
    focus = [x for x in values if x.startswith("FOCUS ")]
    preedit = [x for x in values if x.startswith("PREEDIT ")]
    return bool(focus and focus[-1] == "FOCUS ON"), preedit[-1:] == ["PREEDIT ACTIVE"], len(text) == 2


def wait(predicate, description, seconds=4):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.025)
    raise Unavailable(description)


def xdotool(env, *arguments):
    return hosted.command(["/usr/bin/xdotool", *arguments], env, timeout=2)


def find_window(p, env):
    if p.poll() is not None:
        raise Unavailable("GtkText fixture exited")
    result = xdotool(env, "search", "--all", "--onlyvisible", "--pid", str(p.pid), "--name", TITLE)
    if result.returncode:
        return None
    ids = result.stdout.splitlines()
    if len(ids) != 1 or not re.fullmatch(rb"[1-9][0-9]*", ids[0]):
        raise Unavailable("GtkText fixture window identity ambiguous")
    return ids[0]


def window_manager_ready(p, env):
    if p.poll() is not None:
        raise Unavailable("private window manager exited")
    result = hosted.command(["/usr/bin/xprop", "-root", "_NET_SUPPORTING_WM_CHECK"], env)
    return (result.returncode == 0 and
            re.fullmatch(rb"_NET_SUPPORTING_WM_CHECK\(WINDOW\): window id # 0x[1-9a-f][0-9a-f]*\n",
                         result.stdout))


def parse_focused_context(raw):
    if len(raw) > 16384:
        raise Unavailable("Fcitx context witness exceeded budget")
    try:
        value = ast.literal_eval(raw.decode("ascii"))
    except (ValueError, SyntaxError, UnicodeDecodeError):
        raise Unavailable("Fcitx context witness invalid") from None
    if not isinstance(value, tuple) or len(value) != 1 or not isinstance(value[0], str):
        raise Unavailable("Fcitx context witness invalid")
    ids = re.findall(r"^  IC \[([0-9a-f]{32})\] program:forge-fcitx5-gtk-text-fixture"
                     r" frontend:dbus(?: cap:[0-9a-f]+)? focus:1$", value[0], re.MULTILINE)
    if len(ids) > 1:
        raise Unavailable("multiple focused GtkText Fcitx contexts")
    return ids[0] if ids else None


def focused_context(env):
    # Controller1 DebugInfo is a read-only query on the already-owned daemon.
    result = hosted.command([
        "/usr/bin/gdbus", "call", "--address", env["DBUS_SESSION_BUS_ADDRESS"],
        "--timeout", "1", "--dest", "org.fcitx.Fcitx5", "--object-path", "/controller",
        "--method", "org.fcitx.Fcitx.Controller1.DebugInfo",
    ], env, timeout=2)
    return parse_focused_context(result.stdout) if result.returncode == 0 else None


def active_window(p, env, expected):
    result = xdotool(env, "getwindowfocus", "-f")
    return (result.returncode == 0 and result.stdout.strip() == expected
            and find_window(p, env) == expected)


def key(p, env, target, index):
    if not active_window(p, env, target):
        raise Unavailable("GtkText fixture lost X11 focus before key")
    # XTEST sent to the currently focused window; never windowfocus, windowactivate
    # or type/clipboard, and never accepts a key sequence from an external caller.
    if xdotool(env, "key", KEYS[index]).returncode:
        raise Unavailable("fixed GtkText XTEST key failed")


@contextmanager
def editor(binary, env, witness):
    with witness.open("xb") as out:
        p = subprocess.Popen([str(binary)], env=env, stdin=subprocess.DEVNULL,
                             stdout=out, stderr=subprocess.DEVNULL)
        try:
            yield p
        finally:
            hosted.owned.stop_owned_child(p)


class Pinyin:
    def __init__(self, binary):
        self.binary = binary

    install_profile = staticmethod(install_profile)

    def run(self, env, runtime, *, disable_gl=False):
        witness = runtime / "gtk-text.events"
        with hosted.child(["/usr/bin/openbox", "--sm-disable"], env) as wm:
            wait(lambda: window_manager_ready(wm, env), "private window manager did not become ready")
            with editor(self.binary, hosted.gtk_debug_environment(env, disable_gl), witness) as p:
                target = wait(lambda: find_window(p, env), "no owned GtkText window")
                wait(lambda: active_window(p, env, target) and evidence(observe(witness))[0],
                     "GtkText did not get focus naturally")
                context = wait(lambda: focused_context(env), "no focused GtkText D-Bus context")
                key(p, env, target, 0)  # configured Pinyin trigger, not a CLI switch

                def engine():
                    current = hosted.command(["/usr/bin/fcitx5-remote", "-n"], env)
                    status = hosted.command(["/usr/bin/fcitx5-remote"], env)
                    return (current.returncode == status.returncode == 0
                            and current.stdout == b"pinyin\n" and status.stdout == b"2\n")
                wait(engine, "owned active engine did not report pinyin")
                for i in range(1, 6):
                    key(p, env, target, i)

                def preedit_only():
                    focused, preedit, committed = evidence(observe(witness))
                    if committed:
                        raise Unavailable("GtkText committed before candidate selection")
                    return focused and preedit
                wait(preedit_only, "real Pinyin preedit was not observed")
                if focused_context(env) != context:
                    raise Unavailable("GtkText Fcitx context changed during preedit")
                key(p, env, target, 6)

                def committed():
                    focused, preedit, exact = evidence(observe(witness))
                    return focused and exact and not preedit
                wait(committed, "GtkText did not observe one exact UTF-8 commit")
                time.sleep(.15)
                if (not committed() or not active_window(p, env, target)
                        or focused_context(env) != context):
                    raise Unavailable("GtkText final commit/focus did not stay stable")
        lane = "GL-disabled diagnostic: " if disable_gl else ""
        print(f"{lane}Stock GTK4 GtkText accepted one private Fcitx5 Pinyin commit; terminal untested")


def main(argv=None):
    args = sys.argv[1:] if argv is None else argv
    if args not in ([], ["--diagnostic-gl-disabled"]):
        print("Expected no arguments or --diagnostic-gl-disabled", file=sys.stderr)
        return 2
    diagnostic = bool(args)
    if diagnostic:
        print("GL-disabled stock GtkText diagnostic; normal native gate outcome is unchanged", flush=True)
    try:
        # No arbitrary executable input: runner builds the fixed sibling fixture.
        binary = Path(__file__).parent.parent / "target" / "qa-fcitx5-gtk-text"
        if binary.is_symlink() or not binary.is_file() or not os.access(binary, os.X_OK):
            raise Unavailable("build the owned GtkText fixture before running this lane")
        with tempfile.TemporaryDirectory(prefix="forge-fcitx-pinyin-", dir="/tmp") as scope:
            hosted.run(Path(scope), pinyin_fixture=Pinyin(binary.resolve()),
                       pinyin_gl_diagnostic=diagnostic)
    except (Unavailable, hosted.preflight.Unavailable, OSError, RuntimeError, subprocess.SubprocessError):
        # Do not log exception strings that can include private scope addresses.
        exc = sys.exc_info()[1]
        reason = str(exc) if isinstance(exc, (Unavailable, hosted.preflight.Unavailable)) else type(exc).__name__
        print(f"Forge GTK4 Pinyin fixture unavailable: {reason}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
