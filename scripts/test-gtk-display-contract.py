#!/usr/bin/env python3
"""Exercise the actual GTK runner with fake children, never a display.

Checks shell orchestration only. A pass is NOT native GTK acceptance.
"""
import json
import os
from pathlib import Path
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time

RUNNER = Path(__file__).with_name("test-gtk-display.sh")
DIAGNOSTIC = "ui::organism::tests::live_settings_reuse_body_preserve_focus_and_stop_disabled_timers"
CHAT = "block_view::blocks::tests::chat_pair_keeps_native_surfaces_and_gap_is_not_accumulated"
A11Y = {DIAGNOSTIC, "ui::tasks_panel::tests::approval_details_are_paged_read_only_and_stale_controls_cannot_decide"}
IME = {"block_view::tests::native_ime_commit_cancel_and_independent_ptys",
       "block_view::tests::native_ime_send_button_preserves_composition"}
FAKE_CHILD = r'''
import json, os, sys, time
from pathlib import Path
name, args = Path(sys.argv[0]).name, sys.argv[1:]
e = os.environ
tests = json.loads(Path(e["CONTRACT_TESTS"]).read_text())
with open(e["CONTRACT_LOG"], "a") as f:
    f.write(json.dumps({"command": name, "args": args, "pid": os.getpid(),
        "env": {k: v for k, v in e.items() if k.startswith("XDG_") or k in
            ["GTK_A11Y", "GTK_IM_MODULE", "RUST_TEST_THREADS", "GDK_BACKEND",
             "GSK_RENDERER", "LIBGL_ALWAYS_SOFTWARE", "NO_AT_BRIDGE"]}}) + "\n")
if name == "dbus-run-session":
    assert args[0].startswith("--config-file=") and args[1] == "--", args
    os.execvp(args[2], args[2:])
elif name == "xvfb-run":
    assert args[:2] == ["--auto-servernum", "--server-args=-screen 0 1280x800x24 -nolisten tcp"], args
    os.execvp(args[2], args[2:])
elif name == "timeout":
    assert args[:3] == ["--signal=TERM", "--kill-after=10s", "240s"], args
    os.execvp(args[3], args[3:])
elif name == "cargo":
    prefix = ["test", "--lib", "--all-features", "--locked"]
    assert args[:4] == prefix, args
    if args[4:] == ["--", "--ignored", "--list"]:
        if e["CONTRACT_MODE"] == "list-failure": sys.exit(31)
        for test in tests[:-1] if e["CONTRACT_MODE"] == "missing-test" else tests:
            print(test + ": test")
    elif args[4:] == ["--no-run", "--message-format=json"]:
        print(json.dumps({"reason": "compiler-artifact", "executable": "/fake/test-binary",
                          "profile": {"test": True}, "target": {"kind": ["lib"]}}))
    else:
        assert args[5:] == ["--", "--ignored", "--exact", "--nocapture"], args
        test = args[4]
        assert test in tests
        if e["CONTRACT_MODE"].startswith("cancel-") and test == tests[0]:
            Path(e["CONTRACT_READY"]).touch()
            deadline = time.monotonic() + 10
            while not Path(e["CONTRACT_RELEASE"]).exists():
                assert time.monotonic() < deadline, "contract parent did not release child"
                time.sleep(.01)
        sys.exit(json.loads(e["CONTRACT_FAILURES"]).get(test, 0))
elif name == "gdb":
    if args == ["--version"]: print("fake gdb")
    else: sys.exit(7)
elif name in ("git", "sha256sum"):
    print("fake identity only")
else:
    raise AssertionError("Unexpected fake child " + name)
'''


def main():
    source = RUNNER.read_text()
    tests = re.search(r"^tests=\(\n(.*?)\n\)", source, re.M | re.S).group(1).split()
    assert len(tests) == len(set(tests)) and {CHAT, DIAGNOSTIC, *A11Y, *IME}.issubset(tests)
    bash = shutil.which("bash")
    with tempfile.TemporaryDirectory(prefix="forge-gtk-contract-") as directory:
        root = Path(directory)
        bindir = root / "bin"
        bindir.mkdir()
        (root / "scripts").mkdir()
        runner = root / "scripts" / RUNNER.name
        runner.write_text(source)
        policy = root / "share/dbus-1/session.conf"
        policy.parent.mkdir(parents=True)
        policy.write_text("fake policy: never starts a daemon\n")
        # Closed PATH: no real Cargo/display/debugger can be used accidentally.
        for command in ("dirname", "mktemp", "mkdir", "chmod", "rm", "grep", "tee", "cat"):
            (bindir / command).symlink_to(shutil.which(command))
        (bindir / "python3").symlink_to(sys.executable)
        fake_commands = ("cargo", "dbus-run-session", "dbus-daemon", "xvfb-run", "xdotool",
                         "timeout", "gdb", "git", "sha256sum")
        for command in fake_commands:
            child = bindir / command
            child.write_text(f"#!{sys.executable}\n" + FAKE_CHILD)
            child.chmod(0o755)
        inventory = root / "tests.json"
        inventory.write_text(json.dumps(tests))
        env = dict(os.environ)
        for key in ("BASH_ENV", "FORGE_NATIVE_CRASH_DIAGNOSTICS", "GTK_PAINT_DIAGNOSTICS_DIR"):
            env.pop(key, None)
        env.update(PATH=str(bindir), TMPDIR=str(root), GTK_IM_MODULE="inherited",
                   CONTRACT_TESTS=str(inventory), GTK_PAINT_DIAGNOSTICS_DIR=str(root / "diagnostics"))
        cases = [
            ("success", {}, 0),
            ("early-failure", {tests[0]: 17}, 17),
            ("multiple-failures", {tests[0]: 17, CHAT: 23, tests[-1]: 29}, 17),
            ("child-timeout", {tests[0]: 124}, 124),
            ("child-signal", {tests[0]: 143}, 143),
            ("diagnostic", {DIAGNOSTIC: 53}, 53),
            ("list-failure", {}, 31),
            ("missing-test", {}, 1),
            ("cancel-INT", {}, -signal.SIGINT),
            ("cancel-TERM", {}, -signal.SIGTERM),
            ("missing-wrapper", {}, 1),
        ]
        for mode, failures, status in cases:
            log = root / f"{mode}.jsonl"
            ready, release = root / f"{mode}.ready", root / f"{mode}.release"
            if mode == "missing-wrapper": (bindir / "xvfb-run").unlink()
            run_env = {**env, "CONTRACT_MODE": mode, "CONTRACT_LOG": str(log),
                       "CONTRACT_FAILURES": json.dumps(failures), "CONTRACT_READY": str(ready),
                       "CONTRACT_RELEASE": str(release),
                       "FORGE_NATIVE_CRASH_DIAGNOSTICS": "1" if mode == "diagnostic" else "0"}
            process = subprocess.Popen([bash, str(runner)], env=run_env, text=True,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
            try:
                if mode.startswith("cancel-"):
                    deadline = time.monotonic() + 10
                    while not ready.exists():
                        if process.poll() is not None or time.monotonic() >= deadline:
                            raise AssertionError("first test did not start")
                        time.sleep(.01)
                    # Terminal cancellation reaches the whole foreground group.
                    # Do not release the stalled child: cancellation itself must
                    # terminate it promptly and must not start another case.
                    os.killpg(process.pid, signal.SIGINT if mode == "cancel-INT" else signal.SIGTERM)
                stdout, stderr = process.communicate(timeout=3 if mode.startswith("cancel-") else 90)
            except BaseException:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.communicate()
                raise
            output = stdout + stderr
            assert process.returncode == status, (mode, process.returncode, output)
            records = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            calls = [r for r in records if r["command"] == "cargo"
                     and "--list" not in r["args"] and "--no-run" not in r["args"]]
            if mode.startswith("cancel-"):
                assert len(calls) == 1 and "GTK display regressions passed:" not in output, (mode, output)
            elif mode in ("list-failure", "missing-test", "missing-wrapper"):
                assert not calls and "GTK display regressions passed:" not in output, (mode, output)
            else:
                assert [r["args"][4] for r in calls] == tests, mode
                assert len({r["pid"] for r in calls}) == len(tests), mode
                for call, test in zip(calls, tests):
                    for key, value in {"GTK_A11Y": "test" if test in A11Y else "none",
                                       "GTK_IM_MODULE": "simple" if test in IME else "inherited",
                                       "RUST_TEST_THREADS": "1", "GDK_BACKEND": "x11",
                                       "GSK_RENDERER": "cairo", "LIBGL_ALWAYS_SOFTWARE": "1",
                                       "NO_AT_BRIDGE": "1"}.items():
                        assert call["env"][key] == value, (mode, test, key)
                assert f"GTK display regressions passed: {len(tests)-len(failures)}/{len(tests)}." in output, mode
                for name, code in failures.items():
                    assert f"  {name} (exit {code})" in output, (mode, output)
                if mode == "diagnostic":
                    assert "diagnostic replay exit status: 7; original status remains: 53" in output, output
                    assert sum(r["command"] == "gdb" and "--batch" in r["args"] for r in records) == 1
                if not failures:
                    assert "Failed GTK display regressions" not in output
            # Preserve the original shell's signal semantics. EXIT cleanup is
            # asserted for every ordinary result, not promised for SIGTERM.
            for record in records if not mode.startswith("cancel-") else []:
                for key in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_RUNTIME_DIR"):
                    assert not Path(record["env"][key]).exists(), (mode, "XDG cleanup", key)
            print(f"PASS: fake-command GTK runner contract: {mode}")
    print(f"11 scenarios, {len(tests)} registered cases; no native GUI execution.")


if __name__ == "__main__":
    main()
