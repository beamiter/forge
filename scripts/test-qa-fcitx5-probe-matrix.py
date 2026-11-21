#!/usr/bin/env python3
"""Exercise GL-only probe contrast and redaction; no display or engine."""

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
    def test_each_arm_gets_one_fresh_scope_same_profile_and_no_widget_callback(self):
        inputs, output = [], []

        def run(scope, *, module_probe_fixture, probe_disable_gl):
            self.assertTrue(scope.is_dir())
            self.assertEqual(scope.stat().st_mode & 0o777, 0o700)
            self.assertEqual(list(scope.iterdir()), [])
            self.assertIs(module_probe_fixture, matrix.pinyin)
            inputs.append((scope, probe_disable_gl))
            if not probe_disable_gl:
                raise matrix.hosted.preflight.Unavailable("private diagnostic content")

        matrix.run_matrix(run, lambda *a, **kw: output.extend(a))
        self.assertEqual([flag for _, flag in inputs], [False, True])
        self.assertNotEqual(inputs[0][0], inputs[1][0])
        self.assertTrue(all(not scope.exists() for scope, _ in inputs))
        self.assertIn("module-gl-contrast gl_disabled=0 probe-rejected: "
                      "probe-rejected-without-safe-detail", output)
        self.assertIn("module-gl-contrast gl_disabled=1 module-selected", output)
        self.assertNotIn("private", "\n".join(output))

    def test_only_probe_environment_changes_and_both_arms_have_pinyin_profile_and_policy(self):
        h = matrix.hosted
        for disable_gl in matrix.GL_MODES:
            daemon_envs = []

            @contextmanager
            def bus(scope, **kwargs):
                self.assertEqual(kwargs, {"pinyin_fixture": True})
                yield f"unix:path={scope}/bus"

            @contextmanager
            def child(argv, env):
                daemon_envs.append(env)
                self.assertNotIn("GDK_DEBUG", env)
                process = Mock(pid=4242)
                process.poll.return_value = None
                if argv[0] == "/usr/bin/fcitx5":
                    config = Path(env["XDG_CONFIG_HOME"]) / "fcitx5"
                    self.assertIn("DefaultIM=pinyin", original_read(config / "profile"))
                yield process

            original_read = Path.read_text
            with self.subTest(disable_gl=disable_gl), tempfile.TemporaryDirectory() as d, patch.object(
                    h.owned, "private_bus", side_effect=bus), patch.object(
                    h.owned, "wait_for_owned_peer"), patch.object(
                    h, "child", side_effect=child), patch.object(
                    h.os.path, "lexists", return_value=False), patch.object(
                    h, "make_authority"), patch.object(
                    h, "verify_x_auth"), patch.object(
                    h, "verify_fcitx_owner"), patch.object(
                    h.preflight, "check_probe") as probe, patch.object(
                    Path, "read_text", return_value="4242\n"):
                h.run(Path(d), module_probe_fixture=matrix.pinyin, probe_disable_gl=disable_gl)
                probe.assert_called_once()
                self.assertEqual(len(daemon_envs), 2)
                base = daemon_envs[0]
                self.assertEqual(daemon_envs[1], base)
                self.assertEqual(probe.call_args.args[0],
                                 {**base, "GDK_DEBUG": "gl-disable"} if disable_gl else base)

    def test_disabled_gl_is_rejected_for_non_diagnostic_paths(self):
        h = matrix.hosted
        for kwargs in ({}, {"pinyin_fixture": Mock()}):
            with self.subTest(kwargs=kwargs), self.assertRaises(ValueError):
                h.run(None, probe_disable_gl=True, **kwargs)

    def test_allow_only_precisely_bounded_numeric_probe_summary(self):
        err = ("GTK4 Fcitx probe reported diagnostics (stderr_bytes=134; "
               "gtk_im_module_missing=0, gtk_session_bus=0, gtk_accessibility_bus=0, "
               "egl_warning=2, mesa_loader=0, unknown=0; sample_truncated=0)")
        self.assertEqual(matrix.classify(RuntimeError(err)), err)
        self.assertEqual(matrix.classify(RuntimeError(err + "\nprivate")),
                         "probe-rejected-without-safe-detail")
        self.assertEqual(matrix.classify(RuntimeError(err.replace("egl_warning=2", "egl_warning=private"))),
                         "probe-rejected-without-safe-detail")


if __name__ == "__main__":
    unittest.main()
