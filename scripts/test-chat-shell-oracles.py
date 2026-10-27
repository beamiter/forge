import importlib.util
from pathlib import Path
import unittest
import os
import socket
import stat
import subprocess
import json
import tempfile
from unittest import mock

spec = importlib.util.spec_from_file_location('shell', Path(__file__).with_name('test-chat-shell-smoke.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class ShellOracles(unittest.TestCase):
    def test_keyboard_barrier_arguments(self):
        self.assertEqual(m.SEAT_KEYBOARD_ARGS, ['wtype', '-', '-k', 'F24'])
    def test_keyboard_trace_rejects_any_input(self):
        clean = ' -> zwp_virtual_keyboard_v1@7.keymap(1, fd 3, 200)\n'
        self.assertTrue(m.keyboard_trace_ready(clean))
        self.assertFalse(m.keyboard_trace_ready(''))
        for event in ['key(0, 1, 1)', 'modifiers(0, 0, 0, 0)']:
            with self.assertRaises(RuntimeError):
                m.keyboard_trace_ready(clean + ' -> zwp_virtual_keyboard_v1@7.' + event + '\n')
    def test_keyboard_seat_requires_owned_single_device_and_capability(self):
        inputs = [dict(type='keyboard', identifier='virtual-keyboard')]
        seats = [dict(name='seat0', capabilities=2, devices=inputs)]
        self.assertTrue(m.keyboard_seat_ready(inputs, seats))
        self.assertFalse(m.keyboard_seat_ready([], seats))
        self.assertFalse(m.keyboard_seat_ready(inputs * 2, seats))
        self.assertFalse(m.keyboard_seat_ready(inputs, [dict(seats[0], capabilities=0)]))
        self.assertFalse(m.keyboard_seat_ready(inputs, [dict(seats[0], devices=[])]))
    def test_cleanup_reaps_before_stdin_close_even_after_term_timeout(self):
        for resistant in [False, True]:
            calls = []
            class Pipe:
                closed = False
                def close(self):
                    calls.append('close')
                    self.closed = True
            class Process:
                stdin = Pipe()
                def poll(self): return None
                def terminate(self): calls.append('terminate')
                def kill(self): calls.append('kill')
                def wait(self, timeout):
                    calls.append(('wait', timeout))
                    if resistant and 'kill' not in calls:
                        raise subprocess.TimeoutExpired('owned', timeout)
            m.stop_owned(Process())
            expected = ['terminate', ('wait', 5)]
            if resistant: expected += ['kill', ('wait', 5)]
            self.assertEqual(calls, expected + ['close'])
    def test_cleanup_does_not_release_pipe_if_reaping_fails(self):
        class Pipe:
            closed = False
            def close(self): raise AssertionError('must not release stdin')
        class Process:
            stdin = Pipe()
            def poll(self): return None
            def terminate(self): pass
            def kill(self): pass
            def wait(self, timeout): raise subprocess.TimeoutExpired('owned', timeout)
        with self.assertRaises(subprocess.TimeoutExpired): m.stop_owned(Process())
    def test_cleanup_reaps_terminate_lookup_race_before_pipe_close(self):
        calls = []
        class Pipe:
            closed = False
            def close(self): calls.append('close')
        class Process:
            stdin = Pipe()
            def poll(self): return None
            def terminate(self): raise ProcessLookupError('already exited')
            def wait(self, timeout): calls.append('reap')
        m.stop_owned(Process())
        self.assertEqual(calls, ['reap', 'close'])
    def test_final_pass_only_after_successful_cleanup(self):
        with tempfile.TemporaryDirectory() as name:
            out = Path(name)
            (out / 'seat-keyboard.log').write_text(' -> zwp_virtual_keyboard_v1@7.keymap(1, fd 3, 200)\n')
            def stop(process): self.assertFalse((out / 'result.json').exists())
            with mock.patch.object(m, 'stop_owned', side_effect=stop):
                result = m.finish_outcome(out, dict(stages=[]), None, [object()])
            self.assertEqual(result['status'], 'pass')
            self.assertFalse((out / 'result.json').exists())
    def test_cleanup_failure_or_contamination_never_publishes_pass(self):
        for contamination in [False, True]:
            with tempfile.TemporaryDirectory() as name:
                out = Path(name)
                (out / 'seat-keyboard.log').write_text(' -> zwp_virtual_keyboard_v1@7.key(0, 1, 1)\n' if contamination else '')
                calls = []
                def stop(process):
                    calls.append(process)
                    if not contamination: raise RuntimeError('reap failed')
                with mock.patch.object(m, 'stop_owned', side_effect=stop):
                    with self.assertRaises(RuntimeError):
                        m.finish_outcome(out, dict(stages=[]), None, [1, 2])
                self.assertEqual(calls, [2, 1])
                outcome = json.loads((out / 'result.json').read_text())
                self.assertEqual(outcome['status'], 'fail')
                self.assertTrue(outcome['cleanup_errors'])
    def test_original_failure_retained_with_cleanup_failure(self):
        with tempfile.TemporaryDirectory() as name:
            out = Path(name)
            (out / 'seat-keyboard.log').write_text(' -> zwp_virtual_keyboard_v1@7.keymap(1, fd 3, 200)\n')
            original = ValueError('original shell evidence failure')
            with mock.patch.object(m, 'stop_owned', side_effect=RuntimeError('cleanup failure')):
                m.finish_outcome(out, None, original, [object()])
            outcome = json.loads((out / 'result.json').read_text())
            self.assertEqual(outcome['status'], 'fail')
            self.assertEqual(outcome['original_error'], repr(original))
            self.assertTrue(outcome['cleanup_errors'])
    def test_runtime_cleanup_failure_cannot_leave_pass(self):
        with tempfile.TemporaryDirectory() as name:
            out = Path(name)
            class Runtime:
                def __enter__(self): return name
                def __exit__(self, *args): raise RuntimeError('runtime cleanup failed')
            with mock.patch.object(m, 'private_runtime', return_value=Runtime()):
                with mock.patch.object(m, '_run_case', return_value=dict(status='pass')):
                    with self.assertRaisesRegex(RuntimeError, 'runtime cleanup failed'):
                        m.run_case(None, out, 1, 'test')
            result = json.loads((out / 'result.json').read_text())
            self.assertEqual(result['status'], 'fail')
            self.assertIn('runtime cleanup failed', result['outer_error'])
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
        for n, (path, token) in enumerate([('key', 'first-token'), ('pst', 'second-token')], 1):
            execution_id = f'forge-bash-42-{n}'
            result.append(dict(path=path, token=token, pid=42, tty='/dev/pts/7',
                capsule=dict(font='DejaVu Sans Mono 14', rows=4, padding=[1,1,1,1]),
                metadata_identity=dict(execution_id=execution_id, block_id=n, events=[
                    dict(phase='start', execution_id=execution_id, block_id=n, command_source='Screen'),
                    dict(phase='end', execution_id=execution_id, correlation='MatchedUntrusted', exit_code=0),
                    dict(phase='record', execution_id=execution_id, block_id=n, command_source='Screen', completion_provenance='ShellReported')]),
                accepted_hex=b'\0'.join([token.encode(), ('中文 e\u0301 ＡＢＣ '*8).encode(), b'42', b'/dev/pts/7', execution_id.encode(), b'']).hex()))
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
    def test_rejects_unrelated_or_incomplete_history_disabled_capsules(self):
        for index, field, value in [(0, 'block_id', 99), (1, 'execution_id', 'foreign'),
                                    (1, 'exit_code', 1), (1, 'correlation', 'Unmatched'),
                                    (2, 'completion_provenance', 'BoundaryInferred')]:
            records = self.records()
            records[0]['metadata_identity']['events'][index][field] = value
            with self.assertRaises(RuntimeError): m.shell_acceptance(records)
        records = self.records()
        records[0]['metadata_identity']['events'].pop()
        with self.assertRaises(RuntimeError): m.shell_acceptance(records)
    def test_rejects_reused_history_disabled_execution_or_capsule(self):
        for field in ['execution_id', 'block_id']:
            records = self.records()
            records[1]['metadata_identity'][field] = records[0]['metadata_identity'][field]
            with self.assertRaises(RuntimeError): m.shell_acceptance(records)
    def test_rejects_missing_path(self):
        with self.assertRaises(RuntimeError):m.shell_acceptance(self.records()[:1])

unittest.main()
