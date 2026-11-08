"""Offline Frost wheel contracts using measured artifact geometry, not UI proof."""
import copy
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import reader

TEMPLATE = dict(frost_panel_scope=[692,93,508,757],evidence='fixture-only',
    window='8388611',window_rect=[200,175,1200,850],pid=18490,
    frost_scroll_components=[dict(rect=[1178,103,10,111],fill=1.0,track=[1178,103,10,725])])

def frame(y,height=111):
    result=copy.deepcopy(TEMPLATE);result['frost_scroll_components'][0]['rect']=[1178,y,10,height]
    return result

def sequence(positions):
    result=[]
    for y in positions:result.extend([frame(y) for _ in range(2 if y==103 else 3)])
    return result

def fake(observations):
    r=reader.Reader.__new__(reader.Reader)
    r.app='frost';r.mission_open=True;r.process=SimpleNamespace(pid=18490)
    r.process_identity=dict(start_ticks='fixture-only')
    r.window='8388611';r.window_rect=[200,175,1200,850];r.inputs=[];r.commands=[]
    r.bind_window=lambda:None;source=iter(observations)
    r.observe=lambda *a,**kw:next(source);r.run=lambda command:r.commands.append(command)
    return r

class FrostNavigationTests(unittest.TestCase):
    def setUp(self):
        pause=patch('reader.time.sleep');pause.start();self.addCleanup(pause.stop)

    def test_actual_bottom_to_top(self):
        r=fake(sequence([717,680,643,606,569,532,495,458,421,384,347,310,273,236,199,162,125,103]))
        top=r.frost_rewind();self.assertEqual(top['frost_scroll_components'][0]['rect'][1],103)
        self.assertEqual(len(r.inputs),17);self.assertTrue(all(e['amount']==-4 for e in r.inputs))

    def test_already_at_top_no_wheel(self):
        r=fake([frame(103),frame(103)]);r.frost_rewind();self.assertEqual(r.inputs,[])

    def test_recorded_partial_rewind_not_top(self):
        r=fake(sequence([440,440]))
        with self.assertRaisesRegex(RuntimeError,'no upward progress'):r.frost_rewind()
        self.assertEqual(len(r.inputs),1)

    def test_wrong_direction(self):
        r=fake(sequence([440,477]))
        with self.assertRaisesRegex(RuntimeError,'no upward progress'):r.frost_rewind()

    def test_one_pixel_not_top(self):
        r=fake(sequence([104,104]))
        with self.assertRaisesRegex(RuntimeError,'no upward progress'):r.frost_rewind()

    def test_top_requires_stable_second_frame(self):
        r=fake([frame(103),frame(104)])
        with self.assertRaisesRegex(RuntimeError,'not stable'):r.frost_rewind()

    def test_pid_change(self):
        observations=sequence([440,403]);observations[3]['pid']=18491;r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'ownership changed'):r.frost_rewind()

    def test_window_change(self):
        observations=sequence([440,403]);observations[3]['window']='different';r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'ownership changed'):r.frost_rewind()

    def test_scope_change(self):
        observations=sequence([440,403]);observations[3]['frost_panel_scope'][0]-=1;r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'binding or track changed'):r.frost_rewind()

    def test_track_change(self):
        observations=sequence([440,403]);observations[3]['frost_scroll_components'][0]['track'][1]+=1;r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'binding or track changed'):r.frost_rewind()

    def test_ambiguous_thumb(self):
        o=frame(440);o['frost_scroll_components']*=2;r=fake([o])
        with self.assertRaisesRegex(RuntimeError,'No unique'):r.frost_rewind()

    def test_preinput_observation_must_match(self):
        r=fake([frame(440),frame(403),frame(403)])
        with self.assertRaisesRegex(RuntimeError,'observation changed before'):r.frost_rewind()
        self.assertEqual(r.inputs,[])

    def test_preinput_pair_must_match(self):
        r=fake([frame(440),frame(440),frame(403)])
        with self.assertRaisesRegex(RuntimeError,'changed before wheel'):r.frost_rewind()
        self.assertEqual(r.inputs,[])

    def test_budget_24_batches(self):
        r=fake(sequence(list(range(717,692,-1))))
        with self.assertRaisesRegex(RuntimeError,'24-wheel-batch budget'):r.frost_rewind()
        self.assertEqual(len(r.inputs),24)

    def test_thumb_rounding_height_may_change_while_moving(self):
        observations=sequence([140,103])
        for i in (0,1,2):observations[i]['frost_scroll_components'][0]['rect'][3]=112
        r=fake(observations);r.frost_rewind();self.assertEqual(len(r.inputs),1)

    def test_horizontal_change(self):
        observations=sequence([440,403]);observations[3]['frost_scroll_components'][0]['rect'][0]+=1
        observations[3]['frost_scroll_components'][0]['rect'][2]-=1;r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'binding or track changed'):r.frost_rewind()

    def test_width_change(self):
        observations=sequence([440,403]);observations[3]['frost_scroll_components'][0]['rect'][2]-=1;r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'binding or track changed'):r.frost_rewind()

    def test_height_change_two_pixels(self):
        observations=sequence([440,403]);observations[3]['frost_scroll_components'][0]['rect'][3]+=2;r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'height changed'):r.frost_rewind()

    def test_height_cannot_drift_by_single_pixel_steps(self):
        observations=sequence([440,403,366])
        for i in (3,4,5):observations[i]['frost_scroll_components'][0]['rect'][3]=112
        observations[6]['frost_scroll_components'][0]['rect'][3]=113;r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'height changed'):r.frost_rewind()

    def test_height_range_is_global_not_around_first(self):
        observations=sequence([440,403,366])
        for i in (3,4,5):observations[i]['frost_scroll_components'][0]['rect'][3]=110
        observations[6]['frost_scroll_components'][0]['rect'][3]=112;r=fake(observations)
        with self.assertRaisesRegex(RuntimeError,'height changed'):r.frost_rewind()

    def test_recorded_positions_and_heights_in_reverse(self):
        measured=[(103,111),(140,111),(177,111),(214,111),(250,112),(287,112),(324,112),
            (361,111),(398,111),(435,111),(472,111),(509,111),(545,112),(582,112),
            (619,111),(656,111),(693,111),(717,111)]
        observations=[]
        for y,h in reversed(measured):observations.extend([frame(y,h)]*(2 if y==103 else 3))
        r=fake(observations);r.frost_rewind();self.assertEqual(len(r.inputs),17)

if __name__=='__main__':
    unittest.main()
