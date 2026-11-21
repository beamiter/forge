#!/usr/bin/env python3
"""Exercise the production private-bus entrypoint with fake native boundaries."""

import importlib.util
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

p = Path(__file__).with_name("qa-fcitx5-owned-bus.py")
spec = importlib.util.spec_from_file_location("owned_bus", p)
bus = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bus)


class OwnedBusTests(unittest.TestCase):
    uid = 1000

    def process(self):
        p = Mock(pid=42, returncode=None)
        p.poll.return_value = None
        return p

    def listener(self, pid=42, uid=1000):
        peer = Mock()
        peer.getsockopt.return_value = struct.pack("3i", pid, uid, 1000)
        factory = Mock()
        factory.return_value.__enter__ = Mock(return_value=peer)
        factory.return_value.__exit__ = Mock(return_value=False)
        return factory, peer

    def test_owned_listener_reaches_only_exact_socket(self):
        factory, peer = self.listener()
        addr = Path("/tmp/nonexistent-fixture/bus")
        with patch.object(bus.socket, "socket", factory):
            bus.wait_for_owned_peer(self.process(), addr, self.uid)
        factory.assert_called_once_with(socket.AF_UNIX, socket.SOCK_STREAM)
        peer.connect.assert_called_once_with(str(addr))
        peer.getsockopt.assert_called_once_with(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))

    def test_wrong_peer_pid_or_uid_is_rejected_before_readiness(self):
        for pid, uid in [(43, self.uid), (42, self.uid + 1)]:
            factory, _ = self.listener(pid=pid, uid=uid)
            with self.subTest(pid=pid, uid=uid), patch.object(bus.socket, "socket", factory):
                with self.assertRaisesRegex(bus.Unavailable, "not the owned daemon"):
                    bus.wait_for_owned_peer(self.process(), Path("/tmp/test-fixture/bus"), self.uid)

    def test_dead_daemon_does_not_connect(self):
        p = self.process()
        p.poll.return_value = p.returncode = 7
        with patch.object(bus.socket, "socket") as connect:
            with self.assertRaisesRegex(bus.Unavailable, "exited before ready"):
                bus.wait_for_owned_peer(p, Path("/tmp/test-fixture/bus"), self.uid)
        connect.assert_not_called()

    def test_repeated_missing_socket_hits_deadline(self):
        factory, peer = self.listener()
        peer.connect.side_effect = FileNotFoundError
        with patch.object(bus.socket, "socket", factory):
            with self.assertRaisesRegex(bus.Unavailable, "timed out"):
                bus.wait_for_owned_peer(self.process(), Path("/tmp/test-fixture/bus"), self.uid,
                                        timeout=0.04)

    def launch(self, reply, connectivity=None):
        with tempfile.TemporaryDirectory(prefix="forge-bus-test-", dir="/tmp") as d:
            scope = Path(d)
            daemon = self.process()
            answer = subprocess.CompletedProcess(["gdbus"], *reply)
            env = {"DBUS_SESSION_BUS_ADDRESS": "unix:path=/unowned/session",
                   "DBUS_SYSTEM_BUS_ADDRESS": "unix:path=/unowned/system",
                   "DISPLAY": ":0", "WAYLAND_DISPLAY": "wayland-0", "HOME": "/ambient"}
            with patch.dict(bus.os.environ, env), patch.object(bus.subprocess, "Popen", return_value=daemon) as spawn, patch.object(
                    bus, "wait_for_owned_peer", side_effect=connectivity) as ready, patch.object(
                    bus.subprocess, "run", return_value=answer) as call:
                if connectivity is not None or reply != (0, b"('0123456789abcdef0123456789abcdef',)\n", b""):
                    with self.assertRaises(bus.Unavailable):
                        with bus.private_bus(scope):
                            self.fail("readiness must not yield")
                else:
                    with bus.private_bus(scope) as actual:
                        self.assertEqual(actual, f"unix:path={scope}/bus")
                        daemon.terminate.assert_not_called()
                ready.assert_called_once_with(daemon, scope / "bus", bus.os.geteuid())
                child_env = spawn.call_args.kwargs["env"]
                self.assertEqual(child_env["HOME"], str(scope))
                self.assertFalse((set(env) - {"HOME"}) & set(child_env))
                daemon_args = spawn.call_args.args[0]
                self.assertEqual(daemon_args, [bus.DAEMON, "--nofork", "--nopidfile",
                                               f"--config-file={scope}/bus.conf"])
                cfg = (scope / "bus.conf").read_text()
                self.assertIn(f"<listen>unix:path={scope}/bus</listen>", cfg)
                self.assertNotIn("standard_session_servicedirs", cfg)
                self.assertNotIn("eavesdrop", cfg)
                self.assertNotIn('own="*"', cfg)
                self.assertIn('own="org.fcitx.Fcitx5"', cfg)
                if connectivity is not None:
                    call.assert_not_called()
                else:
                    args = call.call_args.args[0]
                    self.assertEqual(args[:4], [bus.CLIENT, "call", "--address", f"unix:path={scope}/bus"])
                    self.assertEqual(args[-1], "org.freedesktop.DBus.GetId")
                    self.assertEqual(call.call_args.kwargs["timeout"], 3)
                daemon.terminate.assert_called_once()
                daemon.wait.assert_called_once_with(timeout=2)

    def test_production_entrypoint_validates_owned_bus_and_getid(self):
        self.launch((0, b"('0123456789abcdef0123456789abcdef',)\n", b""))

    def test_partial_readiness_or_unknown_peer_cannot_pass(self):
        self.launch((0, b"arbitrary success", b""))
        self.launch((0, b"('0123456789abcdef0123456789abcdef',)\n", b"warning"))
        self.launch((0, b"('0123456789abcdef0123456789abcdef',)\n", b""),
                    connectivity=bus.Unavailable("not owned"))

    def test_preexisting_bus_never_spawns_daemon(self):
        with tempfile.TemporaryDirectory(prefix="forge-bus-test-", dir="/tmp") as d:
            scope = Path(d)
            (scope / "bus").symlink_to("/ambient/session")
            with patch.object(bus.subprocess, "Popen") as spawn:
                with self.assertRaisesRegex(bus.Unavailable, "preexisting"):
                    with bus.private_bus(scope):
                        self.fail("preexisting socket must not be reused")
                spawn.assert_not_called()

    def test_unresponsive_owned_child_gets_killed_once(self):
        p = self.process()
        p.wait.side_effect = [subprocess.TimeoutExpired("dbus", 2), 0]
        bus.stop_owned_child(p)
        p.terminate.assert_called_once()
        p.kill.assert_called_once()


if __name__ == "__main__":
    unittest.main()
