#!/usr/bin/env python3
"""Private D-Bus setup for forthcoming X11 Fcitx QA (no GUI or IME test)."""

from contextlib import contextmanager
import os
from pathlib import Path
import re
import socket
import stat
import struct
import subprocess
import sys
import tempfile
import time


DAEMON = "/usr/bin/dbus-daemon"
CLIENT = "/usr/bin/gdbus"


class Unavailable(Exception):
    pass


def wait_for_owned_peer(process, path, uid, timeout=5.0):
    """Do not address the bus until the socket's kernel peer is our child."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise Unavailable(f"private bus exited before ready ({process.returncode})")
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                connection.settimeout(min(0.25, max(0.001, deadline - time.monotonic())))
                connection.connect(str(path))
                size = struct.calcsize("3i")
                peer_pid, peer_uid, _ = struct.unpack(
                    "3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, size))
        except (FileNotFoundError, ConnectionRefusedError, TimeoutError):
            time.sleep(0.02)
            continue
        except OSError as exc:
            raise Unavailable(f"private Unix socket unavailable (errno {exc.errno})") from exc
        if (peer_pid, peer_uid) != (process.pid, uid):
            raise Unavailable("private bus socket peer is not the owned daemon")
        if process.poll() is not None:
            raise Unavailable("private bus exited during peer verification")
        return
    raise Unavailable("private bus startup timed out")


def stop_owned_child(process):
    if process.poll() is None:
        try:
            process.terminate()
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=2)


@contextmanager
def private_bus(runtime, *, pinyin_fixture=False):
    """Yield only a verified address; callers own a fresh 0700 runtime."""
    uid = os.geteuid()
    if uid == 0:
        raise Unavailable("private bus fixture must run unprivileged")
    runtime = Path(runtime)
    if not re.fullmatch(r"/[A-Za-z0-9_./-]+", str(runtime)) or runtime.resolve() != runtime:
        raise Unavailable("requires a physical, unambiguous runtime path")
    meta = runtime.lstat()
    if not stat.S_ISDIR(meta.st_mode) or stat.S_IMODE(meta.st_mode) != 0o700 or meta.st_uid != uid:
        raise Unavailable("requires owned 0700 runtime directory")
    bus = runtime / "bus"
    if os.path.lexists(bus):
        raise Unavailable("refusing preexisting bus path")
    address = f"unix:path={bus}"
    # A custom config does not import system/session service activation dirs.
    # The socket is reachable only within the caller's own private runtime.
    pinyin_rules = ""
    if pinyin_fixture:
        # Fcitx 5.1.7 replies to the requesting connection's unique name and
        # sends these input-context signals to that same client. Do not grant
        # unsolicited replies, broadcast sends, or monitoring.
        pinyin_rules = """
            <allow send_type="method_return" send_requested_reply="true"/>
            <allow send_type="error" send_requested_reply="true"/>
"""
        for member in ("CurrentIM", "CommitString", "UpdateFormattedPreedit", "UpdateClientSideUI"):
            pinyin_rules += (f'<allow send_type="signal" send_broadcast="false" '
                             f'send_interface="org.fcitx.Fcitx.InputContext1" send_member="{member}"/>')
    cfg = runtime / "bus.conf"
    with cfg.open("x") as f:
        f.write(f"""<busconfig>
          <type>session</type><listen>{address}</listen><auth>EXTERNAL</auth>
          <policy context="default">
            <allow send_destination="org.freedesktop.DBus"/>
            <allow send_destination="org.fcitx.Fcitx5"/>
            <allow send_destination="org.freedesktop.portal.Fcitx"/>
            <!-- Custom bus receive policy defaults to deny, including the
                 bus driver's initial Hello reply and its owner-change signals. -->
            <allow receive_sender="org.freedesktop.DBus"/>
            <allow receive_sender="org.fcitx.Fcitx5"/>
            <allow receive_sender="org.freedesktop.portal.Fcitx"/>
            <allow own="org.fcitx.Fcitx5"/>
            <allow own="org.freedesktop.portal.Fcitx"/>
            {pinyin_rules}
          </policy>
        </busconfig>""")
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "HOME": str(runtime),
           "XDG_RUNTIME_DIR": str(runtime)}
    try:
        process = subprocess.Popen([DAEMON, "--nofork", "--nopidfile", f"--config-file={cfg}"],
                                   env=env, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except OSError as exc:
        raise Unavailable("private D-Bus daemon is unavailable") from exc
    try:
        wait_for_owned_peer(process, bus, uid)
        try:
            reply = subprocess.run(
                [CLIENT, "call", "--address", address, "--timeout", "2",
                 "--dest", "org.freedesktop.DBus", "--object-path", "/org/freedesktop/DBus",
                 "--method", "org.freedesktop.DBus.GetId"],
                capture_output=True, env=env, timeout=3, check=False)
        except subprocess.TimeoutExpired:
            raise Unavailable("owned private D-Bus Hello/GetId readiness timed out") from None
        if (process.poll() is not None or reply.returncode or reply.stderr
                or not re.fullmatch(rb"\('[0-9a-f]{32}',\)\n", reply.stdout)):
            raise Unavailable("private bus did not answer the exact GetId readiness query")
        yield address
    finally:
        stop_owned_child(process)


def main():
    try:
        with tempfile.TemporaryDirectory(prefix="forge-fcitx-bus-", dir="/tmp") as path:
            with private_bus(Path(path)):
                print("Owned private D-Bus ready; X11, Fcitx engine and terminal are untested")
    except (Unavailable, OSError, RuntimeError, subprocess.SubprocessError) as exc:
        print(f"Fcitx QA private bus unavailable: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
