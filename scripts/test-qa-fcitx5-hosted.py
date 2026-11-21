#!/usr/bin/env python3
"""Validate the hosted readiness entrypoint without a native display or bus."""

from contextlib import contextmanager
import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

path = Path(__file__).with_name("qa-fcitx5-hosted.py")
spec = importlib.util.spec_from_file_location("hosted_fcitx", path)
hosted = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hosted)


def reply(code, out=b"", err=b""):
    return subprocess.CompletedProcess([], code, out, err)


class HostedTests(unittest.TestCase):
    def test_wrong_cookie_only_queries_owned_expected_display(self):
        for negative in (1, 0):
            with tempfile.TemporaryDirectory() as d:
                auth = Path(d) / "empty"
                env = {"DISPLAY": ":777", "XAUTHORITY": f"{d}/right"}
                with patch.object(hosted, "command", side_effect=[reply(0), reply(negative)]) as command:
                    if negative:
                        hosted.verify_x_auth(env, auth)
                    else:
                        with self.assertRaisesRegex(hosted.owned.Unavailable, "unauthenticated"):
                            hosted.verify_x_auth(env, auth)
                    self.assertEqual(len(command.call_args_list), 2)
                    first, second = [c.args for c in command.call_args_list]
                    self.assertEqual(first, (["/usr/bin/xdpyinfo", "-display", ":777"], env))
                    self.assertEqual(second, (first[0], {**env, "XAUTHORITY": str(auth)}))
                    self.assertEqual(auth.read_bytes(), b"")
                    self.assertEqual(auth.stat().st_mode & 0o777, 0o600)

    def test_exact_fcitx_owner_on_private_address_is_required(self):
        p = Mock(pid=4242)
        p.poll.return_value = None
        env = {"DBUS_SESSION_BUS_ADDRESS": "unix:path=/expected/bus"}
        for fake in (reply(0, b"(uint32 4242,)\n"), reply(0, b"(uint32 1111,)\n")):
            with patch.object(hosted, "command", return_value=fake) as command:
                if b"4242" in fake.stdout:
                    hosted.verify_fcitx_owner(p, env, env["DBUS_SESSION_BUS_ADDRESS"])
                else:
                    with self.assertRaisesRegex(hosted.owned.Unavailable, "not the launched daemon"):
                        hosted.verify_fcitx_owner(p, env, env["DBUS_SESSION_BUS_ADDRESS"])
                args, sent_env = command.call_args.args
                self.assertEqual(args[2:4], ["--address", "unix:path=/expected/bus"])
                self.assertEqual(args[-1], "org.fcitx.Fcitx5")
                self.assertIs(sent_env, env)

    def test_private_owner_query_timeout_redacts_command_and_address(self):
        p = Mock(pid=4242)
        p.poll.return_value = None
        address = "unix:path=/do-not-leak/bus"
        failure = subprocess.TimeoutExpired(["gdbus", "--address", address], 1.5)
        with patch.object(hosted, "command", side_effect=failure):
            with self.assertRaisesRegex(hosted.owned.Unavailable, "owner query timed out") as error:
                hosted.verify_fcitx_owner(p, {}, address)
            self.assertNotIn(address, str(error.exception))

    def execute(self, auth_denied, diagnostic=False):
        events = []
        fixture = Mock() if diagnostic else None

        @contextmanager
        def private_bus(runtime, **kwargs):
            self.assertEqual(kwargs, {"pinyin_fixture": True} if diagnostic else {})
            events.append("bus")
            try:
                yield f"unix:path={runtime}/bus"
            finally:
                events.append("bus stopped")

        @contextmanager
        def child(argv, env):
            self.assertNotIn("DBUS_SYSTEM_BUS_ADDRESS", env)
            self.assertNotIn("WAYLAND_DISPLAY", env)
            self.assertNotIn("WAYLAND_SOCKET", env)
            self.assertNotIn("GDK_DEBUG", env)
            daemon = Mock(pid=4141)
            daemon.poll.return_value = None
            events.append(argv[0])
            try:
                yield daemon
            finally:
                events.append("stop " + argv[0])

        with tempfile.TemporaryDirectory() as d, patch.object(hosted.owned, "private_bus",
                side_effect=private_bus), patch.object(hosted, "child", side_effect=child), patch.object(
                hosted.os.path, "lexists", return_value=False), patch.object(hosted, "make_authority"), patch.object(
                hosted.owned, "wait_for_owned_peer") as peer, patch.object(
                Path, "read_text", return_value="4141\n"), patch.object(
                hosted, "verify_x_auth", side_effect=(hosted.owned.Unavailable("wrong cookie admitted")
                                                       if auth_denied else None)), patch.object(
                hosted, "verify_fcitx_owner") as owner, patch.object(hosted.preflight, "check_probe") as probe:
            if auth_denied:
                with self.assertRaisesRegex(hosted.owned.Unavailable, "wrong cookie admitted"):
                    hosted.run(Path(d))
                self.assertEqual(events, ["bus", "/usr/bin/Xvfb", "stop /usr/bin/Xvfb", "bus stopped"])
                owner.assert_not_called()
                probe.assert_not_called()
            else:
                hosted.run(Path(d), pinyin_fixture=fixture, pinyin_gl_diagnostic=diagnostic)
                self.assertEqual(events, ["bus", "/usr/bin/Xvfb", "/usr/bin/fcitx5",
                                          "stop /usr/bin/fcitx5", "stop /usr/bin/Xvfb", "bus stopped"])
                owner.assert_called_once()
                probe.assert_called_once()
                self.assertEqual(probe.call_args.args[0].get("GDK_DEBUG"), "gl-disable" if diagnostic else None)
                if diagnostic:
                    fixture.install_profile.assert_called_once()
                    fixture.run.assert_called_once()
                    app_env, app_runtime = fixture.run.call_args.args
                    self.assertNotIn("GDK_DEBUG", app_env)
                    self.assertEqual(app_runtime, Path(d))
                    self.assertEqual(fixture.run.call_args.kwargs, {"disable_gl": True})
            peer.assert_called_once()

    def test_actual_entrypoint_wires_all_steps_in_order(self):
        self.execute(False)

    def test_gl_diagnostic_executes_same_full_fixture_with_scoped_probe_flag(self):
        self.execute(False, diagnostic=True)

    def test_rejected_auth_never_starts_fcitx_or_module_probe(self):
        self.execute(True)


if __name__ == "__main__":
    unittest.main()
