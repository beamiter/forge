#!/usr/bin/env python3
"""AccessKit INVALID range sentinel and adversarial replies, no real UI/provider."""
import json
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import observer
import reader
from test_atspi_geometry import DBusError, GeometryBus, proof
from test_atspi_window_coordinates import WindowGeometryBus
from test_dbus_signatures import Int32, UInt32
from test_reader import node, plan


class RangeReplyBus(GeometryBus):
    def __init__(self):
        super().__init__()
        self.target = None
        self.range_reply = [-1, -1, -1, -1]
        self.later_reply = None
        self.character_count = None
        self.role = 29

    def endpoint(self, owner, path, interface):
        endpoint = super().endpoint(owner, path, interface)
        fixture = self
        class Endpoint:
            def __getattr__(self, method):
                def call(*args, timeout):
                    result = getattr(endpoint, method)(*args, timeout=timeout)
                    if path == '/button' and method == 'GetRole':
                        return fixture.role
                    if method == 'Get' and args == (observer.TEXT, 'CharacterCount'):
                        return result if fixture.character_count is None else fixture.character_count
                    if method == 'GetRangeExtents':
                        return (fixture.later_reply if args[0] and fixture.later_reply is not None
                                else fixture.range_reply)
                    return result
                return call
        return Endpoint()


class RangeUnavailableTests(unittest.TestCase):
    def test_exact_signed_sentinel_is_unknown_not_hidden_or_ocr_proof(self):
        for reply in ([-1]*4, tuple(Int32(-1) for _ in range(4))):
            bus = RangeReplyBus(); bus.range_reply = reply
            callback = Mock(return_value=proof())
            tree = bus.observe(text_geometry=callback, frame_id='frame-1')
            node = observer.validate_tree(tree)['/button']
            with self.subTest(reply=reply):
                self.assertEqual(node['visible_text'], '')
                self.assertEqual(node['text_geometry_status'], 'unknown')
                self.assertEqual(node['text_geometry']['unproven_lines'], [0, 1])
                self.assertEqual(node['text_geometry']['unavailable_ranges'], [
                    dict(index=0,start=0,end=7,coordinates=0,reply=[-1]*4),
                    dict(index=1,start=8,end=18,coordinates=0,reply=[-1]*4)])
                self.assertEqual(tree['unknown_text'][0]['status'], 'range_unavailable')
                self.assertEqual(tree['unknown_text'][0]['clip'], bus.geometry)
                self.assertTrue(observer.text_geometry_unknown_in_scope(tree, [0,0,640,480]))
                self.assertTrue(reader.status_text_unknown(tree,node))
                tree['scoped_nodes'] = tree['nodes']
                self.assertTrue(reader.unknown_notices('ember',tree,plan()))
                self.assertNotIn(bus.text, tree['visible_text'])
                callback.assert_not_called()

    def test_unavailable_text_is_not_field_evidence_or_a_label_input_target(self):
        p = plan(); repo = p['repositories'][0]
        for text in (p['id']+' revision '+str(p['revision']),
                     repo['identity']['full_name']+'\nCandidate: '+repo['candidate'],
                     reader.INSPECT['ember']):
            bus = RangeReplyBus(); bus.text = text; bus.names['/button'] = text
            tree = bus.observe(); tree['scoped_nodes'] = tree['nodes']
            self.assertFalse(reader.bound_identity(tree['visible_text'],p))
            self.assertEqual(reader.bound_candidates(tree['visible_text'],p),set())
            subject = SimpleNamespace(app='ember',plan=p)
            self.assertEqual(reader.Reader.targets(subject,tree,'inspect'),[])

    def test_named_button_is_discoverable_but_unknown_header_blocks_safe_action(self):
        bus = RangeReplyBus(); bus.role = 43
        bus.text = bus.names['/button'] = reader.INSPECT['ember']
        tree = bus.observe(); button = observer.validate_tree(tree)['/button']
        # A real role-43 name plus Component rectangle remains a public target.
        # It is not visible-text/field proof. Here the unknown range intersects
        # the actual refresh/catalog notice strip, so the safety gates block.
        tree['nodes'][0]['children'].extend(['/refresh','/selected'])
        tree['nodes'].extend([
            node('/refresh','/app',owner=bus.owner,name=reader.REFRESH['ember'],
                 rect=[10,0,180,10],visible=True),
            node('/selected','/app',owner=bus.owner,name=plan()['title']+' · Not inspected',
                 rect=[10,90,180,20],visible=True)])
        tree.update(scoped_nodes=tree['nodes'],mission_content_rect=[0,0,640,480],
                    window_rect=[0,0,640,480],window='42',evidence='unknown-header')
        subject = SimpleNamespace(app='ember',plan=plan(),mission_open=True,
            coverage={'stages':{}},click=Mock(),observe=Mock(return_value=tree),
            locate_ember=Mock(return_value=(tree,button)))
        subject.targets = lambda observation,purpose: reader.Reader.targets(subject,observation,purpose)
        self.assertEqual(len(subject.targets(tree,'inspect')),1)
        self.assertEqual(button['visible_text'],'')
        self.assertTrue(reader.unknown_notices('ember',tree,subject.plan))
        with self.assertRaisesRegex(RuntimeError,'notice is busy or unknown'):
            reader.Reader.press(subject,'inspect')
        subject.click.assert_not_called()
        with patch.object(reader.time,'sleep'),self.assertRaisesRegex(RuntimeError,'did not become visibly settled'):
            reader.Reader.wait_inspection_settled(subject)
        self.assertNotIn('inspection_ready',subject.coverage['stages'])
        subject.click.assert_not_called()

    def test_mixed_ranges_preserve_only_actual_inside_line_and_unknown(self):
        bus = RangeReplyBus(); bus.range_reply = [20,20,100,12]
        bus.later_reply = [-1]*4
        tree = bus.observe(); node = observer.validate_tree(tree)['/button']
        self.assertEqual(node['visible_text'],'Mission')
        self.assertEqual(node['text_geometry']['unproven_lines'],[1])
        self.assertEqual(tree['unknown_text'][0]['unproven_lines'],[1])

    def test_later_not_supported_after_sentinel_is_hard_even_before_malformed_third_line(self):
        for text in ('Mission\nrevision 1','Mission\nrevision 1\nmalformed third range'):
            bus = RangeReplyBus(); bus.text = text
            # Third line would be malformed if queried. The second-line
            # capability inconsistency must already reject the whole frame.
            bus.later_reply = [20,20,-1,12]
            callback = Mock(return_value=proof()); error = DBusError()
            def later_unsupported(interface,method,args):
                if method == 'GetRangeExtents' and args[0] == 8:
                    raise error
            bus.extra_error = later_unsupported
            with self.subTest(text=text),self.assertRaises(DBusError) as caught:
                bus.observe(text_geometry=callback,frame_id='frame-1')
            self.assertIs(caught.exception,error)
            self.assertNotIsInstance(error,observer.ObservationNotReady)
            entry = error.atspi_diagnostics[-1]
            self.assertEqual(entry['error_name'],observer.NOT_SUPPORTED)
            self.assertEqual(entry['frame_id'],'frame-1')
            self.assertEqual(entry['clip'],bus.geometry)
            self.assertEqual(entry['arguments'][0],dict(value=8,type='Int32'))
            self.assertEqual(entry['unavailable_ranges'],[
                dict(index=0,start=0,end=7,coordinates=0,reply=[-1]*4)])
            self.assertEqual(sum(m=='GetRangeExtents' for _,m,*_ in bus.wire_calls),2)
            callback.assert_not_called()

    def test_initial_not_supported_retains_existing_capability_fallback(self):
        bus = GeometryBus(); callback = Mock(return_value=proof())
        tree = bus.observe(text_geometry=callback,frame_id='frame-1')
        self.assertIn(bus.text,tree['visible_text'])
        self.assertEqual(tree['unknown_text'],[])
        self.assertEqual(observer.validate_tree(tree)['/button']['text_geometry']['status'],'ocr_verified')
        callback.assert_called_once()

    def test_zero_area_and_clipped_supported_ranges_never_become_proof(self):
        for rect in ([20,20,0,12], [20,20,100,0], [20,20,0,0], [9,20,100,12],
                     [20,79,100,12], [-100,-100,30,12]):
            bus = RangeReplyBus(); bus.range_reply = rect; callback = Mock()
            tree = bus.observe(text_geometry=callback,frame_id='frame-1')
            self.assertEqual(observer.validate_tree(tree)['/button']['visible_text'],'')
            self.assertEqual(tree['unknown_text'],[])
            callback.assert_not_called()

    def test_zero_area_or_outside_node_is_not_queried(self):
        for rect in ([20,20,0,12], [20,20,100,0], [800,800,100,12]):
            bus = RangeReplyBus(); bus.geometry = rect; callback = Mock()
            tree = bus.observe(text_geometry=callback,frame_id='frame-1')
            self.assertEqual(tree['unknown_text'],[])
            self.assertNotIn('GetRangeExtents',[m for _,m,*_ in bus.wire_calls])
            callback.assert_not_called()

    def test_every_other_malformed_reply_is_hard_and_diagnosed(self):
        class Boolean(int):pass
        invalid = [None, [-1]*3, [-1]*5, '-1,-1,-1,-1', [-1.,-1,-1,-1],
                   ['-1',-1,-1,-1], [Boolean(-1),-1,-1,-1], [True,-1,-1,-1],
                   [UInt32(0),-1,-1,-1], [-1,-1,-1,0], [0,0,-1,-1],
                   [20,20,-1,12], [20,20,100,-1], [2**31,20,100,12],
                   [-2**31-1,20,100,12]]
        for raw in invalid:
            bus = RangeReplyBus(); bus.range_reply = raw; callback = Mock()
            with self.subTest(raw=raw), self.assertRaisesRegex(RuntimeError,'Malformed AT-SPI Text range') as caught:
                bus.observe(text_geometry=callback,frame_id='bad-frame')
            error = caught.exception
            self.assertNotIsInstance(error,observer.ObservationNotReady)
            entry = error.atspi_diagnostics[-1]
            self.assertEqual((entry['path'],entry['interface'],entry['method']),
                             ('/button',observer.TEXT,'GetRangeExtents'))
            self.assertEqual(entry['arguments'],[dict(value=0,type='Int32'),
                dict(value=7,type='Int32'),dict(value=0,type='UInt32')])
            self.assertEqual(entry['frame_id'],'bad-frame')
            self.assertEqual(entry['clip'],bus.geometry)
            self.assertEqual(entry['reply_repr'],repr(raw))
            self.assertEqual(entry['reply_type'],type(raw).__name__)
            result = {}; reader.record_failure(result,error)
            json.dumps(result,allow_nan=False)
            callback.assert_not_called()

    def test_sentinel_does_not_hide_bad_later_range_or_count(self):
        bus = RangeReplyBus(); bus.later_reply = [20,20,-1,12]
        with self.assertRaisesRegex(RuntimeError,'Malformed AT-SPI Text range'):bus.observe()
        for count in (0, len('Mission\nrevision 1')-1, len('Mission\nrevision 1')+1):
            bus = RangeReplyBus(); bus.character_count = count
            with self.assertRaisesRegex(RuntimeError,'CharacterCount'):bus.observe()
            self.assertNotIn('GetRangeExtents',[m for _,m,*_ in bus.wire_calls])

    def test_component_invalid_is_still_a_hard_error_with_raw_diagnostic(self):
        bus = RangeReplyBus(); bus.geometry = [-1]*4
        with self.assertRaisesRegex(RuntimeError,'Malformed AT-SPI Component') as caught:bus.observe()
        entry = caught.exception.atspi_diagnostics[-1]
        self.assertEqual(entry['method'],'GetExtents')
        self.assertEqual(entry['reply_repr'],repr([-1]*4))

    def test_malformed_reply_diagnostic_is_bounded_and_success_is_not_logged(self):
        bus = RangeReplyBus(); bus.range_reply = ['private malformed data'*100000]*100
        with self.assertRaisesRegex(RuntimeError,'Malformed AT-SPI Text range') as caught:bus.observe()
        self.assertLess(len(caught.exception.atspi_diagnostics[-1]['reply_repr']),2049)
        bus = RangeReplyBus(); bus.range_reply = [20,20,100,12]
        self.assertEqual(bus.observe()['atspi_diagnostics'],[])

    def test_sentinel_rechecks_geometry_text_tree_and_owner(self):
        for change in ('geometry','text','tree','owner'):
            bus = RangeReplyBus()
            if change=='geometry':bus.final_geometry=[11,10,180,70]
            if change=='text':bus.latest_text='Mission\nrevision 2'
            if change=='tree':bus.latest_children={'/app':[],'/button':[]}
            if change=='owner':bus.final_pid=22
            with self.subTest(change=change),self.assertRaises(RuntimeError):bus.observe()
        bus = RangeReplyBus(); bus.latest_children={'/app':[(':1.8','/evil')],'/button':[]}
        with self.assertRaisesRegex(RuntimeError,'Foreign'):bus.observe()

    def test_late_bus_failure_is_not_downgraded_by_sentinel(self):
        bus = RangeReplyBus(); calls=[]; error=DBusError('org.freedesktop.DBus.Error.AccessDenied')
        def late(interface,method,args):
            if method=='GetText':
                calls.append(method)
                if len(calls)==2:raise error
        bus.extra_error=late
        with self.assertRaises(DBusError) as caught:bus.observe()
        self.assertIs(caught.exception,error)

    def test_window_sentinel_is_not_projected_or_given_ocr(self):
        bus = WindowGeometryBus(); bus.range_error=None; bus.range_rect=tuple(Int32(-1) for _ in range(4))
        callback = Mock(return_value=bus.proof())
        tree = bus.observe(text_geometry=callback,frame_id='frame-1')
        node = observer.validate_tree(tree)['/button']
        self.assertEqual(node['visible_text'],'')
        self.assertEqual(node['text_geometry']['unavailable_ranges'][0]['coordinates'],1)
        self.assertEqual(tree['unknown_text'][0]['clip'],[134,99,180,70])
        callback.assert_not_called()


if __name__ == '__main__':
    unittest.main()
