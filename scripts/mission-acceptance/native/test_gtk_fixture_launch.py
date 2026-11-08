#!/usr/bin/env python3
"""Private X11 inheritance and startup receipts; no display, bus or apps run."""
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

from test_gtk_geometry_smoke import (fixture_environment,fixture_exit_receipt,fixture_stderr_receipt,
                                     finalize_fixture_report,fixture_temporary_directory)


class FixtureEnvironmentTests(unittest.TestCase):
    def setUp(self):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        self.root=Path(temporary.name).resolve()
        private=self.root/'xvfb-run.test';private.mkdir(mode=0o700)
        self.authority=private/'Xauthority';self.authority.write_bytes(b'fixture-cookie-never-read-or-emitted')
        self.authority.chmod(0o600)
        self.source=dict(TMPDIR=str(self.root),XAUTHORITY=str(self.authority),DISPLAY=':99',
                         HOME=str(self.root/'home'),DBUS_SESSION_BUS_ADDRESS='unix:path=/private/test',
                         GSK_RENDERER='cairo',LIBGL_ALWAYS_SOFTWARE='1',OPENAI_API_KEY='must-not-pass',
                         LD_PRELOAD='must-not-pass',PYTHONPATH='must-not-pass')
    def test_only_existing_private_authority_path_and_render_settings_are_inherited(self):
        with patch.object(Path,'read_bytes',side_effect=AssertionError('cookie read forbidden')):
            result=fixture_environment(self.source)
        self.assertEqual(result['XAUTHORITY'],str(self.authority))
        self.assertEqual(result['GSK_RENDERER'],'cairo')
        self.assertEqual(result['LIBGL_ALWAYS_SOFTWARE'],'1')
        self.assertEqual(result['DISPLAY'],':99')
        self.assertFalse(set(result)&{'OPENAI_API_KEY','LD_PRELOAD','PYTHONPATH'})
        self.assertNotIn('fixture-cookie-never-read-or-emitted',json.dumps(result))
    def test_missing_authority_fails_without_home_cookie_or_no_auth_fallback(self):
        source=dict(self.source);del source['XAUTHORITY']
        for candidate in (source,dict(self.source,XAUTHORITY=''),dict(self.source,XAUTHORITY=None)):
            with self.subTest(source=candidate),self.assertRaisesRegex(RuntimeError,'requires inherited'):
                fixture_environment(candidate)
    def test_relative_outside_missing_and_noncanonical_paths_are_rejected(self):
        for value in ('relative-Xauthority',str(self.root/'missing'),str(self.root.parent/'outside'),
                      str(self.authority.parent/'..'/self.authority.parent.name/'Xauthority')):
            with self.subTest(value=value),self.assertRaises((RuntimeError,FileNotFoundError)):
                fixture_environment(dict(self.source,XAUTHORITY=value))
    def test_explicit_private_tmpdir_is_required_without_host_fallback(self):
        source=dict(self.source);del source['TMPDIR']
        with patch('test_gtk_geometry_smoke.tempfile.gettempdir',side_effect=AssertionError('host fallback forbidden')):
            with self.assertRaisesRegex(RuntimeError,'requires inherited.*TMPDIR'):
                fixture_environment(source)
            self.assertEqual(fixture_environment(self.source)['XAUTHORITY'],str(self.authority))
        self.root.chmod(0o755)
        with self.assertRaisesRegex(RuntimeError,'owned private directory'):
            fixture_environment(self.source)

    def test_authority_symlink_or_directory_is_rejected(self):
        link=self.authority.parent/'link';link.symlink_to(self.authority)
        for path in (link,self.authority.parent):
            with self.subTest(path=path),self.assertRaises(RuntimeError):
                fixture_environment(dict(self.source,XAUTHORITY=str(path)))
    def test_authority_outside_inherited_tmpdir_is_rejected_even_when_owned(self):
        other=self.root/'other';other.mkdir(mode=0o700)
        with self.assertRaisesRegex(RuntimeError,'temporary session'):
            fixture_environment(dict(self.source,TMPDIR=str(other)))
    def test_world_writable_authority_and_nonprivate_parent_are_rejected(self):
        self.authority.chmod(0o666)
        with self.assertRaisesRegex(RuntimeError,'regular file'):fixture_environment(self.source)
        self.authority.chmod(0o600);self.authority.parent.chmod(0o755)
        with self.assertRaisesRegex(RuntimeError,'private xvfb-run'):fixture_environment(self.source)
    def test_readable_file_remains_protected_by_its_private_parent(self):
        self.authority.chmod(0o644)
        self.assertEqual(fixture_environment(self.source)['XAUTHORITY'],str(self.authority))
    def test_foreign_owner_is_rejected(self):
        uid=os.getuid()
        with patch('test_gtk_geometry_smoke.os.getuid',return_value=uid+1):
            with self.assertRaisesRegex(RuntimeError,'owned'):fixture_environment(self.source)
    def test_cookie_directly_in_tmp_root_is_rejected(self):
        direct=self.root/'Xauthority';direct.write_bytes(b'test');direct.chmod(0o600)
        with self.assertRaisesRegex(RuntimeError,'private xvfb-run'):
            fixture_environment(dict(self.source,XAUTHORITY=str(direct)))


class FixtureStartupReceiptTests(unittest.TestCase):
    def test_natural_startup_exit_code_is_not_replaced_by_cleanup(self):
        process=Mock();process.poll.return_value=1
        result=fixture_exit_receipt(process)
        self.assertEqual(result['fixture_exit_code_before_cleanup'],1)
        self.assertEqual(result['fixture_exit_code'],1)
        self.assertFalse(result['fixture_terminated_for_cleanup'])
        process.terminate.assert_not_called();process.kill.assert_not_called()
    def test_running_fixture_cleanup_is_explicit_and_bounded(self):
        process=Mock();process.poll.side_effect=[None,-15]
        result=fixture_exit_receipt(process)
        self.assertIsNone(result['fixture_exit_code_before_cleanup'])
        self.assertEqual(result['fixture_exit_code'],-15)
        self.assertTrue(result['fixture_terminated_for_cleanup'])
        process.wait.assert_called_once_with(timeout=5)
    def test_timeout_uses_only_owned_process_kill_and_records_it(self):
        process=Mock();process.poll.side_effect=[None,-9]
        process.wait.side_effect=[subprocess.TimeoutExpired('fixture',5),-9]
        result=fixture_exit_receipt(process)
        self.assertTrue(result['fixture_killed_for_cleanup'])
        process.kill.assert_called_once_with()
    def test_small_stderr_and_invalid_utf8_are_preserved_safely(self):
        data=b'Gtk-WARNING: cannot open display :99\n\xff'
        result=fixture_stderr_receipt(io.BytesIO(data))
        self.assertIn('cannot open display :99',result['fixture_stderr_tail'])
        self.assertFalse(result['fixture_stderr_truncated'])
        self.assertEqual(result['fixture_stderr_total_bytes'],len(data))
        json.dumps(result)
    def test_large_stderr_reads_only_bounded_tail(self):
        class RecordedBytes(io.BytesIO):
            def read(self,size=-1):
                self.requested=size
                if size<0 or size>8192:raise AssertionError('Unbounded stderr read')
                return super().read(size)
        stream=RecordedBytes(b'prefix-not-kept'+b'x'*20000+b'last failure')
        result=fixture_stderr_receipt(stream)
        self.assertTrue(result['fixture_stderr_truncated'])
        self.assertEqual(len(result['fixture_stderr_tail']),8192)
        self.assertTrue(result['fixture_stderr_tail'].endswith('last failure'))
        self.assertNotIn('prefix-not-kept',result['fixture_stderr_tail'])
        self.assertEqual(stream.requested,8192)


class FixtureFinalizationTests(unittest.TestCase):
    def inputs(self):
        report=dict(status='passed')
        process=Mock();process.poll.return_value=1
        stream=Mock(wraps=io.BytesIO(b'bounded fixture failure'))
        path=Mock()
        return report,process,stream,path
    def test_close_failure_keeps_primary_failure_and_still_writes_diagnostics(self):
        report,process,stream,path=self.inputs()
        primary=RuntimeError('primary GTK startup failure')
        stream.close.side_effect=OSError('stderr close failed')
        with self.assertRaises(RuntimeError) as caught:
            try:raise primary
            finally:finalize_fixture_report(report,process,stream,path,primary)
        self.assertIs(caught.exception,primary)
        saved=json.loads(path.write_text.call_args.args[0])
        self.assertEqual(saved['fixture_stderr_close_error'],'stderr close failed')
        self.assertEqual(saved['fixture_stderr_tail'],'bounded fixture failure')
        self.assertIn('stderr close failed',primary.__notes__[0])
    def test_receipt_write_failure_keeps_primary_and_records_secondary_note(self):
        report,process,stream,path=self.inputs()
        primary=RuntimeError('primary GTK startup failure')
        path.write_text.side_effect=OSError('receipt write failed')
        with self.assertRaises(RuntimeError) as caught:
            try:raise primary
            finally:finalize_fixture_report(report,process,stream,path,primary)
        self.assertIs(caught.exception,primary)
        self.assertEqual(report['fixture_receipt_write_error'],'receipt write failed')
        self.assertIn('receipt write failed',primary.__notes__[0])
        stream.close.assert_called_once_with()
    def test_close_or_receipt_failure_after_success_cannot_report_success(self):
        for failure in ('close','write'):
            report,process,stream,path=self.inputs();error=OSError(failure+' failed')
            if failure=='close':stream.close.side_effect=error
            else:path.write_text.side_effect=error
            with self.subTest(failure=failure),self.assertRaises(OSError) as caught:
                finalize_fixture_report(report,process,stream,path)
            self.assertIs(caught.exception,error)
            self.assertEqual(report['status'],'failed')
    def test_cleanup_and_read_failures_do_not_prevent_close_or_receipt(self):
        report,process,stream,path=self.inputs()
        primary=RuntimeError('primary')
        process.poll.side_effect=OSError('poll failed');stream.flush.side_effect=OSError('read failed')
        finalize_fixture_report(report,process,stream,path,primary)
        stream.close.assert_called_once_with();path.write_text.assert_called_once()
        self.assertIn('poll failed',primary.__notes__[0]);self.assertIn('read failed',primary.__notes__[0])


class FixtureTemporaryCleanupTests(unittest.TestCase):
    def test_outer_temporary_cleanup_cannot_replace_primary_failure(self):
        temporary=Mock(name='temporary');temporary.name='/private/fixture'
        temporary.cleanup.side_effect=OSError('temporary cleanup failed')
        primary=RuntimeError('primary GTK startup failure')
        with patch('test_gtk_geometry_smoke.tempfile.TemporaryDirectory',return_value=temporary):
            with self.assertRaises(RuntimeError) as caught:
                with fixture_temporary_directory() as path:
                    self.assertEqual(path,'/private/fixture')
                    raise primary
        self.assertIs(caught.exception,primary)
        self.assertIn('temporary cleanup failed',primary.__notes__[0])
    def test_outer_temporary_cleanup_after_success_still_fails(self):
        temporary=Mock(name='temporary');temporary.name='/private/fixture'
        error=OSError('temporary cleanup failed');temporary.cleanup.side_effect=error
        with patch('test_gtk_geometry_smoke.tempfile.TemporaryDirectory',return_value=temporary):
            with self.assertRaises(OSError) as caught:
                with fixture_temporary_directory():pass
        self.assertIs(caught.exception,error)


if __name__=='__main__':unittest.main()
