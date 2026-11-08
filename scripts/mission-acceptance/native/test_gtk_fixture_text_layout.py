#!/usr/bin/env python3
"""Fixture layout and retained OCR gates; no GTK, display, bus or provider."""
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, call, patch

import test_gtk_geometry_smoke as smoke
from test_dbus_signatures import Int32, UInt32

import observer
from test_gtk_geometry_smoke import configure_fixture_text, VISIBLE, WRAPPED


def word(text,left,top,width,conf=95,line=1):
    return dict(text=text,left=left,top=top,width=width,height=15,conf=conf,
                page_num='1',block_num='1',par_num='1',line_num=str(line))


class FixtureTextLayoutTests(unittest.TestCase):
    def test_layout_configuration_moves_ink_and_prevents_descendant_focus_only(self):
        entry=Mock();view=Mock();wrapped=Mock();center=object()
        configure_fixture_text(entry,view,wrapped,center)
        self.assertEqual(entry.mock_calls,[call.set_can_focus(False),call.set_alignment(0.5)])
        self.assertEqual(view.mock_calls,[call.set_can_focus(False),call.set_left_margin(8),call.set_right_margin(8)])
        self.assertEqual(wrapped.mock_calls,[call.set_xalign(0.5),call.set_justify(center)])

    def test_original_entry_caret_confidence_and_edge_still_fail_closed(self):
        # Reproduce the actual 33ef2db CI word boxes/confidence, not invented
        # acceptable evidence. The production threshold/margin stay unchanged.
        rows=[word('ENTRY',33,73,65,58.765163),word('GEOMETRY',105,77,110,96.352425),
              word('VISIBLE',221,77,75,95.908089)]
        rows[0]['height']=24
        result=observer.verify_ocr_text_geometry(VISIBLE[1],[33,69,592,34],
                    dict(frame_id='captured',rows=rows),'captured')
        self.assertEqual(result['reason'],'OCR word is missing, malformed or low-confidence')
        rows[0]['conf']=95
        result=observer.verify_ocr_text_geometry(VISIBLE[1],[33,69,592,34],
                    dict(frame_id='captured',rows=rows),'captured')
        self.assertEqual(result['reason'],'OCR word is clipped or has invalid geometry')
        self.assertEqual(result['visible_text'],'')

    def test_original_textview_boundary_is_unknown_and_real_inset_is_required(self):
        rows=[word('TEXTVIEW',24,126,102,91.19809),word('GEOMETRY',134,126,110,96.205383),
              word('VISIBLE',250,126,75,95.94381)]
        original=copy.deepcopy(rows)
        result=observer.verify_ocr_text_geometry(VISIBLE[2],[24,122,592,50],
                    dict(frame_id='captured',rows=rows),'captured')
        self.assertEqual(result['reason'],'OCR word is clipped or has invalid geometry')
        self.assertEqual(result['visible_text'],'')
        self.assertEqual(rows,original)  # Verification must not repair raw boxes.
        # A separate synthetic layout with actual eight-pixel ink inset passes.
        inset=[dict(row,left=row['left']+8) for row in rows]
        result=observer.verify_ocr_text_geometry(VISIBLE[2],[24,122,592,50],
                    dict(frame_id='inset-layout',rows=inset),'inset-layout')
        self.assertEqual(result['status'],'ocr_verified')

    def test_high_confidence_interior_wrapped_lines_reach_non_joining_rejection(self):
        rows=[]
        lines=('WRAPPED GEOMETRY MUST','KEEP EACH PHYSICAL LINE',
               'SEPARATE WITHOUT JOINING','THE LOGICAL SENTENCE')
        for line_number,line in enumerate(lines,1):
            left=34
            for text in line.split():
                width=len(text)*8
                rows.append(word(text,left,242+line_number*24,width,line=line_number))
                left+=width+8
        clip=[24,262,296,96];inner=[25,263,294,94]
        self.assertTrue(all(row['conf']>=80 and observer.inside(
            [row[k] for k in ('left','top','width','height')],inner) for row in rows))
        self.assertEqual(' '.join(lines),WRAPPED)
        result=observer.verify_ocr_text_geometry(WRAPPED,clip,dict(frame_id='wrapped',rows=rows),'wrapped')
        self.assertEqual(result['reason'],'OCR line is not an exact unique complete text line')
        self.assertEqual(result['visible_text'],'')
        self.assertEqual(result['unproven_lines'],[0])


def allocation(width=640):
    return [dict(path='0',type='GtkWindow',allocation=[0,0,width,650],content_size=[width,650],
                 realized=True,visible=True,mapped=True)]


def readiness_record(pid=None):
    return dict(schema=1,status='ready',pid=os.getpid() if pid is None else pid,
                frame_ids=[7,8,9],allocations=allocation())


class FixtureReadinessTests(unittest.TestCase):
    def test_only_three_consecutive_distinct_after_paint_samples_become_ready(self):
        state=smoke.FixturePaintReadiness()
        self.assertIsNone(state.sample(7,allocation()))
        self.assertIsNone(state.sample(7,allocation()))
        self.assertIsNone(state.sample(8,allocation()))
        result=state.sample(9,allocation())
        self.assertEqual(result,readiness_record())
        self.assertEqual(state.sample_count,3)

    def test_allocation_change_or_missing_frame_resets_stability(self):
        state=smoke.FixturePaintReadiness()
        for frame,width in ((1,600),(2,600),(3,640),(4,640)):
            self.assertIsNone(state.sample(frame,allocation(width)))
        self.assertEqual(state.sample(5,allocation())['frame_ids'],[3,4,5])
        self.assertIsNone(state.sample(7,allocation()))
        self.assertIsNone(state.sample(8,allocation()))
        self.assertEqual(state.sample(9,allocation())['frame_ids'],[7,8,9])

    def test_continuous_layout_changes_and_bad_frame_counter_fail_closed(self):
        state=smoke.FixturePaintReadiness()
        for frame in range(240):self.assertIsNone(state.sample(frame,allocation(600+frame%2)))
        with self.assertRaisesRegex(RuntimeError,'240 frames'):state.sample(240,allocation())
        state=smoke.FixturePaintReadiness();state.sample(7,allocation())
        for bad in (6,True,-1,7.0):
            with self.subTest(bad=bad),self.assertRaisesRegex(RuntimeError,'counter'):state.sample(bad,allocation())

    def test_readiness_pid_version_shape_geometry_and_mapping_are_strict(self):
        variants=[]
        for key,value in (('pid',True),('pid',os.getpid()+1),('schema',True),('schema',2),
                          ('status','pending'),('frame_ids',[1,1,2]),('frame_ids',[1,2,True]),
                          ('allocations',[])):
            variants.append(dict(readiness_record(),**{key:value}))
        for key,value in (('mapped',False),('path','1'),('content_size',[0,650]),
                          ('allocation',[0,0,640.0,650]),('realized',1)):
            record=readiness_record();record['allocations'][0][key]=value;variants.append(record)
        record=readiness_record();record['unexpected']=1;variants.append(record)
        for record in variants:
            with self.subTest(record=record),self.assertRaises(RuntimeError):
                smoke.validate_fixture_ready(record,os.getpid())

    def test_private_atomic_handshake_is_bound_to_the_owned_process(self):
        with tempfile.TemporaryDirectory(prefix='gtk-geometry-smoke-') as directory:
            ready=Path(directory).resolve()/'fixture-ready.json';record=readiness_record()
            with patch.object(smoke.os,'replace',wraps=os.replace) as replace:
                smoke.write_fixture_ready(ready,record)
            replace.assert_called_once_with(ready.with_name('.fixture-ready.pending.json'),ready)
            self.assertEqual(ready.stat().st_mode & 0o777,0o600)
            self.assertFalse(ready.with_name('.fixture-ready.pending.json').exists())
            process=Mock(pid=os.getpid());process.poll.return_value=None
            with patch.object(smoke.time,'monotonic',return_value=100):
                self.assertEqual(smoke.wait_fixture_ready(ready,process,101),record)
                process.pid+=1
                with self.assertRaisesRegex(RuntimeError,'PID'):smoke.wait_fixture_ready(ready,process,101)
            with self.assertRaisesRegex(RuntimeError,'already exists'):smoke.write_fixture_ready(ready,record)

    def test_handshake_missing_timeout_exit_oversize_and_symlink_never_accept(self):
        with tempfile.TemporaryDirectory(prefix='gtk-geometry-smoke-') as directory:
            ready=Path(directory).resolve()/'fixture-ready.json'
            process=Mock(pid=os.getpid());process.poll.return_value=None
            with patch.object(smoke.time,'monotonic',return_value=101),patch.object(smoke.time,'sleep') as sleep:
                with self.assertRaisesRegex(RuntimeError,'deadline'):smoke.wait_fixture_ready(ready,process,101)
                sleep.assert_not_called()
            process.poll.return_value=1
            with self.assertRaisesRegex(RuntimeError,'exited'):smoke.wait_fixture_ready(ready,process,999999)
            process.poll.return_value=None
            ready.write_bytes(b'x'*(smoke.READY_LIMIT+1));ready.chmod(0o600)
            with patch.object(smoke.time,'monotonic',return_value=100):
                with self.assertRaisesRegex(RuntimeError,'oversized'):smoke.wait_fixture_ready(ready,process,101)
            ready.unlink();target=ready.with_name('other.json');target.write_text(json.dumps(readiness_record()))
            ready.symlink_to(target)
            with patch.object(smoke.time,'monotonic',return_value=100):
                with self.assertRaises(OSError):smoke.wait_fixture_ready(ready,process,101)

    def test_fifo_readiness_is_rejected_without_waiting_for_a_writer(self):
        # A bounded subprocess makes a regression to blocking open fail the
        # test instead of hanging the entire suite before its deadline check.
        with tempfile.TemporaryDirectory(prefix='gtk-geometry-smoke-') as directory:
            ready=Path(directory).resolve()/'fixture-ready.json'
            os.mkfifo(ready,0o600)
            script="""import os,sys,time
from pathlib import Path
from types import SimpleNamespace
import test_gtk_geometry_smoke as smoke
process=SimpleNamespace(pid=os.getpid(),poll=lambda:None)
try:
    smoke.wait_fixture_ready(Path(sys.argv[1]),process,time.monotonic()+1)
except RuntimeError as error:
    if str(error)!='Unsafe or oversized fixture readiness record':raise
else:
    raise AssertionError('FIFO was accepted as a readiness record')
"""
            completed=subprocess.run([sys.executable,'-B','-c',script,str(ready)],
                cwd=Path(smoke.__file__).parent,text=True,capture_output=True,timeout=5)
            self.assertEqual(completed.returncode,0,completed.stdout+completed.stderr)

    def test_handshake_directory_and_filename_cannot_escape_private_fixture(self):
        with tempfile.TemporaryDirectory(prefix='gtk-geometry-smoke-') as directory:
            root=Path(directory).resolve();ready=root/'fixture-ready.json'
            for path in (root/'other.json',Path('fixture-ready.json')):
                with self.subTest(path=path),self.assertRaises(RuntimeError):smoke.fixture_ready_location(path)
            root.chmod(0o755)
            with self.assertRaisesRegex(RuntimeError,'private'):smoke.fixture_ready_location(ready)


class FixtureExtentTraceTests(unittest.TestCase):
    def inputs(self):
        raw=tuple(Int32(v) for v in [0,0,592,34])
        endpoint=SimpleNamespace(bus_name=':1.7',object_path='/entry',GetExtents=Mock(return_value=raw))
        interface=Mock(return_value=endpoint);module=SimpleNamespace(Interface=interface)
        return module,interface,endpoint,raw,dict(events=[],errors=[],omitted=0)

    def test_trace_preserves_typed_arguments_timeout_return_identity_and_interface(self):
        module,interface,endpoint,raw,trace=self.inputs();obj=object();coordinate=UInt32(1)
        with smoke.trace_fixture_extents(module,trace,'initial_atspi','initial-0'):
            actual=module.Interface(obj,observer.COMPONENT).GetExtents(coordinate,timeout=.25)
            self.assertIs(actual,raw)
        self.assertIs(module.Interface,interface);interface.assert_called_once_with(obj,observer.COMPONENT)
        endpoint.GetExtents.assert_called_once_with(coordinate,timeout=.25)
        self.assertIs(endpoint.GetExtents.call_args.args[0],coordinate)
        event=trace['events'][0]
        self.assertEqual(event['arguments'],[dict(value=1,type='UInt32')])
        self.assertEqual(event['rect'],[0,0,592,34]);self.assertEqual(event['value_types'],['Int32']*4)
        self.assertEqual((event['owner'],event['path'],event['phase'],event['frame_id']),
                         (':1.7','/entry','initial_atspi','initial-0'))
        self.assertEqual(trace['errors'],[]);json.dumps(trace)

    def test_non_component_interfaces_and_malformed_replies_are_not_modified(self):
        module,interface,endpoint,raw,trace=self.inputs();bad=object()
        with smoke.trace_fixture_extents(module,trace,'ocr_atspi','frame'):
            self.assertIs(module.Interface(object(),observer.TEXT),endpoint)
            endpoint.GetExtents.return_value=bad
            self.assertIs(module.Interface(object(),observer.COMPONENT).GetExtents(UInt32(0),timeout=1),bad)
        self.assertEqual(len(trace['events']),1);json.dumps(trace)

    def test_original_bus_exception_survives_recording_and_restores_interface(self):
        module,interface,endpoint,raw,trace=self.inputs();error=RuntimeError('actual D-Bus failure')
        endpoint.GetExtents.side_effect=error
        with self.assertRaises(RuntimeError) as caught:
            with smoke.trace_fixture_extents(module,trace,'initial_atspi','initial-0'):
                module.Interface(object(),observer.COMPONENT).GetExtents(UInt32(1),timeout=1)
        self.assertIs(caught.exception,error);self.assertIs(module.Interface,interface)
        self.assertEqual(trace['events'][0]['error'],'actual D-Bus failure')
        endpoint.GetExtents.assert_called_once()

    def test_trace_metadata_failure_cannot_mask_the_actual_exception(self):
        module,interface,endpoint,raw,trace=self.inputs();error=RuntimeError('primary read failure')
        class BrokenMetadata:
            object_path='/entry'
            GetExtents=Mock(side_effect=error)
            @property
            def bus_name(self):raise ValueError('diagnostic metadata unavailable')
        endpoint=BrokenMetadata();interface.return_value=endpoint
        with self.assertRaises(RuntimeError) as caught:
            with smoke.trace_fixture_extents(module,trace,'initial_atspi','initial-0'):
                module.Interface(object(),observer.COMPONENT).GetExtents(UInt32(1),timeout=1)
        self.assertIs(caught.exception,error);self.assertIs(module.Interface,interface)
        self.assertTrue(trace['errors']);endpoint.GetExtents.assert_called_once()

    def test_trace_bound_omits_diagnostics_without_extra_calls_or_reply_changes(self):
        module,interface,endpoint,raw,trace=self.inputs()
        with patch.object(smoke,'TRACE_LIMIT',1),smoke.trace_fixture_extents(module,trace,'initial_atspi','initial-0'):
            for coord in (0,1):
                self.assertIs(module.Interface(object(),observer.COMPONENT).GetExtents(UInt32(coord),timeout=1),raw)
        self.assertEqual(len(trace['events']),1);self.assertEqual(trace['omitted'],1)
        self.assertEqual(endpoint.GetExtents.call_count,2)

    def test_dimensions_failure_is_not_retried_converted_or_swallowed(self):
        module,interface,endpoint,raw,trace=self.inputs()
        report=dict(component_trace=trace)
        error=RuntimeError('SCREEN and WINDOW component dimensions differ')
        with patch.object(observer,'observe_atspi',side_effect=error) as observe,patch.object(smoke.time,'sleep') as sleep:
            with self.assertRaises(RuntimeError) as caught:
                smoke.fixture_observation(module,report,'initial_atspi','initial-0',21,[0,0,640,650],deadline=101)
        self.assertIs(caught.exception,error);self.assertIs(module.Interface,interface)
        observe.assert_called_once_with(21,[0,0,640,650],deadline=101);sleep.assert_not_called()
        self.assertEqual(report['phase'],'initial_atspi')


if __name__=='__main__':unittest.main()
