import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('shell', Path(__file__).with_name('test-chat-shell-smoke.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class ShellOracles(unittest.TestCase):
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
