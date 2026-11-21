#!/usr/bin/env python3
"""Check module selection for a caller-owned, isolated X11 Fcitx5 QA fixture.

Exit 2 means preflight unavailable, not a product failure. Success is deliberately
not evidence of a connected engine, first-window focus, or a committed character.
The caller must launch/verify its own Xvfb and D-Bus; this never starts or repairs either.
"""

import os
from pathlib import Path
import re
import stat
import subprocess
import sys


EXPECTED_MODULE = b"GTK_IM_MODULE=fcitx5\n"


class Unavailable(Exception):
    pass


def require(condition, reason):
    if not condition:
        raise Unavailable(reason)


def validate_scope(env, uid):
    require(uid != 0 and env.get("FORGE_FCITX_QA_ISOLATED") == "1",
            "requires an explicit non-root QA environment")
    require(env.get("GDK_BACKEND") == "x11"
            and re.fullmatch(r":\d+(?:\.\d+)?", env.get("DISPLAY", ""), re.ASCII)
            and not any(env.get(key) for key in ("WAYLAND_DISPLAY", "WAYLAND_SOCKET")),
            "requires a local X11-only display")
    require(env.get("GTK_IM_MODULE") == "fcitx5",
            "fixture must explicitly select the fcitx5 module")
    runtime = Path(env.get("XDG_RUNTIME_DIR", ""))
    # D-Bus addresses use delimiters and escapes; admit only an unambiguous path.
    require(re.fullmatch(r"/[a-zA-Z0-9_./-]+", str(runtime)),
            "requires an unambiguous absolute runtime directory")
    try:
        physical = runtime.resolve()
    except (OSError, RuntimeError) as exc:
        raise Unavailable("could not resolve runtime directory") from exc
    require(physical == runtime, "requires a physical runtime directory")
    authority = runtime / "Xauthority"
    bus = runtime / "bus"
    require(env.get("XAUTHORITY") == str(authority)
            and env.get("DBUS_SESSION_BUS_ADDRESS") == f"unix:path={bus}"
            and not env.get("DBUS_SYSTEM_BUS_ADDRESS"),
            "requires the fixture's own authority file and session bus")
    for path, kind, exact_mode in (
        (runtime, stat.S_ISDIR, 0o700),
        (authority, stat.S_ISREG, 0o600),
        (bus, stat.S_ISSOCK, None),
    ):
        try:
            info = path.lstat()
        except OSError as exc:
            raise Unavailable(f"missing or unreadable {path.name}") from exc
        require(info.st_uid == uid and kind(info.st_mode)
                and (exact_mode is None or stat.S_IMODE(info.st_mode) == exact_mode),
                f"bad owner, type or mode for {path.name}")


# Accept only the GTK/GLib warning framing and the GTK 4.14 source formats
# below; any other module, locale, level or message remains unknown.
_GTK_WARNING = re.compile(
    rb"^(?:\([A-Za-z0-9_.+-]{1,96}:[0-9]{1,12}\): )?"
    rb"Gtk-WARNING \*\*: (?:[0-9]{2}:[0-9]{2}:[0-9]{2}\.[0-9]{3}: )?([^\x00\r]+)$")


def stderr_shape(stderr):
    budget, lines = 8192, 64
    sample = stderr[:budget]
    truncated = len(stderr) > budget
    if truncated:
        sample = sample[:sample.rfind(b"\n") + 1]  # no partial match
    records = sample.split(b"\n", lines)
    truncated |= len(records) > lines and bool(records[-1])
    counts = dict(gtk_im_module_missing=0, gtk_session_bus=0, gtk_accessibility_bus=0,
                  egl_warning=0, mesa_loader=0, unknown=0)
    for line in records[:lines]:
        if not line:
            continue
        match = _GTK_WARNING.fullmatch(line)
        msg = match[1] if match else b""
        # gtkimmodule.c and gtk/a11y/gtkatspicontext.c, GTK 4.14.5.
        if msg == b"No IM module matching GTK_IM_MODULE=fcitx5 found":
            counts["gtk_im_module_missing"] += 1
        elif msg.startswith(b"Unable to acquire session bus: "):
            counts["gtk_session_bus"] += 1
        elif (msg.startswith(b"Unable to acquire the address of the accessibility bus: ")
              and msg.endswith(b" If you are attempting to run GTK without a11y support, "
                               b"GTK_A11Y should be set to 'none'.")):
            counts["gtk_accessibility_bus"] += 1
        # Mesa 25.2.8 src/egl/main/egllog.c and src/loader/loader.c.
        # Only this label is reported, never the message or driver path.
        elif line.startswith(b"libEGL warning: ") and len(line) > len(b"libEGL warning: "):
            counts["egl_warning"] += 1
        elif line.startswith(b"MESA-LOADER: ") and len(line) > len(b"MESA-LOADER: "):
            counts["mesa_loader"] += 1
        else:
            counts["unknown"] += 1
    return (f"stderr_bytes={len(stderr)}; "
            + ", ".join(f"{key}={value}" for key, value in counts.items())
            + f"; sample_truncated={int(truncated)}")


def check_probe(env, uid, run=subprocess.run):
    validate_scope(env, uid)
    # Do not mutate the configured module, display, search path or locale.
    # Reject exit-success with fallback/multiple/empty module lines as unavailable.
    try:
        result = run(["fcitx5-gtk4-immodule-probing"], env=dict(env),
                     capture_output=True, timeout=5, check=False)
    except subprocess.TimeoutExpired as exc:
        # Upstream flushes the ID line only after GTK init and all context
        # calls. No line cannot identify which internal stage hung.
        output = exc.stdout or b""
        state = ("expected-line" if output == EXPECTED_MODULE
                 else "unexpected-output" if output else "no-output")
        raise Unavailable(
            f"GTK4 Fcitx probe timed out after 5s (stdout={state}; "
            f"stderr_bytes={len(exc.stderr or b'')})") from None
    except FileNotFoundError:
        raise Unavailable("GTK4 Fcitx probe executable not found") from None
    except OSError as exc:
        raise Unavailable(f"GTK4 Fcitx probe launch failed (errno={exc.errno})") from None
    require(result.returncode == 0,
            f"GTK4 Fcitx probe did not exit successfully (status={result.returncode})")
    require(result.stdout == EXPECTED_MODULE,
            "GTK4 Fcitx probe did not report the requested single module")
    # A matching ID is not trustworthy in the presence of loader diagnostics.
    if result.stderr:
        # Classification never converts diagnostics into a pass.
        raise Unavailable(f"GTK4 Fcitx probe reported diagnostics ({stderr_shape(result.stderr)})")


def main():
    try:
        check_probe(os.environ, os.geteuid())
    except Unavailable as exc:
        print(f"Fcitx QA preflight unavailable: {exc}", file=sys.stderr)
        return 2
    print("GTK4 selected fcitx5; app focus, engine and commit are untested")
    return 0


if __name__ == "__main__":
    sys.exit(main())
