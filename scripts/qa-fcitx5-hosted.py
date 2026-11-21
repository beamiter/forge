#!/usr/bin/env python3
"""Run real private X11/D-Bus/Fcitx5 setup, without sending keys or launching Forge."""

from contextlib import contextmanager
from datetime import datetime, timezone
import importlib.util
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import tempfile
import time


def sibling(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


owned = sibling("qa-fcitx5-owned-bus")
preflight = sibling("qa-fcitx5-preflight")


def command(argv, env, data=None, timeout=3):
    return subprocess.run(argv, env=env, input=data, timeout=timeout,
                          capture_output=True, check=False)


@contextmanager
def child(argv, env):
    p = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        yield p
    finally:
        owned.stop_owned_child(p)


def make_authority(env, authority):
    fd = os.open(authority, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.close(fd)
    cookie = secrets.token_hex(16)
    data = f"add {env['DISPLAY']} MIT-MAGIC-COOKIE-1 {cookie}\n".encode()
    result = command(["/usr/bin/xauth", "-q", "-f", str(authority)], env, data=data)
    if result.returncode or not authority.stat().st_size:
        raise owned.Unavailable("could not provision private X authority")


def verify_x_auth(env, wrong_authority):
    # An empty private authority file has no usable cookie. Compare positive and
    # negative queries only after the socket peer is identified as our Xvfb.
    fd = os.open(wrong_authority, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    os.close(fd)
    argv = ["/usr/bin/xdpyinfo", "-display", env["DISPLAY"]]
    if command(argv, env).returncode != 0:
        raise owned.Unavailable("owned Xvfb rejected the fixture cookie")
    if command(argv, {**env, "XAUTHORITY": str(wrong_authority)}).returncode == 0:
        raise owned.Unavailable("owned Xvfb accepted an unauthenticated client")


def verify_fcitx_owner(p, env, address, timeout=5):
    deadline = time.monotonic() + timeout
    expected = re.compile(rb"\(uint32 " + str(p.pid).encode() + rb",\)\n")
    argv = ["/usr/bin/gdbus", "call", "--address", address, "--timeout", "1",
            "--dest", "org.freedesktop.DBus", "--object-path", "/org/freedesktop/DBus",
            "--method", "org.freedesktop.DBus.GetConnectionUnixProcessID", "org.fcitx.Fcitx5"]
    while time.monotonic() < deadline and p.poll() is None:
        try:
            result = command(argv, env, timeout=min(1.5, max(.001, deadline - time.monotonic())))
        except subprocess.TimeoutExpired:
            raise owned.Unavailable("owned Fcitx5 D-Bus owner query timed out") from None
        if result.returncode == 0:
            if result.stderr or not expected.fullmatch(result.stdout):
                raise owned.Unavailable("private Fcitx5 bus owner is not the launched daemon")
            if p.poll() is None:
                return
            break
        time.sleep(.05)
    raise owned.Unavailable("owned Fcitx5 did not acquire its private D-Bus name")


def run(runtime, *, pinyin_fixture=None, probe_profile=None, probe_pinyin_bus=False):
    if pinyin_fixture is not None and (probe_profile is not None or probe_pinyin_bus):
        raise ValueError("full Pinyin and diagnostic factors cannot run together")
    started = time.monotonic()

    def stage(message):
        stamp = datetime.now(timezone.utc).isoformat(timespec="milliseconds")
        print(f"{stamp} +{time.monotonic() - started:.3f}s {message}", flush=True)

    number = 200 + secrets.randbelow(800)
    x_socket = Path(f"/tmp/.X11-unix/X{number}")
    x_lock = Path(f"/tmp/.X{number}-lock")
    if os.path.lexists(x_socket) or os.path.lexists(x_lock):
        raise owned.Unavailable("random private display slot is already occupied")
    for name in ("home", "config", "cache", "data", "state"):
        (runtime / name).mkdir(mode=0o700)
    auth = runtime / "Xauthority"
    env = {"PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "HOME": str(runtime / "home"),
           "XDG_RUNTIME_DIR": str(runtime), "XDG_CONFIG_HOME": str(runtime / "config"),
           "XDG_CACHE_HOME": str(runtime / "cache"), "XDG_DATA_HOME": str(runtime / "data"),
           "XDG_STATE_HOME": str(runtime / "state"), "GSETTINGS_BACKEND": "memory",
           "XDG_SESSION_TYPE": "x11", "GTK_A11Y": "none",
           "DISPLAY": f":{number}", "XAUTHORITY": str(auth), "GDK_BACKEND": "x11",
           "GTK_IM_MODULE": "fcitx5", "FORGE_FCITX_QA_ISOLATED": "1"}
    stage("starting owned private D-Bus")
    bus = (owned.private_bus(runtime, pinyin_fixture=True)
           if pinyin_fixture is not None or probe_pinyin_bus else owned.private_bus(runtime))
    with bus as address:
        stage("owned D-Bus peer and GetId verified")
        env["DBUS_SESSION_BUS_ADDRESS"] = address
        make_authority(env, auth)
        xvfb = ["/usr/bin/Xvfb", env["DISPLAY"], "-auth", str(auth), "-nolisten", "tcp",
                "-screen", "0", "1280x800x24"]
        stage("starting owned authenticated Xvfb")
        with child(xvfb, env) as x:
            owned.wait_for_owned_peer(x, x_socket, os.geteuid())
            if x_lock.read_text().strip() != str(x.pid):
                raise owned.Unavailable("private X display lock does not match the owned Xvfb")
            verify_x_auth(env, runtime / "Xauthority-no-cookie")
            stage("owned Xvfb accepted fixture cookie and refused empty authority")
            if pinyin_fixture is not None:
                pinyin_fixture.install_profile(env)
            elif probe_profile is not None:
                probe_profile(env)
            with child(["/usr/bin/fcitx5", "--disable=cloudpinyin"], env) as fcitx:
                verify_fcitx_owner(fcitx, env, address)
                stage("owned Fcitx5 D-Bus name verified")
                preflight.check_probe(env, os.geteuid())
                if fcitx.poll() is not None:
                    raise owned.Unavailable("owned Fcitx5 exited during GTK preflight")
                stage("GTK4 selected fcitx5")
                if pinyin_fixture is not None:
                    pinyin_fixture.run(env, runtime)
    stage("owned processes reaped; terminal remains untested"
          if pinyin_fixture is not None
          else "owned processes reaped; engine and terminal remain untested")


def main():
    try:
        with tempfile.TemporaryDirectory(prefix="forge-fcitx-hosted-", dir="/tmp") as scope:
            run(Path(scope))
    except (owned.Unavailable, preflight.Unavailable, OSError, RuntimeError,
            subprocess.SubprocessError) as exc:
        print(f"Forge hosted Fcitx5 readiness unavailable: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
