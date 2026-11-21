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


WINDOW_EVENT = re.compile(r"WINDOW (GTK-READY|REALIZED (NO-XID|[1-9][0-9]{0,9})|"
                          r"MAPPED|UNMAPPED|PRESENT-RETURNED)")


def read_witness(path):
    with path.open("rb") as f:
        raw = f.read(8193)
    if len(raw) > 8192:
        raise Unavailable("GtkText witness exceeded the evidence budget")
    *lines, _ = raw.split(b"\n")
    try:
        values = [line.decode("ascii") for line in lines]
    except UnicodeDecodeError:
        raise Unavailable("GtkText witness protocol invalid") from None
    return values


def observe(path, window_witness=False):
    values = read_witness(path)
    if window_witness:
        values = [v for v in values if not WINDOW_EVENT.fullmatch(v)]
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


def diagnose_window(p, env, witness):
    """Bounded read-only evidence. Never provides an XID to the input gate."""
    try:
        lines = read_witness(witness)
    except (OSError, Unavailable):
        lines = []
        protocol = "unavailable"
    else:
        protocol = ("valid" if all(v in EVENTS or WINDOW_EVENT.fullmatch(v)
                                   for v in lines) else "invalid")
        if protocol != "valid":
            lines = []
    status = "running" if p.poll() is None else "exited"
    lifecycle = [v for v in lines if v in ("WINDOW MAPPED", "WINDOW UNMAPPED")]
    mapping = (lifecycle[-1].split()[-1].lower() if lifecycle else "unseen")
    xids = [v.rsplit(" ", 1)[-1] for v in lines if v.startswith("WINDOW REALIZED ")]
    xid = (xids[0] if len(xids) == 1 and xids[0] != "NO-XID"
           and int(xids[0]) <= 0xffffffff else None)
    xid_kind = "single" if xid else "unavailable"
    view = pid = name = legacy_name = "unavailable"
    if xid is not None:
        # Trust only the owned child witness and the already authenticated private X display.
        xid_arg = f"0x{int(xid):x}"

        query_env = {**env, "LC_ALL": "C"}  # fixed locale for X11 CLI output, not the GTK child

        def query(argv):
            try:
                result = hosted.command(argv, query_env, timeout=1)
            except (OSError, subprocess.SubprocessError):
                return b""
            return result.stdout if result.returncode == 0 and len(result.stdout) <= 4096 else b""

        info = query(["/usr/bin/xwininfo", "-id", xid_arg, "-stats"])
        state = re.findall(rb"^ +Map State: (IsViewable|IsUnviewable|IsUnMapped)$", info, re.MULTILINE)
        if len(state) == 1:
            view = {b"IsViewable": "viewable", b"IsUnviewable": "unviewable",
                    b"IsUnMapped": "unmapped"}[state[0]]
        props = query(["/usr/bin/xprop", "-id", xid_arg, "_NET_WM_PID", "_NET_WM_NAME", "WM_NAME"])
        owner = re.findall(rb"^_NET_WM_PID\(CARDINAL\) = ([1-9][0-9]{0,9})$", props, re.MULTILINE)
        if len(owner) == 1:
            pid = "match" if owner[0] == str(p.pid).encode("ascii") else "mismatch"
        for prop, expected in ((b"_NET_WM_NAME", b"(UTF8_STRING) = "),
                               (b"WM_NAME", b"(STRING) = ")):
            values = [v[len(prop + expected):] for v in props.splitlines()
                      if v.startswith(prop + expected)]
            match = "unavailable"
            if len(values) == 1:
                match = "match" if values[0] == b'"Forge Fcitx5 Pinyin QA"' else "mismatch"
            if prop == b"_NET_WM_NAME":
                name = match
            else:
                legacy_name = match
    print("GtkText window diagnostic:"
          f" process={status} witness={protocol} gtk_ready={int('WINDOW GTK-READY' in lines)}"
          f" realized={int(bool(xids))} widget_map={mapping}"
          f" present_returned={int('WINDOW PRESENT-RETURNED' in lines)} surface_xid={xid_kind}"
          f" x_view={view} x_pid={pid} x_net_name={name} x_legacy_name={legacy_name}", flush=True)


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
def editor(binary, env, witness, *, window_witness=False):
    with witness.open("xb") as out:
        args = [str(binary), "--window-witness"] if window_witness else [str(binary)]
        p = subprocess.Popen(args, env=env, stdin=subprocess.DEVNULL,
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
            with editor(self.binary, hosted.gtk_debug_environment(env, disable_gl), witness,
                        window_witness=disable_gl) as p:
                try:
                    target = wait(lambda: find_window(p, env), "no owned GtkText window")
                except (Unavailable, subprocess.SubprocessError):
                    if disable_gl:
                        diagnose_window(p, env, witness)
                    raise
                wait(lambda: active_window(p, env, target) and evidence(observe(witness, disable_gl))[0],
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
                    focused, preedit, committed = evidence(observe(witness, disable_gl))
                    if committed:
                        raise Unavailable("GtkText committed before candidate selection")
                    return focused and preedit
                wait(preedit_only, "real Pinyin preedit was not observed")
                if focused_context(env) != context:
                    raise Unavailable("GtkText Fcitx context changed during preedit")
                key(p, env, target, 6)

                def committed():
                    focused, preedit, exact = evidence(observe(witness, disable_gl))
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
