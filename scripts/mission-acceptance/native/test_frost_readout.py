#!/usr/bin/env python3
"""Offline replay of run 42245525353's public Mission pixels and original OCR.

The PNG is exactly the 520x750 public panel crop at (680,90) of 018-refresh;
no terminal, hostname or sidebar pixels are retained. The TSVs preserve only
that panel's original word rows in full-window coordinates. Reconstructing a
black surround below is test scaffolding, never native observation evidence.
No host, native process, X server, pointer input or provider is started here.
"""
import copy
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import observer
import reader
from test_reader import plan

FIXTURES = Path(__file__).with_name('fixtures')
MISSION = 'mission:cd7a257c-d33b-4d9a-a0c3-1cdf60de3187'


def captured(name='controls'):
    return observer.ocr_rows((FIXTURES / ('frost-mission-'+name+'.tsv')).read_text())


def make_reader(out=Path('/unused')):
    p = plan(); p.update(id=MISSION, title='Shared native Mission', revision=1)
    r = reader.Reader('frost', {}, {}, p, None, out)
    r.window = '42'; r.window_rect = [200, 175, 1200, 850]
    r.process = SimpleNamespace(pid=123); r.mission_open = True
    r.process_identity = dict(pid=123, start_ticks='456', executable='/owned/frost', state_dir='/owned/state')
    return r


class FrostReadoutGeometryTests(unittest.TestCase):
    def test_captured_failure_is_not_repaired_or_accepted(self):
        rows = captured()
        self.assertEqual(observer.phrases(rows, 'Refresh'), [])
        self.assertEqual(observer.phrases(rows, 'Back'), [])
        wrong = next(r for r in rows if r['text'].startswith('mission:'))
        self.assertEqual(wrong['text'], MISSION.replace('cd7', 'cd?'))
        self.assertGreater(wrong['conf'], 80)
        self.assertFalse(reader.bound_identity(reader.frost_lines(rows), make_reader().plan))
        original = next(r for r in captured('fields') if r['text'].startswith('mission:'))
        self.assertEqual(original['text'], MISSION)
        self.assertLess(original['conf'], 80)

    def test_identity_crop_uses_unique_current_labels_not_expected_id(self):
        rows = captured()
        self.assertEqual(reader.frost_identity_crop(rows, [200,175,1200,850]), [682,259,518,59])
        for text in ('Mission', 'ID:', 'Plan', 'revision'):
            mutated = copy.deepcopy(rows)
            word = next(r for r in mutated if r['text'] == text and 260 <= r['top'] <= 310)
            word['conf'] = 79.99
            self.assertIsNone(reader.frost_identity_crop(mutated, [200,175,1200,850]))
        rows += [dict(r, block_num='duplicate', top=r['top']+100) for r in rows
                 if 269 <= r['top'] <= 280]
        self.assertIsNone(reader.frost_identity_crop(rows, [200,175,1200,850]))

    def test_panel_anchor_rejects_body_copy_and_missing_or_low_confidence_heading(self):
        rows = captured()
        self.assertEqual(reader.frost_panel_scope(rows, [200,175,1200,850]), [692,95,508,755])
        for mutation in ('missing', 'low', 'left', 'duplicate'):
            altered = copy.deepcopy(rows)
            heading = next(r for r in altered if r['text'] == 'Missions')
            if mutation == 'missing': altered.remove(heading)
            if mutation == 'low': heading['conf'] = 79
            if mutation == 'left': heading['left'] = 200
            if mutation == 'duplicate': altered.append(dict(heading, block_num='duplicate', top=400))
            with self.subTest(mutation=mutation):
                self.assertIsNone(reader.frost_panel_scope(altered, [200,175,1200,850]))

    def test_identity_header_rejects_body_copy_or_missing_title_filter_and_heading(self):
        rows = captured()
        for absent in ('Title', 'Missions', 'repositories'):
            self.assertIsNone(reader.frost_identity_crop([r for r in rows if r['text'] != absent],
                                                         [200,175,1200,850]))
        changed = copy.deepcopy(rows)
        for r in changed:
            if 269 <= r['top'] <= 307:
                r['top'] += 300
        self.assertIsNone(reader.frost_identity_crop(changed, [200,175,1200,850]))

    def test_complete_lines_cannot_bridge_low_nonfinite_or_out_of_range_words(self):
        words = [r for r in captured() if r['block_num'] == '44']
        self.assertEqual(reader.frost_lines(words, complete=True), 'Plan revision 1')
        for confidence in (79.999, float('nan'), float('inf'), 101):
            changed = copy.deepcopy(words); changed[1]['conf'] = confidence
            self.assertEqual(reader.frost_lines(changed, complete=True), '')

    def test_large_flat_component_scientific_area_is_valid_image_metadata(self):
        header = 'Objects (id: bounding-box centroid area mean-color):\n'
        black = '  0: 1200x850+0+0 599.5,424.5 1.02e+06 gray(0)'
        self.assertEqual(reader.frost_components(header+black, [200,175,1200,850]), [])
        self.assertEqual(reader.frost_components(header+black.replace('gray(0)', 'gray(255)'),
                                               [200,175,1200,850]),
                         [dict(rect=[0,0,1200,850], fill=1.0)])

    def test_malformed_or_outside_image_component_is_hard_failure(self):
        header = 'Objects (id: bounding-box centroid area mean-color):\n'
        for row in ('junk', '  1: 10x111+1198+103 1182.5,158.0 1106 gray(255)',
                    *('  1: 10x111+1178+103 1182.5,158.0 '+area+' gray(255)'
                      for area in ('1200', '1e9999', 'nan', 'inf', '-1', '1..2'))):
            with self.assertRaises(RuntimeError):
                reader.frost_components(header+row, [200,175,1200,850])


@unittest.skipUnless(shutil.which('convert') and shutil.which('tesseract'),
                     'Offline image replay requires the existing ImageMagick/Tesseract tools')
class FrostPublicPixelReplayTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(); cls.out = Path(cls.temp.name)
        cls.frame = cls.out/'public-panel-replay.png'
        # Preserve only actual public pixels; outside this crop is irrelevant
        # black test scaffolding. Set a virtual X11 page offset deliberately.
        subprocess.run(['convert', str(FIXTURES/'frost-mission-panel.png'),
            '-background', 'black', '-gravity', 'northwest', '-splice', '680x90',
            '-extent', '1200x850', '-set', 'page', '1600x1200+200+175', str(cls.frame)], check=True)
        cls.reader = make_reader(cls.out)
        cls.commands = []
        def run(command):
            cls.commands.append(command)
            return subprocess.run(command, capture_output=True, text=True, timeout=15, check=True).stdout
        cls.reader.run = run
        cls.observation = cls.reader.frost_readout(cls.frame, 'replay', captured())
        cls.observation.update(evidence='replay',window='42',window_rect=[200,175,1200,850],pid=123)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def test_actual_pixels_recover_exact_toolbar_and_identity_without_confidence_change(self):
        o = self.observation
        self.assertEqual(self.reader.targets(o, 'refresh'),
                         [dict(label='Refresh',rect=[903,343,52,11],node=None)])
        self.assertEqual(self.reader.targets(o, 'close_mission'),
                         [dict(label='Back',rect=[983,343,32,11],node=None)])
        self.assertEqual(len(self.reader.targets(o, 'inspect')), 1)
        view = o['identity_views'][0]
        self.assertEqual(view['text'], 'Mission ID: '+MISSION+'\nPlan revision 1')
        self.assertTrue(reader.bound_identity(view['text'], self.reader.plan))
        self.assertGreaterEqual(min(r['conf'] for r in view['rows']), 80)
        self.assertTrue(all('--psm' in c and c[c.index('--psm')+1] in ('6','7')
                            for c in self.commands if c[0] == 'tesseract'))
        self.assertTrue(all(c.index('+repage') < c.index('-crop') for c in self.commands if '-crop' in c))

    def test_actual_component_measurement_preserves_exact_large_area(self):
        flat = self.out/'flat-1203x853.png'
        subprocess.run(['convert', '-size', '1203x853', 'xc:white', str(flat)], check=True)
        r = make_reader(self.out); r.window_rect = [200,175,1203,853]
        measured = []
        def run(command):
            raw = self.reader.run(command)
            measured.append((command, raw))
            return raw
        r.run = run
        o = r.frost_readout(flat, 'flat-precision', [], regions=False)
        self.assertEqual(o['frost_components'], [dict(rect=[0,0,1203,853], fill=1.0)])
        self.assertEqual(len(measured), 3)
        command, raw = measured[0]
        self.assertEqual(command[command.index('-precision')+1], '16')
        self.assertIn(' 1026159 gray(255)', raw)
        # Default six-digit formatting can round 1026159 upward to 1026160.
        # Preserve the strict bound rather than accepting that rounded area.
        with self.assertRaisesRegex(RuntimeError, 'component escaped captured window'):
            reader.frost_components(raw.replace(' 1026159 ', ' 1.02616e+06 '), r.window_rect)

    def test_absorb_binds_identity_only_from_one_complete_observed_view(self):
        r = make_reader(); r.absorb(self.observation, 'initial')
        self.assertEqual(r.coverage['identity_bindings'], [dict(stage='initial',evidence='replay')])
        self.assertEqual(r.coverage['candidate_bindings'], {})
        self.assertEqual(r.coverage['core_pin_bindings'], {})
        self.assertIn('inspected.identity_revision_binding', reader.missing_coverage('frost',r.plan,r.coverage))
        changed = copy.deepcopy(self.observation)
        changed['identity_views'][0]['text'] = changed['identity_views'][0]['text'].replace('cd7','cd?')
        r = make_reader(); r.absorb(changed, 'initial')
        self.assertEqual(r.coverage['identity_bindings'], [])

    def test_body_identity_text_never_bypasses_missing_header_view(self):
        o = copy.deepcopy(self.observation); o['identity_views'] = []
        o['visible_text'] = 'Mission ID: '+MISSION+'\nPlan revision 1'
        next(row for row in o['rows'] if row['text'].startswith('mission:'))['text'] = MISSION
        r = make_reader(); r.absorb(o, 'initial')
        self.assertEqual(r.coverage['identity_bindings'], [])
        self.assertNotIn('identity', r.coverage['fields'])

    def test_replacement_never_discards_original_low_confidence_notice_evidence(self):
        o = copy.deepcopy(self.observation); o['visible_text'] = ''
        original = o['frost_source_rows'][0]
        o['frost_source_rows'] = []  # Isolate notice-band rejection from the initial not-inspected text.
        self.assertTrue(reader.clean_inspection_status('frost', o, self.reader.plan))
        word = dict(original, text='unknown notice', top=222, left=700,
                    width=80, height=10, conf=1, block_num='notice')
        o['frost_source_rows'].append(word)
        self.assertFalse(reader.clean_inspection_status('frost', o, self.reader.plan))

    def test_discarded_observation_revokes_cached_scroll_scope(self):
        r = make_reader(self.out); r.frost_scope = ('discarded', [692,95,508,755])
        def attempt(label, control=False):
            if r.frost_scope is not None:
                raise observer.ObservationNotReady('Owned window changed during observation')
            return dict(status='fresh')
        with patch.object(r, 'checkpoint'), patch.object(r, 'observe_once', side_effect=attempt), \
                patch('reader.time.sleep'):
            self.assertEqual(r.observe('retry'), dict(status='fresh'))
        self.assertIsNone(r.frost_scope)

    def test_original_high_confidence_warning_remains_a_rejection_only_channel(self):
        o = copy.deepcopy(self.observation); o['visible_text'] = ''
        words = []
        for i, text in enumerate(('Waiting', 'for', 'the', 'host')):
            words.append(dict(o['frost_source_rows'][0], text=text, conf=90,
                              block_num='notice', line_num='1', top=220, left=700+i*35))
        o['frost_source_rows'] = words
        self.assertIn('Waiting for the host', reader.inspection_obstacles(reader.observation_notice_text(o)))
        self.assertFalse(reader.clean_inspection_status('frost', o, self.reader.plan))
        r = make_reader(); r.absorb(o, 'inspected')
        self.assertEqual(r.coverage['inspection_errors'],
                         [dict(problem='Waiting for the host', evidence='replay')])

    def test_empty_crop_keeps_source_rows_instead_of_erasing_them(self):
        r = make_reader(self.out)
        def run(command):
            if command[0] == 'tesseract':
                return 'level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n'
            return self.reader.run(command)
        r.run = run
        rows = captured(); o = r.frost_readout(self.frame, 'empty', rows)
        self.assertEqual(o['rows'], rows)
        self.assertEqual(o['frost_source_rows'], rows)
        self.assertEqual(r.targets(o, 'refresh'), [])

    def test_cached_scope_cannot_cross_process_birth_or_close_reopen(self):
        for mutation in ('birth', 'pid', 'closed', 'ambiguous'):
            r = make_reader(self.out); r.run = self.reader.run
            first = r.frost_readout(self.frame, 'scope-first', captured(), regions=False)
            self.assertIsNotNone(first['frost_panel_scope'])
            same = r.frost_readout(self.frame, 'scope-same', [], regions=False)
            self.assertEqual(same['frost_panel_scope'], first['frost_panel_scope'])
            if mutation == 'birth': r.process_identity['start_ticks'] = '457'
            if mutation == 'pid': r.process.pid = 124
            if mutation == 'closed': r.mission_open = False
            current = []
            if mutation == 'ambiguous':
                current = captured()
                heading = next(w for w in current if w['text'] == 'Missions')
                current.append(dict(heading, block_num='duplicate', top=400))
            changed = r.frost_readout(self.frame, 'scope-changed', current, regions=False)
            self.assertIsNone(changed['frost_panel_scope'])
            r.mission_open = True
            self.assertIsNone(r.frost_readout(self.frame, 'scope-reopened', [], regions=False)['frost_panel_scope'])

    def test_conflicting_valid_high_confidence_id_is_never_selected_by_expected_value(self):
        rows = captured()
        next(r for r in rows if r['text'].startswith('mission:'))['text'] = MISSION.replace('cd7','cd8')
        with self.assertRaisesRegex(RuntimeError, 'Conflicting high-confidence Mission identities'):
            self.reader.frost_readout(self.frame, 'conflict', rows)

    def test_missing_wrong_or_duplicate_toolbar_and_body_copy_never_target(self):
        for mutate in ('missing', 'duplicate', 'wrong', 'body', 'header'):
            o = copy.deepcopy(self.observation)
            button = next(b for b in o['frost_buttons'] if b['text'] == 'Back')
            if mutate == 'missing': o['frost_buttons'].remove(button)
            if mutate == 'duplicate': o['frost_buttons'].append(copy.deepcopy(button))
            if mutate == 'wrong': button['text'] = 'Hack'
            if mutate == 'body':
                for b in o['frost_buttons'][:3]: b['rect'][1] += 300
            if mutate == 'header':
                heading = next(w for w in o['rows'] if w['text'] == 'Missions')
                o['rows'].append(dict(heading, block_num='duplicate', top=400))
            self.assertEqual(self.reader.targets(o, 'refresh'), [])
            self.assertEqual(self.reader.targets(o, 'close_mission'), [])
        o = copy.deepcopy(self.observation)
        o['rows'] = [r for r in o['rows'] if r['text'] != 'environment']
        self.assertEqual(self.reader.targets(o, 'inspect'), [])

    def test_refresh_still_requires_two_equal_current_observations(self):
        r = make_reader()
        with patch.object(r,'observe',return_value=self.observation), patch.object(r,'click') as click:
            r.press('refresh', attempts=1)
            click.assert_called_once()
        changed = copy.deepcopy(self.observation)
        next(b for b in changed['frost_buttons'] if b['text']=='Refresh')['rows'][0]['left'] += 1
        with patch.object(r,'observe',side_effect=[self.observation,changed]), \
                patch.object(r,'click') as click, patch('reader.time.sleep'):
            with self.assertRaisesRegex(RuntimeError,'No unique settled public control'):
                r.press('refresh',attempts=1)
            click.assert_not_called()

    def test_wheel_uses_measured_thumb_not_the_old_exclusive_boundary(self):
        r = make_reader(); r.run = Mock()
        with patch.object(r,'observe',return_value=self.observation), patch.object(r,'bind_window'), \
                patch('reader.time.sleep'):
            r.scroll(4)
        self.assertEqual((r.inputs[0]['x'],r.inputs[0]['y']), (1383,333))
        self.assertTrue(observer.inside([1183,158,1,1], [1178,103,10,111]))
        self.assertFalse(observer.inside([1188,425,1,1], [1178,103,10,111]))
        self.assertEqual(r.inputs[0]['evidence'], ['replay','replay'])
        self.assertEqual(r.run.call_args[0][0][1:4], ['mousemove','1383','333'])

    def test_scroll_missing_duplicate_changed_or_foreign_window_never_sends_input(self):
        for mutation in ('missing','duplicate','changed','window','pid','track','scope'):
            a = copy.deepcopy(self.observation); b = copy.deepcopy(a)
            thumb = next(c for c in b['frost_scroll_components'] if c['rect']==[1178,103,10,111])
            if mutation=='missing': b['frost_scroll_components'].remove(thumb)
            if mutation=='duplicate': b['frost_scroll_components'].append(copy.deepcopy(thumb))
            if mutation=='changed': thumb['rect'][1] += 1
            if mutation=='window': b['window_rect'][0] += 1
            if mutation=='pid': b['pid'] += 1
            if mutation=='track': thumb['track'][1] += 1
            if mutation=='scope': b['frost_panel_scope'][1] += 1
            r = make_reader(); r.run = Mock()
            with patch.object(r,'observe',side_effect=[a,b]), patch.object(r,'bind_window'), \
                    self.assertRaises(RuntimeError):
                r.scroll(4)
            r.run.assert_not_called(); self.assertEqual(r.inputs,[])


if __name__ == '__main__':
    unittest.main()
