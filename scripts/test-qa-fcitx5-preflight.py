#!/usr/bin/env python3
"""Pure mocked validation: never opens a display, bus, or engine."""

import importlib.util
from pathlib import Path
import stat
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

script = Path(__file__).with_name("qa-fcitx5-preflight.py")
spec = importlib.util.spec_from_file_location("preflight", script)
preflight = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preflight)


class PreflightTests(unittest.TestCase):
    uid = 1234
    root = "/fixture/runtime"

    def setUp(self):
        self.env = {
            "FORGE_FCITX_QA_ISOLATED": "1", "GDK_BACKEND": "x11",
            "DISPLAY": ":98", "GTK_IM_MODULE": "fcitx5",
            "XDG_RUNTIME_DIR": self.root, "XAUTHORITY": self.root + "/Xauthority",
            "DBUS_SESSION_BUS_ADDRESS": "unix:path=" + self.root + "/bus",
        }
        self.infos = {
            self.root: (self.uid, stat.S_IFDIR | 0o700),
            self.root + "/Xauthority": (self.uid, stat.S_IFREG | 0o600),
            self.root + "/bus": (self.uid, stat.S_IFSOCK | 0o777),
        }
        lstat = patch.object(Path, "lstat", autospec=True, side_effect=self.lstat)
        realpath = patch.object(Path, "resolve", autospec=True, side_effect=lambda p: p)
        lstat.start()
        realpath.start()
        self.addCleanup(lstat.stop)
        self.addCleanup(realpath.stop)
        self.run_probe = Mock(return_value=self.result())

    def lstat(self, path):
        if str(path) not in self.infos:
            raise FileNotFoundError(path)
        uid, mode = self.infos[str(path)]
        return SimpleNamespace(st_uid=uid, st_mode=mode)

    def result(self, code=0, out=b"GTK_IM_MODULE=fcitx5\n", err=b""):
        return subprocess.CompletedProcess(["probe"], code, out, err)

    def check(self, uid=None):
        preflight.check_probe(self.env, self.uid if uid is None else uid, self.run_probe)

    def test_uses_exact_same_session_and_timeout(self):
        self.check()
        self.run_probe.assert_called_once_with(
            ["fcitx5-gtk4-immodule-probing"], env=self.env, capture_output=True,
            timeout=5, check=False)

    def test_invalid_or_nonlocal_setup_never_starts_probe(self):
        for key, value in (
            ("FORGE_FCITX_QA_ISOLATED", ""), ("DISPLAY", "other-host:0"),
            ("WAYLAND_DISPLAY", "wayland-0"), ("GTK_IM_MODULE", "fcitx5:simple"),
            ("XDG_RUNTIME_DIR", "/fixture/runtime;tcp:host=bad"),
            ("XAUTHORITY", "/home/ordinary/.Xauthority"),
            ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/usual/bus"),
            ("DBUS_SYSTEM_BUS_ADDRESS", "unix:path=/system/bus"),
        ):
            with self.subTest(key=key), patch.dict(self.env, {key: value}):
                with self.assertRaises(preflight.Unavailable):
                    self.check()
        with self.assertRaises(preflight.Unavailable):
            self.check(0)
        self.run_probe.assert_not_called()

    def test_wrong_owner_modes_and_symlink_never_start_probe(self):
        for path, replacement in (
            (self.root, (self.uid, stat.S_IFDIR | 0o755)),
            (self.root + "/Xauthority", (0, stat.S_IFREG | 0o600)),
            (self.root + "/Xauthority", (self.uid, stat.S_IFLNK | 0o777)),
            (self.root + "/bus", (self.uid, stat.S_IFREG | 0o600)),
        ):
            with self.subTest(path=path, replacement=replacement), patch.dict(self.infos, {path: replacement}):
                with self.assertRaises(preflight.Unavailable):
                    self.check()
        with patch.dict(self.infos):
            del self.infos[self.root + "/bus"]
            with self.assertRaisesRegex(preflight.Unavailable, "missing or unreadable bus"):
                self.check()
        self.run_probe.assert_not_called()

    def test_success_exit_does_not_admit_fallback_or_ambiguous_output(self):
        for output in (b"", b"GTK_IM_MODULE=gtk-im-context-simple\n",
                       b"GTK_IM_MODULE=fcitx\n", b"GTK_IM_MODULE=ibus\n",
                       b"GTK_IM_MODULE=fcitx5\nGTK_IM_MODULE=fcitx5\n"):
            with self.subTest(output=output):
                self.run_probe.return_value = self.result(out=output)
                with self.assertRaisesRegex(preflight.Unavailable, "single module"):
                    self.check()

    def test_matching_module_still_rejects_failed_exit_and_loader_diagnostics(self):
        for result in (self.result(code=7), self.result(err=b"loader failure")):
            self.run_probe.return_value = result
            with self.assertRaises(preflight.Unavailable):
                self.check()

    def test_known_warning_still_fails_and_does_not_log_contents(self):
        err = (b"\n(fcitx5-gtk4-immodule-probing:321): Gtk-WARNING **: 11:01:49.011: "
               b"Unable to acquire session bus: /private-path\n"
               b"Gtk-WARNING **: No IM module matching GTK_IM_MODULE=fcitx5 found\n"
               b"Gtk-WARNING **: Unable to acquire the address of the accessibility bus: hidden. "
               b"If you are attempting to run GTK without a11y support, GTK_A11Y should be set to 'none'.\n"
               b"GLib-WARNING **: Unable to acquire session bus: /another-private-path\n"
               b"Gtk-WARNING **: Unable to acquire the address of the accessibility bus: fake suffix\n")
        self.run_probe.return_value = self.result(err=err)
        with self.assertRaisesRegex(preflight.Unavailable, "reported diagnostics") as error:
            self.check()
        self.assertEqual(str(error.exception),
                         f"GTK4 Fcitx probe reported diagnostics (stderr_bytes={len(err)}; "
                         "gtk_im_module_missing=1, gtk_session_bus=1, gtk_accessibility_bus=1, "
                         "unknown=2; sample_truncated=0)")

    def test_bounded_diagnostics_unknown_and_truncation(self):
        sample = b"\x00\n" * 80
        summary = preflight.stderr_shape(sample)
        self.assertIn("unknown=64; sample_truncated=1", summary)
        giant_line = b"Gtk-WARNING **: Unable to acquire session bus: " + b"x" * 8192
        summary = preflight.stderr_shape(giant_line)
        self.assertIn("gtk_session_bus=0", summary)
        self.assertIn("sample_truncated=1", summary)

    def test_launch_failure_is_not_reported_as_probe_timeout(self):
        for failure, reason in ((FileNotFoundError("private-path"), "executable not found"),
                                (PermissionError(13, "private-path"), "launch failed .*13")):
            with self.subTest(failure=failure):
                self.run_probe.side_effect = failure
                with self.assertRaisesRegex(preflight.Unavailable, reason) as error:
                    self.check()
                self.assertNotIn("private-path", str(error.exception))

    def test_probe_timeout_is_bounded_and_reports_only_output_shape(self):
        for output, state in (
            (None, "no-output"),
            (preflight.EXPECTED_MODULE, "expected-line"),
            (b"unexpected-private-text", "unexpected-output"),
        ):
            with self.subTest(output=output):
                self.run_probe.side_effect = subprocess.TimeoutExpired(
                    ["probe", "/private-path"], 5, output=output, stderr=b"private-error")
                with self.assertRaisesRegex(
                        preflight.Unavailable,
                        rf"timed out after 5s .*stdout={state}; stderr_bytes=13") as error:
                    self.check()
                self.assertNotIn("private", str(error.exception))
                self.assertEqual(self.run_probe.call_args.kwargs["timeout"], 5)


if __name__ == "__main__":
    unittest.main()
