import importlib.util
from pathlib import Path
import unittest
import os
import socket
import stat

spec = importlib.util.spec_from_file_location('shell', Path(__file__).with_name('test-chat-shell-smoke.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class ShellOracles(unittest.TestCase):
    def test_owned_short_runtime_budget_mode_and_cleanup(self):
        with m.private_runtime() as name:
            path = Path(name)
            self.assertEqual(path.stat().st_uid, os.getuid())
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o700)
            endpoint = path / 'sway-ipc.4294967295.2147483647.sock'
            self.assertLess(len(os.fsencode(endpoint)), 108)
        self.assertFalse(path.exists())
    def test_long_artifact_path_exceeds_sway_socket_budget(self):
        old = Path('/home/runner/work/forge/forge/gtk-paint-diagnostics/f13-shell/scale-1-font-0/runtime')
        self.assertGreaterEqual(len(os.fsencode(old / 'sway-ipc.1001.1234.sock')), 108)
    def test_runtime_cleanup_on_exception(self):
        with self.assertRaisesRegex(RuntimeError, 'controlled failure'):
            with m.private_runtime() as name:
                path = Path(name)
                raise RuntimeError('controlled failure')
        self.assertFalse(path.exists())
    def test_native_af_unix_longest_sway_suffix_bind(self):
        # This remains mandatory in hosted CI. Some cloud source workspaces
        # prohibit AF_UNIX creation; do not replace this with a mocked bind.
        with m.private_runtime() as name:
            endpoint = Path(name) / 'sway-ipc.4294967295.2147483647.sock'
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                sock.bind(str(endpoint))
    def records(self):
        result = []
        for path, token in [('key', 'first-token'), ('pst', 'second-token')]:
            result.append(dict(path=path, token=token, pid=42, tty='/dev/pts/7',
                capsule=dict(font='DejaVu Sans Mono 14', rows=4, padding=[1,1,1,1]),
                accepted_hex=b'\0'.join([token.encode(), ('中文 e\u0301 ＡＢＣ '*8).encode(), b'42', b'/dev/pts/7', b'']).hex()))
        return result
    def test_accepts_exact_same_owned_shell_and_equal_capsules(self):
        m.shell_acceptance(self.records())
    def test_rejects_corrupt_unicode_bytes(self):
        r=self.records();r[0]['accepted_hex']=r[0]['accepted_hex'].replace('e4b8ad','e4b8',1)
        with self.assertRaises(RuntimeError):m.shell_acceptance(r)
    def test_rejects_wrong_pid_tty_reused_token_or_metrics(self):
        for key,value in [('pid',43),('tty','/dev/pts/8'),('token','first-token'),('capsule',{})]:
            r=self.records();r[1][key]=value
            with self.assertRaises(RuntimeError):m.shell_acceptance(r)
    def test_rejects_missing_path(self):
        with self.assertRaises(RuntimeError):m.shell_acceptance(self.records()[:1])

unittest.main()
