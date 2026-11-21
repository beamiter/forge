#!/usr/bin/env python3
"""Exercise matrix orchestration and redaction; no actual display or service."""

from contextlib import contextmanager
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

p = Path(__file__).with_name("qa-fcitx5-probe-matrix.py")
spec = importlib.util.spec_from_file_location("matrix", p)
matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(matrix)


class MatrixTests(unittest.TestCase):
    def test_each_factor_pair_gets_one_new_private_scope_and_no_widget_callback(self):
        inputs, output = [], []

        def run(scope, *, probe_profile, probe_pinyin_bus):
            self.assertTrue(scope.is_dir())
            self.assertEqual(scope.stat().st_mode & 0o777, 0o700)
            self.assertEqual(list(scope.iterdir()), [])
            inputs.append((scope, probe_profile, probe_pinyin_bus))
            if len(inputs) == 2:
                raise matrix.hosted.preflight.Unavailable("private diagnostic content")
            if len(inputs) == 3:
                raise OSError("private fixture path")

        matrix.run_matrix(run, lambda *a, **kw: output.extend(a))
        self.assertEqual(len(inputs), 4)
        self.assertEqual(len({scope for scope, _, _ in inputs}), 4)
        self.assertTrue(all(not scope.exists() for scope, _, _ in inputs))
        self.assertEqual([(bool(profile), policy) for _, profile, policy in inputs],
                         [(False, False), (True, False), (False, True), (True, True)])
        self.assertIn("module-matrix profile=0 pinyin_bus_policy=0 module-selected", output)
        self.assertIn("module-matrix profile=1 pinyin_bus_policy=0 probe-rejected: "
                      "probe-rejected-without-safe-detail", output)
        self.assertIn("module-matrix profile=0 pinyin_bus_policy=1 setup-unavailable", output)
        self.assertIn("module-matrix profile=1 pinyin_bus_policy=1 module-selected", output)
        self.assertNotIn("private", "\n".join(output))

    def test_module_runner_installs_only_selected_profile_before_starting_daemon(self):
        hosted = matrix.hosted
        for profile, policy in matrix.FACTORS:
            events = []

            @contextmanager
            def bus(scope, **kwargs):
                events.append(("bus", kwargs))
                yield f"unix:path={scope}/bus"

            @contextmanager
            def child(argv, env):
                events.append(argv[0])
                process = Mock(pid=4242)
                process.poll.return_value = None
                if argv[0] == "/usr/bin/fcitx5":
                    config = Path(env["XDG_CONFIG_HOME"]) / "fcitx5"
                    self.assertEqual(config.exists(), profile)
                yield process

            with self.subTest(profile=profile, policy=policy), tempfile.TemporaryDirectory() as d, patch.object(
                    hosted.owned, "private_bus", side_effect=bus), patch.object(
                    hosted.owned, "wait_for_owned_peer"), patch.object(
                    hosted, "child", side_effect=child), patch.object(
                    hosted.os.path, "lexists", return_value=False), patch.object(
                    hosted, "make_authority"), patch.object(
                    hosted, "verify_x_auth"), patch.object(
                    hosted, "verify_fcitx_owner"), patch.object(
                    hosted.preflight, "check_probe") as probe, patch.object(
                    Path, "read_text", return_value="4242\n"):
                hosted.run(Path(d), probe_profile=matrix.pinyin.install_profile if profile else None,
                           probe_pinyin_bus=policy)
                self.assertEqual(events, [("bus", {"pinyin_fixture": True} if policy else {}),
                                          "/usr/bin/Xvfb", "/usr/bin/fcitx5"])
                probe.assert_called_once()

    def test_allow_only_precisely_bounded_numeric_probe_summary(self):
        err = ("GTK4 Fcitx probe reported diagnostics (stderr_bytes=134; "
               "gtk_im_module_missing=0, gtk_session_bus=0, gtk_accessibility_bus=0, "
               "unknown=2; sample_truncated=0)")
        self.assertEqual(matrix.classify(RuntimeError(err)), err)
        self.assertEqual(matrix.classify(RuntimeError(err + "\nprivate")),
                         "probe-rejected-without-safe-detail")
        self.assertEqual(matrix.classify(RuntimeError(err.replace("unknown=2", "unknown=private"))),
                         "probe-rejected-without-safe-detail")


if __name__ == "__main__":
    unittest.main()
