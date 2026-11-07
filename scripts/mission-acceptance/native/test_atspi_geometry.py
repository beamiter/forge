#!/usr/bin/env python3
"""Capability/geometry regressions; no apps, real buses or providers are run."""
import copy
import unittest
from unittest.mock import Mock, patch

import observer
from test_dbus_signatures import SignatureBus


class DBusError(RuntimeError):
    def __init__(self, name=observer.NOT_SUPPORTED, message=''):
        super().__init__(message)
        self.name = name
    def get_dbus_name(self):
        return self.name
    def get_dbus_message(self):
        return str(self)


class GeometryBus(SignatureBus):
    def __init__(self):
        super().__init__()
        self.module.DBusException = DBusError
        self.text = 'Mission\nrevision 1'
        self.error = DBusError()
        self.target = (observer.TEXT, 'GetRangeExtents')
        self.wire_error = self.inject
        self.extra_error = None
        self.geometry = [10, 10, 180, 70]
        self.final_geometry = None
        self.extent_reads = 0
    def inject(self, interface, method, args):
        if self.extra_error:
            self.extra_error(interface, method, args)
        if (interface, method) == self.target:
            raise self.error
    def endpoint(self, owner, path, interface):
        endpoint = super().endpoint(owner, path, interface)
        fixture = self
        class Endpoint:
            def __getattr__(self, method):
                def call(*args, timeout):
                    result = getattr(endpoint, method)(*args, timeout=timeout)
                    if method == 'GetExtents':
                        fixture.extent_reads += 1
                        return (fixture.final_geometry if fixture.extent_reads > 2
                                and fixture.final_geometry is not None else fixture.geometry)
                    return result
                return call
        return Endpoint()


def rows(lines=('Mission', 'revision 1')):
    result = []
    for number, line in enumerate(lines, 1):
        x = 20
        for word in line.split(' '):
            width = max(5, len(word)*7)
            result.append(dict(text=word, conf=95.0, left=x, top=number*20,
                               width=width, height=12, page_num='1', block_num='1',
                               par_num='1', line_num=str(number)))
            x += width+5
    return result


def proof(lines=('Mission', 'revision 1'), frame='frame-1'):
    return dict(frame_id=frame, rows=rows(lines))


class OCRGeometryTests(unittest.TestCase):
    def verify(self, candidate=None, text='Mission\nrevision 1', clip=None):
        return observer.verify_ocr_text_geometry(text, clip or [10, 10, 180, 70],
                                                candidate or proof(), 'frame-1')
    def test_complete_actual_lines_are_exact_and_record_word_geometry(self):
        result = self.verify()
        self.assertEqual(result['status'], 'ocr_verified')
        self.assertEqual(result['visible_text'], 'Mission\nrevision 1')
        self.assertEqual(result['unproven_lines'], [])
        self.assertEqual(result['lines'][1]['words'][1]['text'], '1')
    def test_complete_subset_retains_explicit_unknown_lines(self):
        result = self.verify(proof(('Mission',)))
        self.assertEqual(result['status'], 'ocr_partial')
        self.assertEqual(result['visible_text'], 'Mission')
        self.assertEqual(result['unproven_lines'], [1])
    def test_wrong_or_missing_frame_is_hard(self):
        for candidate in (proof(frame='old'), {'rows': rows()}, None):
            with self.subTest(candidate=candidate), self.assertRaisesRegex(RuntimeError, 'different frame'):
                observer.verify_ocr_text_geometry('Mission', [0,0,100,100], candidate, 'frame-1')
        with self.assertRaisesRegex(RuntimeError, 'frame identity'):
            observer.verify_ocr_text_geometry('Mission', [0,0,100,100], proof(), None)
    def test_low_confidence_or_bad_geometry_never_drops_words_to_repair(self):
        for key, value in [('conf',79.9), ('conf',float('nan')), ('conf',True),
                           ('width',0), ('width',1.5), ('left',10), ('top',10),
                           ('left',-10), ('height',1000), ('text','revision\n1')]:
            candidate = proof(); candidate['rows'][0][key] = value
            with self.subTest(key=key,value=value):
                result = self.verify(candidate)
                self.assertEqual(result['visible_text'], '')
                self.assertEqual(result['status'], 'unavailable')
    def test_no_substring_normalization_duplicate_or_fabricated_lines(self):
        for candidate, text in [(proof(('Miss',)), 'Mission\nrevision 1'),
                                (proof(('Mission', 'revision1')), 'Mission\nrevision 1'),
                                (proof(('Mission',)), 'Mission\nMission'),
                                (proof(('Mission','Mission')), 'Mission\nrevision 1'),
                                (proof(('revision 1','Mission')), 'Mission\nrevision 1'),
                                (proof(('Mission','Unknown notice')), 'Mission\nrevision 1')]:
            with self.subTest(candidate=candidate,text=text):
                self.assertEqual(self.verify(candidate,text)['visible_text'], '')
    def test_line_local_indentation_and_whitespace_keep_every_character(self):
        text = '  {"candidate":   "abc123"},\n    "revision": 1'
        candidate = proof(('{"candidate": "abc123"},', '"revision": 1'))
        self.assertEqual(self.verify(candidate, text)['status'], 'ocr_verified')
        for candidate in (proof(('{"candidate" "abc123"},', '"revision": 1')),
                          proof(('{"candidate": "abc124"},', '"revision": 1')),
                          proof(('{"candidate":', '"abc123"},'))):
            self.assertEqual(self.verify(candidate,text)['visible_text'],'')

    def test_overlapping_words_lines_and_split_vertical_groups_reject(self):
        for mode in ('words','lines','vertical'):
            candidate = proof()
            if mode == 'words': candidate['rows'][2]['left'] = 21
            if mode == 'lines':
                for row in candidate['rows'][1:]: row['top'] = 22
            if mode == 'vertical': candidate['rows'][2]['top'] = 55
            self.assertEqual(self.verify(candidate)['visible_text'], '')
    def test_excessive_rows_and_bad_line_identifier_are_hard(self):
        candidate=proof(); candidate['rows'][0]['line_num']=None
        with self.assertRaisesRegex(RuntimeError,'line identity'): self.verify(candidate)
        with self.assertRaisesRegex(RuntimeError,'excessive'):
            self.verify(dict(frame_id='frame-1',rows=[{}]*8193))


class ObserverGeometryTests(unittest.TestCase):
    def test_only_exact_not_supported_on_range_is_optional_and_diagnostic(self):
        bus=GeometryBus(); diagnostics=[]
        tree=bus.observe(diagnostics=diagnostics)
        node=next(n for n in tree['nodes'] if n['path']=='/button')
        self.assertTrue(node['visible'])  # A real typed button remains a control.
        self.assertEqual(node['visible_text'],'')  # Its name is not text proof.
        self.assertEqual(node['text_geometry_status'],'unknown')
        self.assertEqual(tree['unknown_text'][0]['path'],'/button')
        self.assertEqual(tree['unknown_text'][0]['clip'],bus.geometry)
        d=diagnostics[0]
        self.assertEqual((d['interface'],d['method'],d['error_name']),
                         (observer.TEXT,'GetRangeExtents',observer.NOT_SUPPORTED))
        self.assertEqual(d['expected_signature'],'iiu')
        self.assertEqual([a['type'] for a in d['arguments']],['Int32','Int32','UInt32'])
        self.assertEqual(d['node']['role'],43)
        self.assertEqual(d['owner'],bus.owner)
        self.assertEqual(sum(method=='GetRangeExtents' for _,method,*_ in bus.wire_calls),1)
        self.assertGreaterEqual(sum(method=='GetText' for _,method,*_ in bus.wire_calls),2)
    def test_callback_never_receives_expected_text_and_proves_only_same_frame(self):
        bus=GeometryBus(); callback=Mock(return_value=proof())
        tree=bus.observe(text_geometry=callback,frame_id='frame-1')
        self.assertIn(bus.text,tree['visible_text'])
        self.assertEqual(tree['unknown_text'],[])
        self.assertEqual(callback.call_args.args,('/button',bus.geometry))
        self.assertEqual(set(callback.call_args.kwargs),{'deadline'})
    def test_partial_proof_is_not_notice_free_and_scope_excludes_terminal(self):
        bus=GeometryBus()
        tree=bus.observe(text_geometry=lambda *a,**kw:proof(('Mission',)),frame_id='frame-1')
        self.assertEqual(len(observer.text_geometry_unknown_in_scope(tree,[0,0,640,480],{'/button'})),1)
        self.assertEqual(observer.text_geometry_unknown_in_scope(tree,[300,200,40,40]),[])
        self.assertEqual(observer.text_geometry_unknown_in_scope(tree,[0,0,640,480],{'/mission'}),[])
    def test_other_methods_and_all_other_bus_errors_propagate_unchanged(self):
        for target in bus_targets():
            bus=GeometryBus(); bus.target=target
            with self.subTest(target=target),self.assertRaises(DBusError) as caught:
                bus.observe(text_geometry=lambda *a,**kw:proof(),frame_id='frame-1')
            self.assertIs(caught.exception,bus.error)
            self.assertTrue(caught.exception.atspi_diagnostics)
        for name in ('AccessDenied','InvalidArgs','NoReply','Disconnected','UnknownMethod','NotSupportedExtra'):
            bus=GeometryBus(); bus.error=DBusError('org.freedesktop.DBus.Error.'+name)
            with self.subTest(name=name),self.assertRaises(DBusError) as caught: bus.observe()
            self.assertIs(caught.exception,bus.error)
        bus=GeometryBus(); bus.error=RuntimeError(observer.NOT_SUPPORTED)
        with self.assertRaises(RuntimeError) as caught: bus.observe()
        self.assertIs(caught.exception,bus.error)
    def test_interface_construction_not_supported_is_not_range_capability(self):
        bus=GeometryBus();bus.target=None; original=bus.module.Interface
        error=DBusError(observer.NOT_SUPPORTED,'injected interface-construction failure')
        reads=[]
        def interface(obj, name):
            if name==observer.TEXT:
                reads.append(name)
                if len(reads)==2:raise error
            return original(obj,name)
        bus.module.Interface=interface
        callback=Mock(return_value=proof())
        with self.assertRaises(DBusError) as caught:
            bus.observe(text_geometry=callback,frame_id='frame-1')
        self.assertIs(caught.exception,error)
        self.assertEqual(error.atspi_diagnostics[-1]['method'],'Interface')
        callback.assert_not_called()

    def test_late_safety_error_wins_over_unsupported_and_transient_change(self):
        bus=GeometryBus(); bus.latest_names['/button']='Changed'
        error=DBusError('org.freedesktop.DBus.Error.AccessDenied')
        calls=[]
        def late(interface,method,args):
            if method=='GetText':
                calls.append(method)
                if len(calls)==2: raise error
        bus.extra_error=late
        with self.assertRaises(DBusError) as caught: bus.observe()
        self.assertIs(caught.exception,error)
    def test_tree_owner_text_and_geometry_changes_are_never_paired_with_old_ocr(self):
        for change in ('name','text','tree','owner','geometry'):
            bus=GeometryBus()
            if change=='name':bus.latest_names['/button']='Changed'
            if change=='text':bus.latest_text='Mission\nrevision 2'
            if change=='tree':bus.latest_children={'/app':[],'/button':[]}
            if change=='owner':bus.final_pid=22
            if change=='geometry':bus.final_geometry=[11,10,180,70]
            with self.subTest(change=change),self.assertRaises(RuntimeError):
                bus.observe(text_geometry=lambda *a,**kw:proof(),frame_id='frame-1')
    def test_foreign_edge_after_unsupported_remains_hard(self):
        bus=GeometryBus();bus.latest_children={'/app':[(':1.8','/evil')],'/button':[]}
        with self.assertRaisesRegex(RuntimeError,'Foreign'):bus.observe()
    def test_outside_clip_never_calls_text_geometry_or_range_extents(self):
        bus=GeometryBus();bus.geometry=[800,800,50,50]; callback=Mock()
        tree=bus.observe(text_geometry=callback,frame_id='frame-1')
        self.assertEqual(tree['unknown_text'],[])
        callback.assert_not_called()
        self.assertNotIn('GetRangeExtents',[m for _,m,*_ in bus.wire_calls])
    def test_supported_path_is_unchanged_and_never_calls_ocr(self):
        bus=GeometryBus();bus.target=None;callback=Mock()
        tree=bus.observe(text_geometry=callback,frame_id='frame-1')
        self.assertIn(bus.text,tree['visible_text']);callback.assert_not_called()
        self.assertEqual(tree['unknown_text'],[])
    def test_read_only_wire_allowlist_remains_exact(self):
        bus=GeometryBus();bus.observe()
        self.assertEqual(set(observer.READ_SIGNATURES),set(SignatureBus.signatures))
        self.assertTrue(all((i,m) in observer.READ_SIGNATURES for i,m,*_ in bus.wire_calls))


def bus_targets():
    return [target for target in SignatureBus.signatures if target!=(observer.TEXT,'GetRangeExtents')]


class EmberHeaderGeometryTests(unittest.TestCase):
    def tree(self):
        def node(path,parent,role,name,rect,children=()):
            return dict(path=path,parent=parent,role=role,name=name,rect=rect,
                children=list(children),owner=':1.8',pid=7,interfaces=[observer.COMPONENT],
                states=[(1<<25)|(1<<30),0])
        nodes=[node('/app',None,75,'App',[0,0,1200,850],['/window']),
               node('/window','/app',23,'Ember',[0,0,1200,850],['/mission']),
               node('/mission','/window',23,'Missions · delivery evidence',[230,180,700,430],['/bar','/close']),
               node('/bar','/mission',67,'',[238,191,655,30]),
               node('/close','/mission',43,'Close window',[893,199,14,14])]
        return dict(owner=':1.8',pid=7,root='/app',nodes=nodes)
    def scope(self,tree):
        return observer.mission_content_scope(tree,'Missions · delivery evidence',[0,0,1200,850])
    def test_exact_sibling_close_is_inside_the_owned_header_strip(self):
        tree=self.tree(); scope=self.scope(tree); nodes=observer.validate_tree(tree)
        self.assertEqual(scope['header_close'],'/close')
        self.assertEqual(scope['header_rect'],[230,180,700,41])
        bounds=observer.node_observation_bounds(nodes['/close'],nodes,[0,0,1200,850],scope)
        self.assertTrue(observer.visible(nodes['/close'],nodes,bounds))
    def test_body_partial_outside_and_other_names_get_no_header_privilege(self):
        for name,rect in [('Close window',[893,300,14,14]),('Close window',[893,215,14,14]),
                          ('Close',[893,199,14,14]),('Hide window',[893,199,14,14])]:
            tree=self.tree();tree['nodes'][-1].update(name=name,rect=rect)
            with self.subTest(name=name,rect=rect):self.assertIsNone(self.scope(tree)['header_close'])
    def test_foreign_parent_and_duplicate_close_cannot_gain_header_privilege(self):
        tree=self.tree();tree['nodes'][-1]['parent']='/window'
        with self.assertRaisesRegex(RuntimeError,'Foreign'):self.scope(tree)
        tree=self.tree();other=copy.deepcopy(tree['nodes'][-1]);other['path']='/close2'
        tree['nodes'].append(other);tree['nodes'][2]['children'].append('/close2')
        with self.assertRaisesRegex(RuntimeError,'Ambiguous'):self.scope(tree)
    def test_app_level_close_is_never_the_mission_header_close(self):
        tree=self.tree();tree['nodes'][2]['children'].remove('/close')
        tree['nodes'][1]['children'].append('/close');tree['nodes'][-1]['parent']='/window'
        self.assertIsNone(self.scope(tree)['header_close'])


if __name__=='__main__':
    unittest.main()
