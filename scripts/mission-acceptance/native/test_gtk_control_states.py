#!/usr/bin/env python3
"""GTK state/ancestry and settled-target contracts; no native app or input."""
import copy
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

import observer
import reader
from test_reader import node, plan
from test_status_scope import status_fixture


GTK_STATES = (1 << 11) | (1 << 24) | (1 << 25) | (1 << 30)


def control_fixture(app='anvil'):
    label = 'Missions · delivery plans' if app == 'anvil' else '＋ New'
    window = [122,248,1356,704]
    nodes = [node('/app', children=['/frame'], role=75, interfaces=[], states=[0,0]),
             node('/frame', '/app', children=['/container'], role=23,
                  name=app.title()+' · ~', rect=window, states=[GTK_STATES,0]),
             node('/container', '/frame', children=['/control'], role=39,
                  rect=[177,290,280,613], states=[GTK_STATES,0]),
             node('/control', '/container', role=43, name=label,
                  rect=[198,599,248,28], states=[GTK_STATES,0])]
    observed = dict(root='/app', owner=':1.20', pid=21, nodes=nodes, scoped_nodes=nodes,
                    window='42', window_rect=window, evidence='first', visible_text='')
    refresh_visibility(observed)
    return observed


def refresh_visibility(observed):
    by_path = observer.validate_tree(observed)
    for n in observed['nodes']:
        n['visible'] = observer.visible(n, by_path, observed['window_rect'])
        n['visible_text'] = n['name'] if n['visible'] else ''


class GtkControlStatesTests(unittest.TestCase):
    def reader(self, app='anvil'):
        return reader.Reader(app, {}, {}, plan(), None, Path('/unused'))

    def test_recorded_sensitive_not_enabled_state_accepts_one_exact_control(self):
        for app, purpose in [('anvil','open'),('forge','menu')]:
            o = control_fixture(app)
            n = o['nodes'][-1]
            self.assertEqual(n['states'], [1124075520,0])
            self.assertFalse(observer.state(n,8))
            self.assertEqual(self.reader(app).targets(o,purpose),
                             [dict(label=n['name'],rect=n['rect'],node=n['path'])])
            self.assertFalse(observer.state(o['nodes'][0],24))

    def test_gtk_disabled_and_inherited_disabled_reject_with_or_without_enabled(self):
        for enabled in (False,True):
            for index in (1,2,3):
                o = control_fixture()
                if enabled:
                    for n in o['nodes'][1:]:n['states'][0] |= 1 << 8
                o['nodes'][index]['states'][0] &= ~(1 << 24)
                with self.subTest(enabled=enabled,index=index):
                    self.assertEqual(self.reader().targets(o,'open'),[])

    def test_defunct_ancestor_and_missing_owned_ancestry_fail_closed(self):
        for index in (1,2,3):
            o = control_fixture();o['nodes'][index]['states'][0] |= 1 << 6
            self.assertEqual(self.reader().targets(o,'open'),[])
        for mutate in (lambda o:o['nodes'][2].update(owner=':1.99'),
                       lambda o:o['nodes'][2].update(pid=99),
                       lambda o:o['nodes'][2].update(parent='/missing'),
                       lambda o:o['nodes'][0].update(role=23),
                       lambda o:o['nodes'][1]['children'].append('/container'),
                       lambda o:o['nodes'][2]['children'].append('/frame')):
            o=control_fixture();mutate(o)
            with self.assertRaises(RuntimeError):self.reader().targets(o,'open')

    def test_clipped_hidden_label_only_and_wrong_semantics_stay_rejected(self):
        for mutate in (lambda n:n.update(rect=[1450,599,248,28]),
                       lambda n:n['states'].__setitem__(0,GTK_STATES & ~(1 << 25)),
                       lambda n:n['states'].__setitem__(0,GTK_STATES & ~(1 << 30)),
                       lambda n:n.update(role=29),
                       lambda n:n.update(name='Missions - delivery plans'),
                       lambda n:n.update(name='Create Mission')):
            o=control_fixture();mutate(o['nodes'][-1]);refresh_visibility(o)
            self.assertEqual(self.reader().targets(o,'open'),[])

    def test_ember_still_requires_enabled_and_sensitive(self):
        o=control_fixture('anvil');n=o['nodes'][-1];n['name']='Missions · 1 need you'
        r=self.reader('ember')
        self.assertEqual(r.targets(o,'open'),[])
        n['states'][0] |= 1 << 8
        self.assertEqual(len(r.targets(o,'open')),1)
        n['states'][0] &= ~(1 << 24)
        self.assertEqual(r.targets(o,'open'),[])

    def test_duplicate_controls_are_not_collapsed_or_guessed(self):
        o=control_fixture();n=copy.deepcopy(o['nodes'][-1]);n['path']='/duplicate'
        o['nodes'].append(n);o['nodes'][2]['children'].append(n['path'])
        r=self.reader();r.observe=Mock(return_value=o);r.click=Mock()
        with self.assertRaisesRegex(RuntimeError,'Ambiguous visible input target'):
            r.press('open',attempts=1)
        r.click.assert_not_called()

    def test_two_settled_controls_required_and_later_disabled_ancestor_rejects(self):
        first=control_fixture();second=copy.deepcopy(first);second['evidence']='second'
        for change in ('none','ancestor','node','name','geometry','missing'):
            changed=copy.deepcopy(second)
            if change=='ancestor':changed['nodes'][2]['states'][0] &= ~(1 << 24)
            if change=='node':changed['nodes'][-1]['states'][0] &= ~(1 << 24)
            if change=='name':changed['nodes'][-1]['name']='Create Mission'
            if change=='geometry':changed['nodes'][-1]['rect'][0]+=1
            if change=='missing':changed['nodes'][-1]['visible']=False
            r=self.reader();r.observe=Mock(side_effect=[first,changed]);r.click=Mock()
            with patch('reader.time.sleep'):
                if change=='none':
                    r.press('open',attempts=1)
                    r.click.assert_called_once_with(r.targets(second,'open')[0],'open','second')
                else:
                    with self.assertRaisesRegex(RuntimeError,'No unique settled public control'):
                        r.press('open',attempts=1)
                    r.click.assert_not_called()

    def test_action_eligibility_does_not_prove_inspection_completion(self):
        good=status_fixture('anvil')
        for n in good['nodes']:n['states'][0] &= ~(1 << 8)
        good['nodes'][0]['states']=[0,0]
        good['evidence']='status'
        r=self.reader();r.observe=Mock(return_value=good)
        with patch('reader.time.sleep'):r.wait_inspection_settled()
        self.assertEqual(r.coverage['stages']['inspection_ready']['status'],'settled')
        for case in ('unknown','busy','insensitive','stale_inspection'):
            o=copy.deepcopy(good);status=reader.gtk_status_node('anvil',o)
            if case=='unknown':
                status['visible_text']=''
                o['unknown_text']=[dict(path=status['path'],clip=status['rect'],status='unavailable',unproven_lines=[0])]
            if case=='busy':o['visible_text']+='\n'+reader.INSPECTION_BUSY[0]
            if case=='stale_inspection':o['visible_text']+='\nSaved observation requires reinspection'
            if case=='insensitive':next(n for n in o['nodes'] if n['path']=='/inspect')['states'][0] &= ~(1 << 24)
            r=self.reader();r.observe=Mock(return_value=o)
            with self.subTest(case=case),patch('reader.time.sleep'),self.assertRaises(RuntimeError):
                r.wait_inspection_settled()
            self.assertNotIn('inspection_ready',r.coverage['stages'])

    def test_absent_enabled_bit_does_not_prove_freeze_disabled(self):
        for app in ('anvil','forge'):
            o=control_fixture(app);n=o['nodes'][-1]
            n['name']={'anvil':'Freeze inspected commits','forge':'Freeze inspected clean commits as candidates'}[app]
            r=self.reader(app);r.observe=Mock(return_value=o);r.scroll=Mock()
            with self.assertRaisesRegex(RuntimeError,'did not visibly disable'):
                r.freeze_after_refresh()
            self.assertEqual(r.coverage['stages']['freeze_after_refresh']['status'],'unexpected_enabled')
            n['states'][0] &= ~(1 << 24)
            r.freeze_after_refresh()
            self.assertEqual(r.coverage['stages']['freeze_after_refresh']['status'],'disabled')


if __name__=='__main__':unittest.main()
