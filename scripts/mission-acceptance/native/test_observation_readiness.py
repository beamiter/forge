#!/usr/bin/env python3
"""Fault-injected readiness tests. No native app, host, X server or provider runs."""
import copy
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import observer
import reader


class BusFixture:
    """Small public AT-SPI/D-Bus surface; all calls and timeouts are recorded."""
    def __init__(self):
        self.owner = ':1.7'
        self.root = '/app'
        self.roots = [(self.owner, self.root)]
        self.latest_roots = None
        self.children = {'/app': ['/button'], '/button': []}
        self.latest_children = None
        self.names = {'/app': 'Application', '/button': 'Missions'}
        self.latest_names = {}
        self.text = None
        self.latest_text = None
        self.pid = 21
        self.final_pid = 21
        self.fail = None
        self.counts = {}
        self.calls = []
        self.module = SimpleNamespace(SessionBus=lambda: self,
            bus=SimpleNamespace(BusConnection=lambda address: self),
            UInt32=int, Int32=int,
            Interface=lambda obj, interface: self.endpoint(*obj, interface))

    def get_object(self, owner, path, introspect=True):
        if introspect:
            raise AssertionError('Unbounded implicit introspection is forbidden')
        return owner, path

    def endpoint(self, owner, path, interface):
        fixture = self
        class Endpoint:
            def __getattr__(self, method):
                def call(*args, timeout):
                    key = (owner, path, interface, method, args)
                    fixture.counts[key] = fixture.counts.get(key, 0) + 1
                    count = fixture.counts[key]
                    fixture.calls.append((method, path, timeout))
                    if fixture.fail and fixture.fail(method, path, count):
                        raise ValueError('real D-Bus failure: ' + method)
                    if method == 'GetAddress':
                        return 'unix:path=/fake-atspi'
                    if method == 'GetNameOwner':
                        return args[0]
                    if method == 'GetConnectionUnixProcessID':
                        return fixture.pid if count == 1 else fixture.final_pid
                    if owner == 'org.a11y.atspi.Registry':
                        return fixture.latest_roots if count > 1 and fixture.latest_roots is not None else fixture.roots
                    if method == 'GetChildren':
                        source = (fixture.latest_children if fixture.latest_children is not None
                                  and (count > 1 or path not in fixture.children) else fixture.children)
                        return [(fixture.owner, child) if isinstance(child, str) else child
                                for child in source[path]]
                    if method == 'GetInterfaces':
                        return ([observer.COMPONENT, observer.TEXT] if fixture.text is not None
                                and path == '/button' else [observer.COMPONENT])
                    if method == 'GetRole':
                        return 43 if path == '/button' else 75
                    if method == 'GetState':
                        return [(1 << 8) | (1 << 24) | (1 << 25) | (1 << 30), 0]
                    if method in ('GetExtents', 'GetRangeExtents'):
                        return [10, 10, 80, 20]
                    if method == 'Get':
                        if args[1] == 'Name':
                            return fixture.latest_names.get(path, fixture.names[path]) if count > 1 else fixture.names[path]
                        text = fixture.latest_text if count > 1 and fixture.latest_text is not None else fixture.text
                        return len(text)
                    if method == 'GetText':
                        return fixture.latest_text if count > 1 and fixture.latest_text is not None else fixture.text
                    raise AssertionError('Unexpected AT-SPI call: ' + repr(key))
                return call
        return Endpoint()

    def observe(self, **kwargs):
        with patch.dict(sys.modules, {'dbus': self.module}):
            return observer.observe_atspi(21, [0, 0, 640, 480], **kwargs)


class TypedObservationTests(unittest.TestCase):
    def test_only_absent_exact_pid_root_is_temporary(self):
        bus = BusFixture(); bus.roots = []
        with self.assertRaises(observer.ObservationNotReady):
            bus.observe()
        for roots in [[(':1.7', '/app'), (':1.7', '/other')], [(':1.7', '/app')] * 2]:
            bus = BusFixture(); bus.roots = roots
            with self.assertRaisesRegex(RuntimeError, 'Ambiguous') as caught:
                bus.observe()
            self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
        bus = BusFixture(); bus.pid = 99
        with self.assertRaises(observer.ObservationNotReady):
            bus.observe()

    def test_stable_owned_tree_returns_without_repair(self):
        bus = BusFixture()
        result = bus.observe()
        self.assertEqual(result['pid'], 21)
        self.assertEqual(result['owner'], ':1.7')
        self.assertIn('Missions', result['visible_text'])
        observer.validate_tree(result)

    def test_valid_structure_name_and_text_updates_are_typed(self):
        for change in ('tree', 'name', 'text'):
            with self.subTest(change=change):
                bus = BusFixture()
                if change == 'tree':
                    bus.latest_children = {'/app': ['/button', '/added'], '/button': [], '/added': []}
                elif change == 'name':
                    bus.latest_names['/button'] = 'Missions updated'
                else:
                    bus.text = 'old'; bus.latest_text = 'new'
                with self.assertRaisesRegex(observer.ObservationNotReady, change + ' changed'):
                    bus.observe()

    def test_foreign_duplicate_cycle_and_invalid_path_are_always_hard(self):
        variants = [
            {'/app': [(':1.99', '/foreign')], '/button': []},
            {'/app': ['/button', '/button'], '/button': []},
            {'/app': ['/button'], '/button': ['/app']},
            {'/app': ['/invalid!'], '/button': []},
        ]
        for latest in (False, True):
            for children in variants:
                with self.subTest(latest=latest, children=children):
                    bus = BusFixture()
                    if latest:
                        bus.latest_children = children
                    else:
                        bus.children = children
                    with self.assertRaises(RuntimeError) as caught:
                        bus.observe()
                    self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)

    def test_new_subtree_is_validated_before_reporting_a_change(self):
        for bad in [[(':1.99', '/foreign')], ['/app'], ['/added']]:
            bus = BusFixture()
            bus.latest_children = {'/app': ['/button', '/added'], '/button': [], '/added': bad}
            with self.assertRaises(RuntimeError) as caught:
                bus.observe()
            self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)

    def test_owner_pid_error_is_not_hidden_by_a_tree_change(self):
        bus = BusFixture(); bus.latest_names['/button'] = 'Changed'; bus.final_pid = 99
        with self.assertRaisesRegex(RuntimeError, 'PID changed') as caught:
            bus.observe()
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)

    def test_late_root_ambiguity_or_owner_change_is_hard_even_after_content_changed(self):
        for roots in [[(':1.7', '/app'), (':1.7', '/other')], [(':1.8', '/app')], [(':1.7', '/other')]]:
            bus = BusFixture(); bus.latest_names['/button'] = 'Changed'; bus.latest_roots = roots
            with self.subTest(roots=roots), self.assertRaises(RuntimeError) as caught:
                bus.observe()
            self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)

    def test_text_bound_cannot_be_downgraded_to_temporary_text_change(self):
        bus = BusFixture(); bus.text = 'old'; bus.latest_text = 'x' * 131073
        with self.assertRaisesRegex(RuntimeError, 'Text bound exceeded') as caught:
            bus.observe()
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)

    def test_real_dbus_errors_are_not_reclassified(self):
        for method in ('GetAddress', 'GetNameOwner', 'GetConnectionUnixProcessID', 'GetChildren', 'Get'):
            bus = BusFixture(); bus.fail = lambda actual, path, count: actual == method
            with self.subTest(method=method), self.assertRaisesRegex(ValueError, 'real D-Bus failure'):
                bus.observe()
        bus = BusFixture(); bus.latest_names['/button'] = 'Changed'
        bus.fail = lambda method, path, count: method == 'GetNameOwner' and count > 1
        with self.assertRaisesRegex(ValueError, 'real D-Bus failure'):
            bus.observe()

    def test_deadline_is_hard_and_dbus_timeouts_use_remaining_budget(self):
        bus = BusFixture()
        with patch('observer.time.monotonic', return_value=100):
            bus.observe(deadline=100.25)
        self.assertTrue(bus.calls)
        self.assertTrue(all(0 < timeout <= .25 for _, _, timeout in bus.calls))
        bus = BusFixture()
        with patch('observer.time.monotonic', return_value=100), self.assertRaisesRegex(RuntimeError, 'deadline') as caught:
            bus.observe(deadline=100)
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
        self.assertEqual(bus.calls, [])


class Clock:
    def __init__(self):
        self.now = 100.0
        self.sleeps = []
    def sleep(self, seconds):
        self.sleeps.append(seconds)
        self.now += seconds


class ReaderReadinessTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(); self.addCleanup(temporary.cleanup)
        self.out = Path(temporary.name)
        self.clock = Clock()
        self.identity = dict(pid=21, start_ticks='100', executable='/tmp/installed/anvil', state_dir='/tmp/shared')
        self.actual_identity = self.identity.copy()
        self.process = Mock(pid=21); self.process.poll.return_value = None
        self.host = Mock()
        plan = dict(id='mission-exact', title='Saved mission', goal='Read only', required_environment='native',
                    revision=1, repositories=[])
        self.r = reader.Reader('anvil', dict(binary=self.identity['executable'], sha256='a'*64, config='/tmp/config'),
            dict(HOME='/tmp/fresh', JTERM_WORKSPACE_STATE_DIR='/tmp/shared'), plan, self.host, self.out)
        self.r.deadline = self.clock.now + 300
        self.r.process = self.process; self.r.process_identity = self.identity.copy()
        self.commands = []; self.captures = []; self.searches = 0
        self.empty_searches = 0; self.ids = ['4194307']; self.window_pid = '21'
        self.active = '4194307'; self.rect = [0, 0, 640, 480]
        self.colors = [2]; self.metadata = None; self.fail_command = None
        self.bind_reads = 0; self.move_after_capture = False
        self.tree = BusFixture().observe()
        for target, kwargs in [
            ('reader.time.monotonic', dict(side_effect=lambda: self.clock.now)),
            ('reader.time.sleep', dict(side_effect=self.clock.sleep)),
            ('reader.proc_identity', dict(side_effect=lambda pid: self.actual_identity.copy())),
            ('reader.subprocess.run', dict(side_effect=self.tool)),
        ]:
            mocked = patch(target, **kwargs); mocked.start(); self.addCleanup(mocked.stop)
        self.atspi = patch('reader.observe_atspi', side_effect=lambda *a, **kw: copy.deepcopy(self.tree)).start()
        self.addCleanup(patch.stopall)
        self.addCleanup(lambda: self.r.log.close() if self.r.log and not self.r.log.closed else None)

    def tool(self, command, **kwargs):
        self.commands.append((command, kwargs['timeout']))
        code = 0; out = ''; err = ''
        if self.fail_command and command[0] == self.fail_command:
            code = 1; err = 'genuine tool failure'
        elif command[0] == 'import':
            path = Path(command[-1]); self.captures.append(path)
            path.write_text('fresh frame %d' % len(self.captures))
        elif command[0] == 'identify':
            color = self.colors.pop(0) if len(self.colors) > 1 else self.colors[0]
            out = self.metadata if self.metadata is not None else '%d %d %d' % (*self.rect[2:], color)
        elif command[1] == 'search':
            self.searches += 1
            if self.searches <= self.empty_searches:
                code = 1
            else:
                out = '\n'.join(self.ids); code = 0 if out else 1
        elif command[1] == 'getwindowpid':
            out = self.window_pid
        elif command[1] == 'getwindowname':
            out = 'Anvil'
        elif command[1] == 'getwindowgeometry':
            self.bind_reads += 1
            if self.move_after_capture and self.bind_reads >= 2:
                self.rect[0] = 10
            out = 'WINDOW=%s\nX=%d\nY=%d\nWIDTH=%d\nHEIGHT=%d\nSCREEN=0\n' % (command[-1], *self.rect)
        elif command[1] == 'getactivewindow':
            out = self.active
        else:
            raise AssertionError('Unexpected tool: ' + repr(command))
        return reader.subprocess.CompletedProcess(command, code, out, err)

    def observe(self):
        return self.r.observe('open', control=True)

    def test_missing_root_and_changed_tree_each_discard_and_recapture_whole_frame(self):
        self.atspi.side_effect = [observer.ObservationNotReady('root absent'),
            observer.ObservationNotReady('tree changed'), copy.deepcopy(self.tree)]
        with patch('reader.time.time_ns', side_effect=[101, 102, 103]):
            result = self.observe()
        self.assertEqual([p.name for p in self.captures], ['001-open.png', '002-open.png', '003-open.png'])
        self.assertEqual(result['evidence'], '003-open')
        self.assertEqual(result['screenshot'], '003-open.png')
        self.assertEqual(result['captured_at_ns'], 103)
        self.assertEqual((self.out / result['screenshot']).read_text(), 'fresh frame 3')
        self.assertFalse((self.out / '001-open.json').exists())
        self.assertFalse((self.out / '002-open.json').exists())
        report = json.loads((self.out / 'readiness-001-open.json').read_text())
        self.assertEqual([a['evidence'] for a in report['attempts']], ['001-open', '002-open'])
        self.assertEqual(self.searches, 4)
        self.assertEqual(self.atspi.call_count, 3)
        self.assertEqual(self.r.coverage['fields'], {})
        self.assertEqual(self.r.inputs, [])

    def test_only_flat_frames_wait_and_tree_guards_still_run_for_black_frames(self):
        self.colors = [1, 2]
        result = self.observe()
        self.assertEqual(result['evidence'], '002-open')
        self.assertEqual(self.atspi.call_count, 2)
        self.assertEqual(self.searches, 4)
        self.assertEqual(self.clock.sleeps, [.2])
        self.colors = [1]
        self.atspi.side_effect = RuntimeError('Foreign accessibility edge')
        with self.assertRaisesRegex(RuntimeError, 'Foreign'):
            self.observe()
        self.assertEqual(len(self.captures), 3)
        self.assertEqual(self.clock.sleeps, [.2])

    def test_uniform_frames_exhaust_without_any_coverage(self):
        self.colors = [1]
        with self.assertRaisesRegex(RuntimeError, 'readiness bound exhausted'):
            self.observe()
        self.assertEqual(len(self.captures), reader.OBSERVATION_ATTEMPTS)
        self.assertEqual(self.r.coverage['fields'], {})
        self.assertEqual(self.r.inputs, [])

    def test_hard_tree_dbus_and_catalog_errors_never_retry(self):
        for error in [RuntimeError('Foreign accessibility edge'), RuntimeError('Ambiguous PID-owned accessibility roots'),
                      RuntimeError('Cyclic accessibility tree'), RuntimeError('catalog empty'),
                      ValueError('D-Bus owner vanished'), RuntimeError('Accessibility PID changed')]:
            with self.subTest(error=error):
                before = len(self.captures); calls = self.atspi.call_count
                self.atspi.side_effect = error
                with self.assertRaises(type(error)):
                    self.observe()
                self.assertEqual(len(self.captures), before + 1)
                self.assertEqual(self.atspi.call_count, calls + 1)
        self.assertEqual(self.clock.sleeps, [])

    def test_transient_attempt_limit_does_not_depend_on_time_advancing(self):
        self.atspi.side_effect = observer.ObservationNotReady('root absent')
        with patch('reader.time.sleep') as sleep, self.assertRaisesRegex(RuntimeError, 'bound exhausted'):
            self.observe()
        self.assertEqual(self.atspi.call_count, reader.OBSERVATION_ATTEMPTS)
        self.assertEqual(sleep.call_count, reader.OBSERVATION_ATTEMPTS - 1)
        self.assertEqual(self.r.deadline, 400)
        self.assertIsNone(self.r.observation_deadline)

    def test_local_time_limit_stops_slow_transients_before_attempt_limit(self):
        def slow(*args, **kwargs):
            self.clock.now += 8
            raise observer.ObservationNotReady('tree changed')
        self.atspi.side_effect = slow
        with self.assertRaisesRegex(RuntimeError, 'readiness deadline exhausted'):
            self.observe()
        self.assertEqual(self.atspi.call_count, 2)
        self.assertEqual(self.r.deadline, 400)

    def test_remaining_app_budget_bounds_tools_atspi_and_retry_sleep(self):
        self.r.deadline = 100.1
        self.atspi.side_effect = observer.ObservationNotReady('root absent')
        with self.assertRaisesRegex(RuntimeError, 'bound exhausted'):
            self.observe()
        self.assertEqual(self.clock.sleeps, [])
        self.assertEqual(self.atspi.call_args.kwargs['deadline'], 100.1)
        self.assertTrue(all(0 < timeout <= .100001 for _, timeout in self.commands))
        self.assertEqual(self.r.deadline, 100.1)

    def test_expired_app_budget_after_transient_fails_before_wait(self):
        self.r.deadline = 100.1
        def expired(*args, **kwargs):
            self.clock.now += .2
            raise observer.ObservationNotReady('root absent')
        self.atspi.side_effect = expired
        with self.assertRaisesRegex(RuntimeError, '300-second budget exhausted'):
            self.observe()
        self.assertEqual(self.atspi.call_count, 1)
        self.assertEqual(self.clock.sleeps, [])

    def test_host_and_process_errors_after_transient_are_hard(self):
        for failure in ('host', 'birth', 'executable', 'state'):
            with self.subTest(failure=failure):
                self.actual_identity = self.identity.copy(); self.host.check.side_effect = None
                before = len(self.captures)
                def changed(*args, **kwargs):
                    if failure == 'host':
                        self.host.check.side_effect = RuntimeError('host changed')
                    else:
                        field = {'birth': 'start_ticks', 'executable': 'executable', 'state': 'state_dir'}[failure]
                        self.actual_identity[field] = 'changed'
                    raise observer.ObservationNotReady('root absent')
                self.atspi.side_effect = changed
                with self.assertRaisesRegex(RuntimeError, 'changed'):
                    self.observe()
                self.assertEqual(len(self.captures), before + 1)
        self.assertEqual(self.clock.sleeps, [])

    def test_missing_or_inactive_window_can_settle_without_input(self):
        self.empty_searches = 2
        result = self.observe()
        self.assertEqual(result['evidence'], '001-open')
        self.assertEqual(self.searches, 4)
        self.assertEqual(self.clock.sleeps, [.2, .2])
        self.assertEqual(self.r.inputs, [])
        self.active = '543'
        def activate(seconds):
            self.clock.sleep(seconds); self.active = '4194307'
        with patch('reader.time.sleep', side_effect=activate):
            result = self.observe()
        self.assertEqual(result['evidence'], '002-open')
        self.assertEqual(self.r.inputs, [])

    def test_ambiguous_foreign_and_malformed_window_are_hard(self):
        for failure in ('ambiguous', 'foreign', 'malformed'):
            with self.subTest(failure=failure):
                self.ids = ['4194307']; self.window_pid = '21'
                if failure == 'ambiguous':
                    self.ids.append('4194308')
                else:
                    self.window_pid = '99' if failure == 'foreign' else 'not a pid'
                before = self.searches
                with self.assertRaises(RuntimeError) as caught:
                    self.observe()
                self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
                self.assertEqual(self.searches, before + 1)
        self.assertEqual(self.captures, [])
        self.assertEqual(self.clock.sleeps, [])

    def test_window_movement_discards_old_screenshot(self):
        self.move_after_capture = True
        result = self.observe()
        self.assertEqual(result['evidence'], '002-open')
        self.assertEqual(result['window_rect'], [10, 0, 640, 480])
        captures = [cmd for cmd, _ in self.commands if cmd[0] == 'import']
        self.assertEqual(captures[0][-2], '640x480+0+0')
        self.assertEqual(captures[1][-2], '640x480+10+0')

    def test_genuine_tool_failures_and_bad_image_metadata_are_hard(self):
        for command in ('xdotool', 'import', 'identify'):
            self.fail_command = command
            with self.subTest(command=command), self.assertRaisesRegex(RuntimeError, 'genuine tool failure'):
                self.observe()
        self.fail_command = None
        for metadata in ('bad', '640 480 0', '639 480 2'):
            self.metadata = metadata
            with self.subTest(metadata=metadata), self.assertRaises(RuntimeError):
                self.observe()
        self.assertEqual(self.atspi.call_count, 0)
        self.assertEqual(self.clock.sleeps, [])

    def test_startup_uses_first_observation_readiness_not_fixed_paint_sleep(self):
        with patch('reader.sha256', return_value='a'*64), patch('reader.safe_config'), \
             patch('reader.subprocess.Popen', return_value=self.process):
            self.r.launch(1)
        self.assertEqual(self.clock.sleeps, [])
        self.assertEqual(self.r.process_identity, self.identity)
        self.atspi.side_effect = [observer.ObservationNotReady('not registered'), copy.deepcopy(self.tree)]
        self.assertEqual(self.observe()['evidence'], '002-open')
        self.assertEqual(self.clock.sleeps, [.2])
        self.assertEqual(self.r.deadline, 400)


if __name__ == '__main__':
    unittest.main()
