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


def check_probe(env, uid, run=subprocess.run):
    validate_scope(env, uid)
    # Do not mutate the configured module, display, search path or locale.
    # Reject exit-success with fallback/multiple/empty module lines as unavailable.
    try:
        result = run(["fcitx5-gtk4-immodule-probing"], env=dict(env),
                     capture_output=True, timeout=5, check=False)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Unavailable("GTK4 Fcitx probe could not run successfully") from exc
    require(result.returncode == 0, "GTK4 Fcitx probe did not exit successfully")
    require(result.stdout == b"GTK_IM_MODULE=fcitx5\n",
            "GTK4 Fcitx probe did not report the requested single module")
    # A matching ID is not trustworthy in the presence of loader diagnostics.
    require(not result.stderr, "GTK4 Fcitx probe reported diagnostics")


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
