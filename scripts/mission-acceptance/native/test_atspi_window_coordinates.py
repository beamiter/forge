#!/usr/bin/env python3
"""GTK4.14 zero SCREEN origins and bounded WINDOW projection, no real bus/UI."""
import copy
import sys
import unittest
from unittest.mock import Mock, patch

import observer
from test_atspi_geometry import DBusError, proof
from test_dbus_signatures import Int32, SignatureBus, UInt32
import test_observation_readiness as readiness
import reader


class WindowGeometryBus(SignatureBus):
    def __init__(self):
        super().__init__()
        self.module.DBusException = DBusError
        self.children = {'/app': ['/frame'], '/frame': ['/box'], '/box': ['/button'], '/button': []}
        self.names = {path: '' for path in self.children}
        self.names['/frame'] = 'Observed client'
        self.roles = {'/app': 75, '/frame': 23, '/box': 39, '/button': 29}
        self.window = [110, 75, 640, 480]
        self.local = {'/frame': [0, 0, 640, 480], '/box': [24, 24, 592, 432],
                      '/button': [24, 24, 180, 70]}
        self.screen = {path: [0, 0, *rect[2:]] for path, rect in self.local.items()}
        self.latest_local = {}; self.latest_screen = {}
        self.text_nodes = {'/button'}
        self.range_error = DBusError()
        self.range_rect = [30, 30, 100, 20]
        self.wire_error = self.inject
        self.coordinate_calls = []
        self.late_error = None

    def inject(self, interface, method, args):
        if method == 'GetRangeExtents' and self.range_error is not None:
            raise self.range_error

    def endpoint(self, owner, path, interface):
        endpoint = super().endpoint(owner, path, interface)
        fixture = self
        class Endpoint:
            def __getattr__(self, method):
                def call(*args, timeout):
                    result = getattr(endpoint, method)(*args, timeout=timeout)
                    if owner == 'org.a11y.atspi.Registry':return result
                    if method == 'GetInterfaces':
                        return ([] if path == '/app' else [observer.COMPONENT]
                                + ([observer.TEXT] if path in fixture.text_nodes else []))
                    if method == 'GetRole':return fixture.roles[path]
                    if method == 'GetExtents':
                        coordinate = int(args[0])
                        fixture.coordinate_calls.append((path, coordinate, type(args[0])))
                        key = (owner, path, interface, method, args)
                        later = fixture.counts[key] > 1
                        if later and fixture.late_error is not None and coordinate == 1:
                            raise fixture.late_error
                        source = fixture.screen if coordinate == 0 else fixture.local
                        latest = fixture.latest_screen if coordinate == 0 else fixture.latest_local
                        return latest.get(path, source[path]) if later else source[path]
                    if method == 'GetRangeExtents':return fixture.range_rect
                    return result
                return call
        return Endpoint()

    def observe(self, **kwargs):
        kwargs.setdefault('window_binding', self.binding())
        with patch.dict(sys.modules, {'dbus': self.module}):
            return observer.observe_atspi(self.pid, self.window, **kwargs)

    def binding(self):
        return dict(window=4194307, pid=self.pid, title='Observed client', rect=self.window[:])

    def proof(self):
        candidate = proof()
        for row in candidate['rows']:
            row['left'] += self.window[0] + 14
            row['top'] += self.window[1] + 14
        return candidate


class WindowCoordinateTests(unittest.TestCase):
    def test_nonzero_screen_origin_keeps_actual_raw_and_projected_rectangles(self):
        bus = WindowGeometryBus(); callback = Mock(return_value=bus.proof())
        tree = bus.observe(text_geometry=callback, frame_id='frame-1')
        node = observer.validate_tree(tree)['/button']
        self.assertEqual(node['rect'], [134, 99, 180, 70])
        self.assertEqual(node['component_extents'],
                         {'screen': [0, 0, 180, 70], 'window': [24, 24, 180, 70]})
        self.assertEqual(tree['coordinate_geometry'], dict(mode='window_to_screen', anchor='/frame',
                         captured_window=[110, 75, 640, 480], offset=[110, 75], binding=bus.binding()))
        self.assertEqual(callback.call_args.args, ('/button', [134, 99, 180, 70]))
        self.assertEqual(set(callback.call_args.kwargs), {'deadline'})
        self.assertIn(bus.text, tree['visible_text'])
        self.assertEqual(tree['unknown_text'], [])
        self.assertTrue(all(kind is UInt32 for _, _, kind in bus.coordinate_calls))
        for path in bus.local:
            self.assertEqual([(p, c) for p, c, _ in bus.coordinate_calls if p == path],
                             [(path, 0), (path, 1), (path, 0), (path, 1)])
        ranges = [args for _, method, args, *_ in bus.wire_calls if method == 'GetRangeExtents']
        self.assertEqual([int(args[2]) for args in ranges], [1])
        self.assertEqual(set(observer.READ_SIGNATURES), set(SignatureBus.signatures))

    def test_zero_capture_origin_is_not_mistaken_for_node_origin(self):
        bus = WindowGeometryBus(); bus.window[:2] = [0, 0]
        tree = bus.observe(text_geometry=lambda *a, **kw:bus.proof(), frame_id='frame-1')
        self.assertEqual(observer.validate_tree(tree)['/button']['rect'], [24, 24, 180, 70])
        self.assertIn(bus.text, tree['visible_text'])

    def test_supported_text_extents_use_same_projection_and_still_recheck(self):
        bus = WindowGeometryBus(); bus.range_error = None; callback = Mock()
        tree = bus.observe(text_geometry=callback, frame_id='frame-1')
        self.assertIn(bus.text, tree['visible_text']); callback.assert_not_called()
        bus = WindowGeometryBus(); bus.range_error = None
        bus.latest_local['/button'] = [25, 24, 180, 70]
        with self.assertRaisesRegex(observer.ObservationNotReady, 'WINDOW geometry'):
            bus.observe()

    def test_raw_window_changes_are_not_hidden_by_constant_zero_screen(self):
        for path in ('/frame', '/box', '/button'):
            bus = WindowGeometryBus()
            bus.latest_local[path] = [bus.local[path][0] + 1, *bus.local[path][1:]]
            with self.subTest(path=path), self.assertRaisesRegex(observer.ObservationNotReady, 'WINDOW geometry'):
                bus.observe(text_geometry=lambda *a, **kw:bus.proof(), frame_id='frame-1')

    def test_raw_screen_change_is_rechecked_independently(self):
        bus = WindowGeometryBus(); bus.latest_screen['/button'] = [1, 0, 180, 70]
        with self.assertRaisesRegex(observer.ObservationNotReady, 'geometry'):
            bus.observe(text_geometry=lambda *a, **kw:bus.proof(), frame_id='frame-1')

    def test_observed_scrollbar_width_race_discards_the_entire_observation(self):
        # Replay the CI receipt: SCREEN width 7, then WINDOW width 6.
        # Only a second SCREEN read proves a temporal change, not a tolerated
        # cross-coordinate discrepancy. No text geometry may be accepted.
        bus = WindowGeometryBus(); callback = Mock()
        bus.screen['/button'] = [0, 0, 7, 50]
        bus.local['/button'] = [611, 192, 6, 50]
        bus.latest_screen['/button'] = [0, 0, 6, 50]
        with self.assertRaisesRegex(observer.ObservationNotReady, 'between SCREEN and WINDOW'):
            bus.observe(text_geometry=callback, frame_id='discarded')
        callback.assert_not_called()
        self.assertEqual([(p, c) for p, c, _ in bus.coordinate_calls if p == '/button'],
                         [('/button', 0), ('/button', 1), ('/button', 0), ('/button', 1)])

    def test_changed_window_coordinates_also_require_a_fresh_observation(self):
        bus = WindowGeometryBus(); callback = Mock()
        bus.local['/button'] = [24, 24, 179, 70]
        bus.latest_local['/button'] = [24, 24, 180, 70]
        with self.assertRaises(observer.ObservationNotReady):
            bus.observe(text_geometry=callback, frame_id='discarded')
        callback.assert_not_called()
        # A new, internally consistent capture is independently validated.
        stable = WindowGeometryBus()
        tree = stable.observe(text_geometry=lambda *a, **kw:dict(stable.proof(), frame_id='fresh'), frame_id='fresh')
        self.assertIn(stable.text, tree['visible_text'])
        self.assertEqual(observer.validate_tree(tree)['/button']['text_geometry']['frame_id'], 'fresh')

    def test_stable_dimension_disagreement_is_still_a_hard_failure(self):
        bus = WindowGeometryBus(); callback = Mock()
        bus.local['/button'][2] -= 1
        with self.assertRaisesRegex(RuntimeError, 'dimensions differ') as caught:
            bus.observe(text_geometry=callback, frame_id='discarded')
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
        callback.assert_not_called()
        self.assertEqual(len([p for p, _, _ in bus.coordinate_calls if p == '/button']), 4)

    def test_mismatch_recheck_does_not_convert_protocol_or_access_errors(self):
        for failure in ('malformed', 'denied'):
            bus = WindowGeometryBus(); callback = Mock()
            bus.local['/button'][2] -= 1
            if failure == 'malformed':bus.latest_screen['/button'] = [0, 0, -1, 70]
            else:
                bus.latest_screen['/button'] = [0, 0, 179, 70]
                bus.late_error = DBusError('org.freedesktop.DBus.Error.AccessDenied')
            with self.subTest(failure=failure), self.assertRaises(RuntimeError) as caught:
                bus.observe(text_geometry=callback, frame_id='discarded')
            self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
            if failure == 'denied':self.assertIs(caught.exception, bus.late_error)
            callback.assert_not_called()

    def test_mismatch_recheck_keeps_the_original_deadline(self):
        bus = WindowGeometryBus(); bus.local['/button'][2] -= 1
        now = [100.0]
        window_reads = [0]
        def expire_on_window(interface, method, args):
            if method == 'GetExtents' and int(args[0]) == 1:
                window_reads[0] += 1
                if window_reads[0] == 3:now[0] = 101.0
        bus.wire_error = expire_on_window
        with patch.object(observer.time, 'monotonic', side_effect=lambda:now[0]):
            with self.assertRaisesRegex(RuntimeError, 'deadline') as caught:
                bus.observe(deadline=100.5)
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
        self.assertEqual([(p, c) for p, c, _ in bus.coordinate_calls if p == '/button'],
                         [('/button', 0), ('/button', 1)])

    def test_deadline_expiring_between_rechecks_is_not_readiness(self):
        bus = WindowGeometryBus(); bus.local['/button'][2] -= 1
        bus.latest_screen['/button'] = [0, 0, 179, 70]
        now = [100.0]; screen_reads = [0]
        def expire_on_screen_recheck(interface, method, args):
            if method == 'GetExtents' and int(args[0]) == 0:
                screen_reads[0] += 1
                if screen_reads[0] == 4:now[0] = 101.0
        bus.wire_error = expire_on_screen_recheck
        with patch.object(observer.time, 'monotonic', side_effect=lambda:now[0]):
            with self.assertRaisesRegex(RuntimeError, 'deadline') as caught:
                bus.observe(deadline=100.5)
        self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
        self.assertEqual([(p, c) for p, c, _ in bus.coordinate_calls if p == '/button'],
                         [('/button', 0), ('/button', 1), ('/button', 0)])

    def test_nonzero_screen_path_is_unchanged_and_never_queries_window(self):
        bus = SignatureBus(); tree = bus.observe()
        self.assertNotIn('coordinate_geometry', tree)
        self.assertTrue(all(int(args[0]) == 0 for _, method, args, *_ in bus.wire_calls
                            if method == 'GetExtents'))
        self.assertIn(bus.text, tree['visible_text'])

    def test_clipping_ancestor_and_negative_child_offsets_stay_strict(self):
        bus = WindowGeometryBus(); bus.roles['/box'] = 49
        bus.local['/box'] = [24, 24, 100, 30]; bus.screen['/box'] = [0, 0, 100, 30]
        callback = Mock(return_value=bus.proof())
        tree = bus.observe(text_geometry=callback, frame_id='frame-1')
        self.assertEqual(callback.call_args.args[1], [134, 99, 100, 30])
        self.assertEqual(tree['unknown_text'][0]['clip'], [134, 99, 100, 30])
        self.assertEqual(observer.validate_tree(tree)['/button']['visible_text'], '')
        bus = WindowGeometryBus(); bus.local['/button'] = [-200, -100, 180, 70]
        callback = Mock(); tree = bus.observe(text_geometry=callback, frame_id='frame-1')
        self.assertFalse(observer.validate_tree(tree)['/button']['visible'])
        callback.assert_not_called()
        self.assertEqual(tree['unknown_text'], [])

    def test_nested_frame_container_is_not_a_second_native_toplevel(self):
        bus = WindowGeometryBus(); bus.roles['/box'] = 23
        tree = bus.observe(text_geometry=lambda *a, **kw:bus.proof(), frame_id='frame-1')
        self.assertIn(bus.text, tree['visible_text'])
        self.assertEqual(tree['coordinate_geometry']['anchor'], '/frame')

    def test_second_toplevel_dialog_window_or_sibling_component_is_ambiguous(self):
        for role in (23, 16, 69, 39):
            bus = WindowGeometryBus(); bus.children['/app'].append('/extra'); bus.children['/extra'] = []
            bus.names['/extra'] = 'Observed client'; bus.roles['/extra'] = role
            bus.local['/extra'] = [0, 0, 640, 480]; bus.screen['/extra'] = [0, 0, 640, 480]
            callback = Mock()
            with self.subTest(role=role), self.assertRaisesRegex(RuntimeError, 'ambiguous'):
                bus.observe(text_geometry=callback, frame_id='frame-1')
            callback.assert_not_called()
        for role in (16, 69):
            bus = WindowGeometryBus(); bus.roles['/box'] = role
            with self.subTest(nested_role=role), self.assertRaisesRegex(RuntimeError, 'boundary'):
                bus.observe()

    def test_missing_toplevel_or_mismatched_client_anchor_never_projects(self):
        for variant in ('role', 'screen_size', 'window_size', 'window_origin', 'child_size'):
            bus = WindowGeometryBus()
            if variant == 'role':bus.roles['/frame'] = 39
            if variant == 'screen_size':bus.screen['/frame'][2] -= 1
            if variant == 'window_size':bus.local['/frame'][2] -= 1
            if variant == 'window_origin':bus.local['/frame'][0] = 1
            if variant == 'child_size':bus.local['/button'][2] += 1
            callback = Mock()
            with self.subTest(variant=variant), self.assertRaises(RuntimeError):
                bus.observe(text_geometry=callback, frame_id='frame-1')
            callback.assert_not_called()

    def test_malformed_component_replies_fail_without_numeric_coercion(self):
        class Boolean(int):pass
        for raw in ([Boolean(1), 0, 180, 70], [UInt32(0), 0, 180, 70], [0, 0, 180], [0, 0, 180, 70, 0], [0, 0, True, 70],
                    [0, 0, 180.0, 70], [0, 0, '180', 70], [0, 0, -1, 70],
                    [2**31, 0, 180, 70], None):
            for coordinates in ('screen', 'local'):
                bus = WindowGeometryBus(); getattr(bus, coordinates)['/button'] = raw
                with self.subTest(raw=raw, coordinates=coordinates), self.assertRaisesRegex(RuntimeError, 'Malformed'):
                    bus.observe()

    def test_owner_tree_frame_and_safety_failures_cannot_accept_projected_ocr(self):
        for failure in ('pid', 'foreign', 'frame', 'safety'):
            bus = WindowGeometryBus()
            if failure == 'pid':bus.final_pid = 99
            if failure == 'foreign':
                bus.latest_children = copy.deepcopy(bus.children)
                bus.latest_children['/box'] = [(':1.99', '/foreign')]
            if failure == 'frame':callback = lambda *a, **kw:dict(bus.proof(), frame_id='old')
            else:callback = lambda *a, **kw:bus.proof()
            if failure == 'safety':bus.late_error = DBusError('org.freedesktop.DBus.Error.AccessDenied')
            with self.subTest(failure=failure), self.assertRaises(RuntimeError) as caught:
                bus.observe(text_geometry=callback, frame_id='frame-1')
            self.assertNotIsInstance(caught.exception, observer.ObservationNotReady)
            if failure == 'safety':self.assertIs(caught.exception, bus.late_error)

    def test_unique_observed_title_and_size_select_one_of_two_native_clients(self):
        bus = WindowGeometryBus()
        bus.children['/app'].append('/other'); bus.children['/other'] = ['/other_control']
        bus.children['/other_control'] = []
        bus.names.update({'/other': 'Other client', '/other_control': 'UNSELECTED ACTION'})
        bus.roles.update({'/other': 23, '/other_control': 43})
        bus.text_nodes.add('/other_control')
        bus.local.update({'/other': [0, 0, 640, 480], '/other_control': [24, 24, 180, 70]})
        bus.screen.update({'/other': [0, 0, 640, 480], '/other_control': [0, 0, 180, 70]})
        tree = bus.observe(text_geometry=lambda *a, **kw:bus.proof(), frame_id='frame-1')
        nodes = observer.validate_tree(tree)
        self.assertIn(bus.text, tree['visible_text'])
        self.assertNotIn('Other client', tree['visible_text'])
        self.assertNotIn('UNSELECTED ACTION', tree['visible_text'])
        for path in ('/other', '/other_control'):
            self.assertFalse(nodes[path]['coordinate_selected'])
            self.assertFalse(nodes[path]['visible'])
            self.assertEqual(nodes[path]['visible_text'], '')
            self.assertNotIn('rect', nodes[path])
            self.assertNotIn('text_geometry', nodes[path])
            self.assertEqual(nodes[path]['component_extents'], dict(screen=bus.screen[path], window=bus.local[path]))
        self.assertEqual(nodes['/other_control']['text'], bus.text)
        self.assertEqual(tree['unknown_text'], [])
        self.assertEqual(sum(method == 'GetRangeExtents' for _, method, *_ in bus.wire_calls), 1)
        bus.latest_local['/other_control'] = [25, 24, 180, 70]
        # Use a new read counter to simulate a change during a fresh capture.
        bus.counts.clear()
        with self.assertRaisesRegex(observer.ObservationNotReady, 'WINDOW geometry'):
            bus.observe()

    def test_missing_wrong_or_unbound_title_identity_is_never_inferred(self):
        bus = WindowGeometryBus()
        variants = [None, {}, dict(bus.binding(), pid=99), dict(bus.binding(), window=True),
                    dict(bus.binding(), window=0), dict(bus.binding(), title=''),
                    dict(bus.binding(), title='unobserved'), dict(bus.binding(), rect=[0, 0, 640, 480])]
        for binding in variants:
            bus = WindowGeometryBus(); callback = Mock()
            with self.subTest(binding=binding), self.assertRaises(RuntimeError):
                bus.observe(window_binding=binding, text_geometry=callback, frame_id='frame-1')
            callback.assert_not_called()

    def test_selected_accessible_title_drift_discards_capture(self):
        bus = WindowGeometryBus(); bus.latest_names['/frame'] = 'Changed client'
        with self.assertRaisesRegex(observer.ObservationNotReady, 'name'):
            bus.observe(text_geometry=lambda *a, **kw:bus.proof(), frame_id='frame-1')

    def test_window_read_errors_are_not_optional_geometry_capabilities(self):
        for name in (observer.NOT_SUPPORTED, 'org.freedesktop.DBus.Error.AccessDenied',
                     'org.freedesktop.DBus.Error.InvalidArgs', 'org.freedesktop.DBus.Error.NoReply'):
            bus = WindowGeometryBus(); error = DBusError(name); callback = Mock()
            def fail(interface, method, args):
                if method == 'GetExtents' and args == (1,):raise error
            bus.wire_error = fail
            with self.subTest(name=name), self.assertRaises(DBusError) as caught:
                bus.observe(text_geometry=callback, frame_id='frame-1')
            self.assertIs(caught.exception, error)
            self.assertEqual(error.atspi_diagnostics[-1]['method'], 'GetExtents')
            self.assertEqual(error.atspi_diagnostics[-1]['arguments'], [dict(type='UInt32', value=1)])
            callback.assert_not_called()

    def test_native_range_replies_are_strict_in_both_coordinate_modes(self):
        class Boolean(int):pass  # dbus.Boolean is an int subclass, not Python bool.
        invalid = [[30, 30, 100], [30, 30, 100, 20, 0], [True, 30, 100, 20],
                   [Boolean(1), 30, 100, 20], [UInt32(30), 30, 100, 20],
                   [30.0, 30, 100, 20], ['30', 30, 100, 20], [30, 30, -1, 20], None]
        for mode in ('screen', 'window'):
            for raw in invalid:
                bus = WindowGeometryBus(); bus.range_error = None; bus.range_rect = raw
                if mode == 'screen':
                    bus.screen = {p: observer.project_window_rect(r, bus.window) for p, r in bus.local.items()}
                with self.subTest(mode=mode, raw=raw), self.assertRaisesRegex(RuntimeError, 'Malformed AT-SPI Text range'):
                    bus.observe()
        bus = WindowGeometryBus(); bus.range_error = None
        bus.range_rect = tuple(Int32(v) for v in [30, 30, 100, 20])
        self.assertIn(bus.text, bus.observe()['visible_text'])

    def test_unknown_and_exact_ocr_gate_are_unchanged_after_projection(self):
        for kind in ('none', 'low_confidence', 'wrong_text', 'wrapped'):
            bus = WindowGeometryBus(); candidate = bus.proof()
            if kind == 'low_confidence':candidate['rows'][0]['conf'] = 79.9
            if kind == 'wrong_text':candidate['rows'][0]['text'] = 'Missing'
            if kind == 'wrapped':bus.text = 'Mission revision 1'
            callback = None if kind == 'none' else lambda *a, **kw:candidate
            tree = bus.observe(text_geometry=callback, frame_id='frame-1')
            node = observer.validate_tree(tree)['/button']
            with self.subTest(kind=kind):
                self.assertEqual(node['text_geometry_status'], 'unknown')
                self.assertEqual(node['visible_text'], '')
                self.assertEqual(tree['unknown_text'][0]['path'], '/button')


class ReaderWindowBindingTests(unittest.TestCase):
    setUp = readiness.ReaderReadinessTests.setUp

    def width_race(self, *args, **kwargs):
        bus = WindowGeometryBus()
        bus.screen['/button'] = [0, 0, 7, 50]
        bus.local['/button'] = [611, 192, 6, 50]
        bus.latest_screen['/button'] = [0, 0, 6, 50]
        return bus.observe(deadline=kwargs['deadline'])

    def test_proven_width_race_recaptures_without_publishing_old_evidence(self):
        def first_races(*args, **kwargs):
            if self.atspi.call_count == 1:return self.width_race(*args, **kwargs)
            return copy.deepcopy(self.tree)
        self.atspi.side_effect = first_races
        result = self.r.observe('width-race', control=True)
        self.assertEqual(len(self.captures), 2)
        self.assertEqual(result['evidence'], '002-width-race')
        self.assertFalse((self.out / '001-width-race.json').exists())
        self.assertEqual(self.r.inputs, [])

    def test_repeated_proven_width_race_exhausts_existing_attempt_bound(self):
        self.atspi.side_effect = self.width_race
        with self.assertRaisesRegex(RuntimeError, 'bound exhausted'):
            self.r.observe('width-race', control=True)
        self.assertEqual(self.atspi.call_count, reader.OBSERVATION_ATTEMPTS)
        self.assertEqual(len(self.captures), reader.OBSERVATION_ATTEMPTS)
        self.assertEqual(self.r.coverage['fields'], {})
        self.assertEqual(self.r.inputs, [])

    def tool(self, command, **kwargs):
        result = readiness.ReaderReadinessTests.tool(self, command, **kwargs)
        if command[1] == 'getwindowname':
            names = getattr(self, 'window_names', {})
            result.stdout = names.get(command[-1], 'Anvil')
        return result

    def test_selected_observed_xid_pid_title_and_rectangle_reach_observer(self):
        self.r.observe('open', control=True)
        self.assertEqual(self.atspi.call_args.kwargs['window_binding'],
                         dict(window=4194307, pid=21, title='Anvil', rect=[0, 0, 640, 480]))
        self.assertEqual(self.r.inputs, [])

    def test_title_drift_discards_old_frame_even_when_xid_pid_and_rect_match(self):
        def changed(*args, **kwargs):
            self.window_names = {'4194307': 'Anvil renamed'}
            return copy.deepcopy(self.tree)
        self.atspi.side_effect = changed
        result = self.r.observe('open', control=True)
        self.assertEqual(result['evidence'], '002-open')
        self.assertFalse((self.out / '001-open.json').exists())
        self.assertEqual(self.atspi.call_args_list[0].kwargs['window_binding']['title'], 'Anvil')
        self.assertEqual(self.atspi.call_args_list[1].kwargs['window_binding']['title'], 'Anvil renamed')
        self.assertEqual(len(self.captures), 2)

    def test_xid_drift_discards_frame_even_when_title_pid_and_rect_match(self):
        def changed(*args, **kwargs):
            self.ids = ['4194308']; self.active = '4194308'
            return copy.deepcopy(self.tree)
        self.atspi.side_effect = changed
        result = self.r.observe('open', control=True)
        self.assertEqual(result['evidence'], '002-open')
        self.assertFalse((self.out / '001-open.json').exists())
        self.assertEqual(self.atspi.call_args_list[0].kwargs['window_binding']['window'], 4194307)
        self.assertEqual(self.atspi.call_args_list[1].kwargs['window_binding']['window'], 4194308)
        self.assertEqual(len(self.captures), 2)

    def test_anvil_mission_binding_comes_from_filtered_client_not_main_window(self):
        self.ids = ['4194307', '4194308']; self.active = '4194308'
        self.window_names = {'4194307': 'Anvil', '4194308': reader.FRAME['anvil']}
        self.r.mission_open = True
        # Stop immediately after the binding has crossed into the observer.
        error = RuntimeError('stop after observing binding')
        self.atspi.side_effect = error
        with self.assertRaises(RuntimeError) as caught:self.r.observe('open', control=True)
        self.assertIs(caught.exception, error)
        self.assertEqual(self.atspi.call_args.kwargs['window_binding'],
                         dict(window=4194308, pid=21, title=reader.FRAME['anvil'], rect=[0, 0, 640, 480]))
        self.assertEqual(self.r.inputs, [])


if __name__ == '__main__':
    unittest.main()
