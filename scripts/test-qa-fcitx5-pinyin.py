#!/usr/bin/env python3
"""Pure policy, focus and event-contract checks; no X11, bus or IM engine."""

from contextlib import contextmanager, redirect_stdout
import importlib.util
from io import StringIO
from pathlib import Path
import tempfile
import subprocess
import unittest
from unittest.mock import MagicMock
from unittest.mock import Mock, patch
import xml.etree.ElementTree as ET

p = Path(__file__).with_name("qa-fcitx5-pinyin.py")
spec = importlib.util.spec_from_file_location("pinyin", p)
pinyin = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pinyin)


class PinyinTests(unittest.TestCase):
    def test_preedit_and_one_exact_commit_are_distinct(self):
        base = ["TEXT EMPTY", "FOCUS ON", "PREEDIT ACTIVE"]
        self.assertEqual(pinyin.evidence(base), (True, True, False))
        self.assertEqual(pinyin.evidence(base + ["TEXT EXACT", "PREEDIT CLEAR"]),
                         (True, False, True))
        self.assertEqual(pinyin.evidence(base + ["TEXT EXACT", "FOCUS OFF"]),
                         (False, True, True))

    def test_ascii_leak_or_duplicate_commit_rejected(self):
        for tail in (["TEXT OTHER"], ["TEXT EXACT", "TEXT EXACT"],
                     ["TEXT EXACT", "TEXT EMPTY"], ["TEXT EMPTY"]):
            with self.subTest(tail=tail), self.assertRaises(pinyin.Unavailable):
                pinyin.evidence(["TEXT EMPTY", *tail])

    def test_witness_path_is_bounded_and_rejects_arbitrary_output(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "events"
            p.write_bytes(b"TEXT EMPTY\nFOCUS")
            self.assertEqual(pinyin.observe(p), ["TEXT EMPTY"])
            for data in (b"\xff\n", b"FAKE\n", b"X" * 8193):
                p.write_bytes(data)
                with self.assertRaises(pinyin.Unavailable):
                    pinyin.observe(p)

    def test_focused_context_requires_exact_fixture_program_and_one_context(self):
        ident = "ab" * 16
        active = f"  IC [{ident}] program:forge-fcitx5-gtk-text-fixture frontend:dbus cap:12 focus:1\n"

        def wire(record):
            return repr((record,)).encode("ascii")
        self.assertEqual(pinyin.parse_focused_context(wire(active)), ident)
        for line in (active.replace("focus:1", "focus:0"),
                     active.replace("frontend:dbus", "frontend:xim"),
                     active.replace("gtk-text-fixture", "gtk-text-other")):
            self.assertIsNone(pinyin.parse_focused_context(wire(line)))
        for bad in (wire(active * 2), b"x" * 16385, b"bad"):
            with self.assertRaises(pinyin.Unavailable):
                pinyin.parse_focused_context(bad)

    def test_lost_focus_does_not_send_key_and_no_focus_repair(self):
        p = Mock()
        env = {}
        with patch.object(pinyin, "active_window", return_value=False), patch.object(
                pinyin, "xdotool") as x:
            with self.assertRaisesRegex(pinyin.Unavailable, "lost X11 focus"):
                pinyin.key(p, env, b"41", 1)
            x.assert_not_called()
        with patch.object(pinyin, "active_window", return_value=True), patch.object(
                pinyin, "xdotool", return_value=Mock(returncode=0)) as x:
            pinyin.key(p, env, b"41", 0)
            x.assert_called_once_with(env, "key", "ctrl+space")

    def test_only_exact_diagnostic_cli_flag_can_select_experiment(self):
        for args, want in (([], False), (["--diagnostic-gl-disabled"], True)):
            with self.subTest(args=args), patch.object(Path, "is_file", return_value=True), patch.object(
                    pinyin.os, "access", return_value=True), patch.object(pinyin.hosted, "run") as run:
                self.assertEqual(pinyin.main(args), 0)
                self.assertIs(run.call_args.kwargs["pinyin_gl_diagnostic"], want)
                self.assertIsInstance(run.call_args.kwargs["pinyin_fixture"], pinyin.Pinyin)
        with patch.object(pinyin.hosted, "run") as run:
            self.assertEqual(pinyin.main(["--diagnostic-gl-disabled", "extra"]), 2)
            run.assert_not_called()

    def test_witness_rules_and_key_order_are_the_same_with_gl_disabled(self):
        for disabled, leak in ((False, False), (True, False), (True, True)):
            actions = []
            w = Mock()
            w.poll.return_value = None

            @contextmanager
            def editor(binary, env, witness, *, window_witness):
                self.assertIs(window_witness, disabled)
                self.assertEqual(env, {"GDK_DEBUG": "gl-disable"} if disabled else {})
                witness.write_text("TEXT EMPTY\nFOCUS ON\n")
                yield w

            def press(p, env, target, index):
                self.assertEqual(env, {})
                actions.append(index)
                if index == 5:
                    with (scope / "gtk-text.events").open("a") as out:
                        out.write(("TEXT OTHER\n" if leak else "PREEDIT ACTIVE\n"))
                if index == 6:
                    with (scope / "gtk-text.events").open("a") as out:
                        out.write("TEXT EXACT\nPREEDIT CLEAR\n")

            def command(args, env):
                return Mock(returncode=0, stdout=b"pinyin\n" if args[-1] == "-n" else b"2\n")

            with self.subTest(disabled=disabled, leak=leak), tempfile.TemporaryDirectory() as d, patch.object(
                    pinyin.hosted, "child", return_value=MagicMock()), patch.object(
                    pinyin, "window_manager_ready", return_value=True), patch.object(
                    pinyin, "editor", side_effect=editor), patch.object(
                    pinyin, "find_window", return_value=b"41"), patch.object(
                    pinyin, "active_window", return_value=True), patch.object(
                    pinyin, "focused_context", return_value="ab" * 16), patch.object(
                    pinyin, "key", side_effect=press), patch.object(
                    pinyin.hosted, "command", side_effect=command), patch.object(pinyin.time, "sleep"):
                scope = Path(d)
                fixture = pinyin.Pinyin(Path("/synthetic-binary"))
                if leak:
                    with self.assertRaisesRegex(pinyin.Unavailable, "unexpected or duplicate"):
                        fixture.run({}, scope, disable_gl=disabled)
                    self.assertEqual(actions, list(range(6)))
                else:
                    fixture.run({}, scope, disable_gl=disabled)
                    self.assertEqual(actions, list(range(7)))

    def test_lifecycle_witness_is_opt_in_and_strict(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "events"
            path.write_text("WINDOW GTK-READY\nWINDOW REALIZED 41\nTEXT EMPTY\n"
                            "WINDOW MAPPED\nWINDOW PRESENT-RETURNED\n")
            with self.assertRaises(pinyin.Unavailable):
                pinyin.observe(path)
            self.assertEqual(pinyin.observe(path, True), ["TEXT EMPTY"])
            with path.open("a") as out:
                out.write("WINDOW REALIZED /private/not-a-number\n")
            with self.assertRaises(pinyin.Unavailable):
                pinyin.observe(path, True)

    def test_window_failure_is_observed_without_fallback_or_keys(self):
        w = Mock(pid=101)
        w.poll.return_value = None

        for diagnostic in (False, True):
            for visible in (False, True):
                @contextmanager
                def editor(binary, env, witness, *, window_witness):
                    self.assertIs(window_witness, diagnostic)
                    witness.write_text("WINDOW GTK-READY\nWINDOW REALIZED 41\nWINDOW MAPPED\n"
                                       "WINDOW PRESENT-RETURNED\n" if diagnostic else "")
                    yield w

                def command(argv, env, timeout):
                    self.assertEqual(env, {"LC_ALL": "C"})
                    self.assertEqual(timeout, 1)
                    self.assertEqual(argv[1:3], ["-id", "0x29"])
                    if argv[0] == "/usr/bin/xwininfo":
                        return Mock(returncode=0, stdout=(b"  Map State: IsViewable\n" if visible
                                                          else b"  Map State: IsUnviewable\n"))
                    self.assertEqual(argv[0], "/usr/bin/xprop")
                    if not visible:
                        raise subprocess.TimeoutExpired(argv, timeout)
                    return Mock(returncode=0, stdout=b"_NET_WM_PID(CARDINAL) = 102\n"
                                b'_NET_WM_NAME(UTF8_STRING) = "Forge Fcitx5 Pinyin QA"\n'
                                b'WM_NAME(STRING) = "not the synthetic title /private/value"\n')

                clock = iter((0, 1, 2, 3, 7))
                out = StringIO()
                with (self.subTest(diagnostic=diagnostic, visible=visible),
                      tempfile.TemporaryDirectory() as d,
                      patch.object(pinyin.hosted, "child", return_value=MagicMock()),
                      patch.object(pinyin, "window_manager_ready", return_value=True)):
                    with patch.object(pinyin, "editor", side_effect=editor), patch.object(
                            pinyin, "xdotool", return_value=Mock(returncode=1)) as x, patch.object(
                            pinyin.hosted, "command", side_effect=command) as query, patch.object(
                            pinyin, "key") as key, patch.object(pinyin.time, "sleep"), patch.object(
                            pinyin.time, "monotonic", side_effect=lambda: next(clock)), redirect_stdout(out):
                        env = {"LC_ALL": "C"}
                        with self.assertRaisesRegex(pinyin.Unavailable, "^no owned GtkText window$"):
                            pinyin.Pinyin(Path("/synthetic")).run(env, Path(d), disable_gl=diagnostic)
                        x.assert_called_once_with(env, "search", "--all", "--onlyvisible", "--pid", "101",
                                                  "--name", pinyin.TITLE)
                        key.assert_not_called()
                    msg = out.getvalue()
                    self.assertEqual(query.call_count, 2 if diagnostic else 0)
                    if diagnostic:
                        self.assertIn("gtk_ready=1 realized=1 widget_map=mapped present_returned=1", msg)
                        if visible:
                            self.assertIn("x_view=viewable x_pid=mismatch x_net_name=match x_legacy_name=mismatch", msg)
                        else:
                            self.assertIn("x_view=unviewable x_pid=unavailable", msg)
                        self.assertNotIn("/private", msg)
                        self.assertNotIn("0x29", msg)
                    else:
                        self.assertEqual(msg, "")

    def test_window_diagnostic_without_single_bounded_xid_never_queries_display(self):
        w = Mock()
        w.poll.return_value = 1
        cases = (("WINDOW GTK-READY\n", "realized=0"),
                 ("WINDOW REALIZED NO-XID\n", "realized=1"),
                 ("WINDOW REALIZED 4294967296\n", "realized=1"),
                 ("WINDOW REALIZED 41\nWINDOW REALIZED 42\n", "realized=1"),
                 ("WINDOW REALIZED /private/value\n", "witness=invalid"))
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "events"
            for data, evidence in cases:
                with self.subTest(data=data), patch.object(pinyin.hosted, "command") as command:
                    out = StringIO()
                    path.write_text(data)
                    with redirect_stdout(out):
                        pinyin.diagnose_window(w, {}, path)
                    self.assertIn("process=exited", out.getvalue())
                    self.assertIn("surface_xid=unavailable", out.getvalue())
                    self.assertIn(evidence, out.getvalue())
                    self.assertNotIn("/private", out.getvalue())
                    command.assert_not_called()

    def test_opt_in_policy_only_adds_requested_replies_and_four_direct_signals(self):
        bus = pinyin.hosted.owned
        policies = []
        for enabled in (False, True):
            with tempfile.TemporaryDirectory() as d:
                with patch.object(bus.subprocess, "Popen", side_effect=OSError):
                    with self.assertRaises(bus.Unavailable):
                        with bus.private_bus(Path(d), pinyin_fixture=enabled):
                            self.fail("fake launch must fail")
                cfg = ET.parse(Path(d) / "bus.conf")
                policies.append([rule.attrib for rule in cfg.findall("policy/allow")])
        base, expanded = policies
        self.assertEqual(expanded[:len(base)], base)
        self.assertCountEqual(expanded[len(base):], [
            {"send_type": t, "send_requested_reply": "true"} for t in ("error", "method_return")
        ] + [
            {"send_type": "signal", "send_broadcast": "false",
             "send_interface": "org.fcitx.Fcitx.InputContext1", "send_member": m}
            for m in ("CurrentIM", "CommitString", "UpdateFormattedPreedit", "UpdateClientSideUI")
        ])


if __name__ == "__main__":
    unittest.main()
