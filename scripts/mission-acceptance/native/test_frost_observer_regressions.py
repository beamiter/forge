#!/usr/bin/env python3
"""Synthetic pixel/word negatives. These images are never native evidence."""
import copy
import io
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch
import reader
from test_frost_readout import make_reader


@unittest.skipUnless(shutil.which('convert') and shutil.which('tesseract'),'Existing image tools required')
class FrostMaskTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.out=Path(self.temp.name)
        self.r=make_reader(self.out)
        self.r.run=lambda args:subprocess.run(args,check=True,text=True,capture_output=True,timeout=15).stdout
    def tearDown(self):self.temp.cleanup()
    def picture(self,name,extra=()):
        p=self.out/(name+'.png')
        subprocess.run(['convert','-size','1200x850','xc:rgb(14,22,33)',
            '-fill','rgb(18,42,79)','-draw','rectangle 1178,103 1187,827',
            '-fill','rgb(68,126,255)','-draw','rectangle 1178,250 1187,361',
            *extra,str(p)],check=True,capture_output=True)
        return p
    def read(self,p):
        self.r.frost_scope=((self.r.process.pid,tuple(sorted(self.r.process_identity.items())),
                            self.r.window,tuple(self.r.window_rect)),[692,95,508,755])
        o=self.r.frost_readout(p,p.stem,[],regions=False)
        o.update(pid=123,window='42',window_rect=[200,175,1200,850],evidence=p.stem)
        return o
    def test_white_antialias_single_pixel_merges_luminance_but_not_chroma(self):
        p=self.picture('adjacent',['-fill','rgb(57,70,91)','-draw','point 1177,260'])
        o=self.read(p)
        lum=next(c for c in o['frost_components'] if c['rect']==[1177,250,11,112])
        self.assertAlmostEqual(lum['fill'],1121/1232)
        blue=next(c for c in o['frost_scroll_components'] if c['rect']==[1178,250,10,112])
        self.assertEqual(blue['fill'],1)
        with patch.object(self.r,'observe',return_value=o),patch.object(self.r,'bind_window'),patch.object(self.r,'run') as run,patch('reader.time.sleep'):
            self.r.scroll(4)
        self.assertEqual((self.r.inputs[0]['x'],self.r.inputs[0]['y']),(1383,481))
    def test_white_dark_rail_and_body_bars_cannot_become_a_thumb(self):
        p=self.picture('missing',['-fill','rgb(213,222,236)','-draw','rectangle 1178,250 1187,361',
                                   '-fill','rgb(68,126,255)','-draw','rectangle 100,250 109,361'])
        o=self.read(p)
        with patch.object(self.r,'observe',return_value=o),patch.object(self.r,'bind_window'),patch.object(self.r,'run') as run:
            with self.assertRaisesRegex(RuntimeError,'unique'):self.r.scroll(4)
        run.assert_not_called()
    def test_second_blue_bar_and_changed_scope_fail_closed(self):
        p=self.picture('ambiguous',['-fill','rgb(18,42,79)','-draw','rectangle 1150,103 1159,827',
                                    '-fill','rgb(68,126,255)','-draw','rectangle 1150,250 1159,361'])
        o=self.read(p)
        with patch.object(self.r,'observe',return_value=o),patch.object(self.r,'bind_window'),patch.object(self.r,'run') as run:
            with self.assertRaisesRegex(RuntimeError,'unique'):self.r.scroll(4)
        run.assert_not_called()
    def test_single_blue_body_stroke_without_a_longer_rail_is_not_a_scrollbar(self):
        p=self.picture('body-stroke',['-fill','rgb(14,22,33)','-draw','rectangle 1178,103 1187,827',
                                     '-fill','rgb(68,126,255)','-draw','rectangle 1150,250 1159,361'])
        o=self.read(p)
        with patch.object(self.r,'observe',return_value=o),patch.object(self.r,'bind_window'),patch.object(self.r,'run') as run:
            with self.assertRaisesRegex(RuntimeError,'unique'):self.r.scroll(4)
        run.assert_not_called()
    def test_incomplete_thumb_does_not_get_lower_fill_threshold(self):
        p=self.picture('hole',['-fill','rgb(18,42,79)','-draw','rectangle 1180,270 1184,340'])
        o=self.read(p)
        with patch.object(self.r,'observe',return_value=o),patch.object(self.r,'bind_window'),patch.object(self.r,'run') as run:
            with self.assertRaisesRegex(RuntimeError,'unique'):self.r.scroll(4)
        run.assert_not_called()
    def test_identity_uses_new_text_once_with_declared_scale_and_no_expected_value_input(self):
        new_id='mission:01234567-89ab-4cde-8012-3456789abcde'
        p=self.out/'synthetic-identity.png'
        subprocess.run(['convert','-size','1200x850','xc:rgb(14,22,33)','-font','DejaVu-Sans',
            '-pointsize','13','-fill','rgb(213,222,236)',
            '-draw',"text 693,280 'Mission ID: "+new_id+"' text 693,307 'Plan revision 1'",str(p)],check=True,capture_output=True)
        commands=[];run=self.r.run
        def record(args):commands.append(args);return run(args)
        self.r.run=record
        with patch('reader.frost_identity_crop',return_value=[681,258,519,62]):
            o=self.r.frost_readout(p,'new-identity',[])
        view=o['identity_views'][0]
        self.assertEqual(view['scale'],3);self.assertEqual(view['psm'],6)
        self.assertEqual(view['text'],'Mission ID: '+new_id+'\nPlan revision 1')
        self.assertGreaterEqual(min(w['conf'] for w in view['rows']),80)
        self.assertEqual(sum(c[0]=='tesseract' for c in commands),1)
        self.assertFalse(any(self.r.plan['id'] in str(c) for c in commands))
        self.assertFalse(reader.bound_identity(view['text'],self.r.plan))
        changed=dict(self.r.plan,id=new_id)
        self.assertTrue(reader.bound_identity(view['text'],changed))
        for bad in (new_id[:-1],new_id+'a',new_id.replace('0123','012?')):
            self.assertFalse(reader.bound_identity(view['text'].replace(new_id,bad),changed))
        low=copy.deepcopy(view['rows']);next(w for w in low if w['text'].startswith('mission:'))['conf']=79.9
        self.assertFalse(reader.bound_identity(reader.frost_lines(low,complete=True),changed))


class ReaderOcrCallbackTests(unittest.TestCase):
    def test_one_frame_cache_retains_low_confidence_and_edge_words_without_expected_text(self):
        with tempfile.TemporaryDirectory() as td:
            r=make_reader(Path(td));r.run=Mock(side_effect=['','unused tsv'])
            words=[dict(left=10,top=10,width=20,height=10,conf=95,text='observed'),
                   dict(left=29,top=10,width=20,height=10,conf=5,text='unknown')]
            cb=r.clip_text_geometry(Path('/synthetic-frame.png'),'frame-1',[200,175,1200,850])
            with patch('reader.ocr_rows',return_value=copy.deepcopy(words)):
                first=cb('/node',[210,185,25,20],deadline=float('inf'))
                second=cb('/other',[210,185,25,20],deadline=float('inf'))
            self.assertEqual(first,second);self.assertEqual(len(first['rows']),2)
            self.assertEqual(first['rows'][1]['conf'],5)
            self.assertEqual(first['rows'][1]['left'],229)
            self.assertEqual(r.run.call_count,2)
            self.assertEqual(first['frame_id'],'frame-1')


if __name__=='__main__':unittest.main()
