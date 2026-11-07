#!/usr/bin/env python3
"""Replay real public OCR words; no native input, provider or host operations."""
import copy
from pathlib import Path
import sys
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import observer
import reader
from test_reader import node, plan


FIXTURE = Path(__file__).with_name('fixtures') / 'frost-sidebar-open.tsv'


def captured_rows():
    return observer.ocr_rows(FIXTURE.read_text())


def mission_words(rows):
    return [row for row in rows if row['block_num'] == '26']


class FrostOpenControlTests(unittest.TestCase):
    def setUp(self):
        self.rows = captured_rows()
        self.reader = reader.Reader('frost', {}, {}, plan(), None, Path('/unused'))
        self.reader.window_rect = [200, 175, 1200, 850]

    def observation(self, rows=None, evidence='replay'):
        return dict(rows=self.rows if rows is None else rows, visible_text='', evidence=evidence)

    def targets(self, rows=None):
        return self.reader.targets(self.observation(rows), 'open')

    def test_real_words_preserve_count_and_observed_glyph_with_exact_box(self):
        self.assertEqual(len(self.rows), 14)
        self.assertEqual(self.targets(), [dict(label='Missions · 0 need review',
            observed_label='Missions - 0 need review', rect=[271, 449, 158, 11], node=None)])
        self.assertEqual(mission_words(self.rows)[1]['conf'], 89.711044)
        mission_words(self.rows)[2]['text'] = '7'
        self.assertEqual(self.targets()[0]['label'], 'Missions · 7 need review')

    def test_canonical_glyph_is_also_accepted_without_global_normalization(self):
        mission_words(self.rows)[1]['text'] = '·'
        self.assertEqual(self.targets()[0]['observed_label'], 'Missions · 0 need review')
        for app in reader.APPS:
            self.assertFalse(reader.allowed_control(app, 'Missions - 0 need review', plan(), 'open'))
        self.assertTrue(reader.allowed_control('frost', 'Missions · 0 need review', plan(), 'open'))

    def test_atspi_stays_exact_and_requires_real_button_role(self):
        self.reader.app = 'anvil'
        n = node('/mission', '/app', visible=True, name='Missions - 0 need review')
        root = node('/app', children=['/mission'], role=75, states=[0,0])
        observed = dict(pid=n['pid'], owner=n['owner'], root='/app',
                        nodes=[root,n], scoped_nodes=[n])
        self.assertEqual(self.reader.targets(observed, 'open'), [])
        n['name'] = 'Missions · 0 need review'
        self.assertEqual(len(self.reader.targets(observed, 'open')), 1)
        n['role'] = 29
        self.assertEqual(self.reader.targets(observed, 'open'), [])

    def test_low_nonfinite_and_out_of_range_confidence_on_any_word_rejects(self):
        for index in range(len(self.rows)):
            if self.rows[index]['text'] == '¥':
                continue  # The observed collapse icon is not a label word.
            for confidence in (79.999, float('nan'), float('inf'), 101):
                rows = copy.deepcopy(self.rows)
                rows[index]['conf'] = confidence
                with self.subTest(index=index, confidence=confidence):
                    self.assertEqual(self.targets(rows), [])

    def test_wrong_count_or_format_is_not_corrected(self):
        for index, bad_values in ((0, ('Mission', 'Missions:', 'XMissions')),
                                  (1, ('—', '–', '•', '.', ':', '')),
                                  (2, ('O', '-1', '+1', '00', '01', '1.0', '1,000', '١')),
                                  (3, ('needs', 'NEED')), (4, ('reviews', 'review!'))):
            for value in bad_values:
                rows = copy.deepcopy(self.rows)
                mission_words(rows)[index]['text'] = value
                with self.subTest(index=index, value=value):
                    self.assertEqual(self.targets(rows), [])

    def test_real_dash_or_baseline_period_geometry_is_not_a_middle_dot(self):
        for changes in (dict(width=7, height=1), dict(top=284), dict(height=8)):
            rows = copy.deepcopy(self.rows)
            mission_words(rows)[1].update(changes)
            with self.subTest(changes=changes):
                self.assertEqual(self.targets(rows), [])

    def test_missing_cross_line_or_displaced_words_rejects(self):
        for mutate in (lambda words: words.pop(),
                       lambda words: mission_words(words)[2].update(line_num='2'),
                       lambda words: mission_words(words)[3].update(left=500),
                       lambda words: mission_words(words)[4].update(top=290),
                       lambda words: mission_words(words)[0].update(width=0)):
            rows = copy.deepcopy(self.rows)
            mutate(rows)
            self.assertEqual(self.targets(rows), [])

    def test_matching_prose_without_sidebar_or_outside_control_band_rejects(self):
        self.assertEqual(self.targets(mission_words(self.rows)), [])
        for dx, dy in ((350, 0), (0, 60), (0, -40), (20, 0)):
            rows = copy.deepcopy(self.rows)
            for word in mission_words(rows):
                word['left'] += dx
                word['top'] += dy
            with self.subTest(dx=dx, dy=dy):
                self.assertEqual(self.targets(rows), [])

    def test_extra_nearby_text_rejects_even_separate_block_or_low_confidence(self):
        for block in ('26', '99'):
            for confidence in (95, 1):
                rows = copy.deepcopy(self.rows)
                extra = dict(mission_words(rows)[-1], text='bogus', left=235, width=35,
                             block_num=block, conf=confidence)
                rows.append(extra)
                with self.subTest(block=block, confidence=confidence):
                    self.assertEqual(self.targets(rows), [])

    def test_ambiguous_filter_strip_rejects(self):
        rows = copy.deepcopy(self.rows)
        rows.append(dict(next(row for row in rows if row['text'] == 'All'), block_num='99', top=400))
        self.assertEqual(self.targets(rows), [])

    def test_sidebar_heading_is_required_unique_and_in_owned_left_region(self):
        self.assertEqual(self.targets([w for w in self.rows if w['block_num'] != '10']), [])
        for include_heading in (False, True):
            rows = copy.deepcopy(self.rows)
            for word in rows:
                if include_heading or word['block_num'] != '10':
                    word['left'] += 500
                    word['top'] += 100
            with self.subTest(include_heading=include_heading):
                self.assertEqual(self.targets(rows), [])
        rows = copy.deepcopy(self.rows)
        rows.extend(dict(w, block_num='99', left=w['left']+500, top=w['top']+100)
                    for w in self.rows if w['block_num'] == '10')
        self.assertEqual(self.targets(rows), [])

    def test_decorative_heading_icon_cannot_hide_prefix_prose(self):
        for changes in (dict(text='X'), dict(width=50), dict(top=20)):
            rows = copy.deepcopy(self.rows)
            rows[0].update(changes)
            with self.subTest(changes=changes):
                self.assertEqual(self.targets(rows), [])

    def test_translation_and_scale_use_only_observed_word_geometry(self):
        for scale in (.5, 1, 2, 3):
            self.reader.window_rect = [200, 175, round(1200*scale), round(850*scale)]
            rows = copy.deepcopy(self.rows)
            for word in rows:
                for field in ('left', 'top', 'width', 'height'):
                    word[field] = round(word[field]*scale)
                word['left'] += 17
                word['top'] += 23
            words = mission_words(rows)
            x = min(w['left'] for w in words); y = min(w['top'] for w in words)
            expected = [x+200, y+175, max(w['left']+w['width'] for w in words)-x,
                        max(w['top']+w['height'] for w in words)-y]
            with self.subTest(scale=scale):
                self.assertEqual(self.targets(rows)[0]['rect'], expected)

    def test_two_matching_controls_remain_ambiguous_and_never_click(self):
        rows = copy.deepcopy(self.rows)
        rows.extend(dict(w, block_num='99', top=w['top']+14) for w in mission_words(self.rows))
        self.assertEqual(len(self.targets(rows)), 2)
        with patch.object(self.reader, 'observe', return_value=self.observation(rows)), \
                patch.object(self.reader, 'click') as click:
            with self.assertRaisesRegex(RuntimeError, 'Ambiguous visible input target'):
                self.reader.press('open', attempts=1)
            click.assert_not_called()

    def test_press_still_requires_two_equal_observations(self):
        with patch.object(self.reader, 'observe', side_effect=[self.observation(evidence='first'),
                self.observation(evidence='settled')]) as observe, patch.object(self.reader, 'click') as click:
            self.reader.press('open', attempts=1)
        self.assertEqual(observe.call_count, 2)
        click.assert_called_once_with(self.targets()[0], 'open', 'settled')
        for change in ('count', 'geometry', 'observed_glyph'):
            changed = copy.deepcopy(self.rows)
            if change == 'count':
                mission_words(changed)[2]['text'] = '1'
            elif change == 'geometry':
                for word in mission_words(changed):
                    word['left'] += 1
            else:
                mission_words(changed)[1]['text'] = '·'
            with patch.object(self.reader, 'observe', side_effect=[self.observation(), self.observation(changed)]), \
                    patch.object(self.reader, 'click') as click, patch('reader.time.sleep'):
                with self.assertRaisesRegex(RuntimeError, 'No unique settled public control'):
                    self.reader.press('open', attempts=1)
                click.assert_not_called()

    def test_owned_window_bounds_are_still_rechecked_before_input(self):
        target = self.targets()[0]
        self.reader.window_rect = [500, 500, 1200, 850]
        self.reader.run = Mock()
        with patch.object(self.reader, 'bind_window'):
            with self.assertRaisesRegex(RuntimeError, 'Target lies outside owned window'):
                self.reader.click(target, 'open', 'replay')
        self.reader.run.assert_not_called()
        self.assertEqual(self.reader.inputs, [])

    def test_other_purposes_do_not_inherit_badge_tolerance(self):
        for purpose in ('menu', 'select', 'refresh', 'inspect', 'expand', 'close_mission'):
            self.assertEqual(self.reader.targets(self.observation(), purpose), [])
            for app in reader.APPS:
                self.assertFalse(reader.allowed_control(app, 'Missions · 0 need review', plan(), purpose))


if __name__ == '__main__':
    unittest.main()
