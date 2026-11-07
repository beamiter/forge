#!/usr/bin/env python3
"""Bounded offline navigation simulations; no native process or input tool."""
import copy
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch
import reader
from test_reader import node, plan


class EmberSimulation:
    def __init__(self, offset=0):
        self.offset=offset; self.wheels=[]; self.sequence=0
        self.r=reader.Reader('ember',{}, {},plan(),None,Path('/unused'))
        self.r.process=SimpleNamespace(pid=21); self.r.window='42'
        self.r.window_rect=[200,175,1200,850]; self.r.scroll_rect=[216,221,700,382]
        self.r.mission_open=True; self.r.observe=self.observe; self.r.scroll=self.scroll
        self.r.click=Mock()
    def observe(self,*args,**kwargs):
        self.sequence+=1
        frame=node('/frame',role=23,name=reader.FRAME['ember'],rect=[216,191,700,420])
        nodes=[frame]
        for path,name,y,enabled in [('/refresh',reader.REFRESH['ember'],246,True),
                                   ('/catalog',self.r.plan['title']+' · Needs your attention',278,True),
                                   ('/inspect',reader.INSPECT['ember'],1774,True),
                                   ('/freeze','Freeze observed candidates',1806,False)]:
            rect=[223,y-self.offset,227,29]
            n=node(path,'/frame',name=name,rect=rect,
                   visible=reader.inside(rect,[216,221,700,382]))
            if not enabled: n['states'][0]&=~((1<<8)|(1<<24))
            nodes.append(n)
        close=node('/close','/frame',name='Close window',rect=[893,199,14,14],visible=True)
        nodes.append(close)
        return dict(nodes=nodes,scoped_nodes=nodes,window='42',pid=21,window_rect=[200,175,1200,850],
                    mission_content_rect=[216,221,700,382],mission_header_rect=[216,191,700,30],
                    mission_header_close='/close',evidence='sim-%d'%self.sequence,
                    visible_text='mission-exact-id · revision 2' if self.offset<100 else '')
    def scroll(self,amount,observation=None):
        self.wheels.append(amount)
        self.offset=max(0,min(3076,self.offset+40*amount))


class EmberNavigationTests(unittest.TestCase):
    def test_captured_initial_failure_moves_up_and_clicks_only_when_fully_visible(self):
        sim=EmberSimulation(1876) # Inspect y=-102 in the original failed observation.
        sim.r.press('inspect')
        self.assertEqual(sim.wheels,[-4,-4,-4])
        sim.r.click.assert_called_once()
        self.assertTrue(reader.inside(sim.r.click.call_args.args[0]['rect'],sim.r.scroll_rect))
    def test_initial_top_moves_down_using_observed_geometry_not_larger_magic_rewind(self):
        sim=EmberSimulation(); sim.r.press('inspect')
        self.assertEqual(sim.wheels,[4]*8)
        sim.r.click.assert_called_once()
    def test_header_from_captured_bottom_proves_identity_refresh_and_saturation(self):
        sim=EmberSimulation(3076); result=sim.r.ember_header()
        self.assertEqual(sim.offset,0); self.assertLessEqual(len(sim.wheels),24)
        self.assertTrue(reader.bound_identity(result['visible_text'],sim.r.plan))
        self.assertEqual(len(sim.r.targets(result,'refresh')),1)
        sim.r.click.assert_not_called()
    def test_header_stall_without_identity_is_not_success(self):
        sim=EmberSimulation(); original=sim.observe
        def observe(*a,**kw):
            o=original();o['visible_text']='';return o
        sim.r.observe=observe
        with self.assertRaisesRegex(RuntimeError,'identity'):sim.r.ember_header()
    def test_refresh_from_bottom_locates_up_and_freeze_is_observation_only(self):
        sim=EmberSimulation(3076);sim.r.press('refresh')
        self.assertTrue(all(w<0 for w in sim.wheels));sim.r.click.reset_mock()
        sim.r.freeze_after_refresh()
        self.assertEqual(sim.r.coverage['stages']['freeze_after_refresh']['status'],'disabled')
        sim.r.click.assert_not_called()
        self.assertFalse(reader.allowed_control('ember','Freeze observed candidates',sim.r.plan,'inspect'))
    def test_visible_titlebar_close_needs_neither_body_rewind_nor_guessed_coordinates(self):
        sim=EmberSimulation(3076);sim.r.close_mission()
        self.assertEqual(sim.wheels,[]);sim.r.click.assert_called_once()
        self.assertEqual(sim.r.click.call_args.args[0]['node'],'/close')
    def test_duplicate_foreign_disabled_missing_and_unusable_geometry_fail_without_input(self):
        for mutation in ('duplicate','foreign_pid','foreign_owner','ancestry','disabled','missing','width','oversize','contained_hidden'):
            sim=EmberSimulation(1876);original=sim.observe
            def observe(*a,**kw):
                o=original();n=next(n for n in o['scoped_nodes'] if n['path']=='/inspect')
                if mutation=='duplicate':o['scoped_nodes'].append(dict(n,path='/duplicate'))
                if mutation=='foreign_pid':n['pid']=99
                if mutation=='foreign_owner':n['owner']=':1.99'
                if mutation=='ancestry':n['parent']='/other'
                if mutation=='disabled':n['states'][0]&=~(1<<8)
                if mutation=='missing':o['scoped_nodes'].remove(n)
                if mutation=='width':n['rect'][0]=0
                if mutation=='oversize':n['rect'][3]=500
                if mutation=='contained_hidden':n['rect'][1]=300;n['visible']=False
                return o
            sim.r.observe=observe
            with self.subTest(mutation=mutation),self.assertRaises(RuntimeError):sim.r.press('inspect')
            self.assertEqual(sim.wheels,[]);sim.r.click.assert_not_called()
    def test_no_progress_or_wrong_direction_is_not_a_top_or_visibility_proof(self):
        for delta in (0,-40):
            sim=EmberSimulation(1876)
            def scroll(amount,observation=None):sim.wheels.append(amount);sim.offset-=delta
            sim.r.scroll=scroll
            with self.assertRaisesRegex(RuntimeError,'no progress'):sim.r.press('inspect')
            self.assertEqual(len(sim.wheels),1);sim.r.click.assert_not_called()
    def test_window_or_scope_change_during_seek_rejects(self):
        for mutation in ('window','pid','scope'):
            sim=EmberSimulation(1876);original=sim.observe
            def observe(*a,**kw):
                o=original()
                if sim.sequence>1:
                    if mutation=='window':o['window']='43'
                    if mutation=='pid':o['pid']=22
                    if mutation=='scope':o['mission_content_rect'][1]+=1
                return o
            sim.r.observe=observe
            with self.assertRaises(RuntimeError):sim.r.press('inspect')
            sim.r.click.assert_not_called()
    def test_unsettled_visible_second_frame_never_clicks(self):
        sim=EmberSimulation(1440);original=sim.observe
        def observe(*a,**kw):
            o=original()
            if sim.sequence>1:next(n for n in o['scoped_nodes'] if n['path']=='/inspect')['rect'][1]+=1
            return o
        sim.r.observe=observe
        with self.assertRaisesRegex(RuntimeError,'settled'):sim.r.press('inspect')
        sim.r.click.assert_not_called()
    def test_budget_is_bounded_when_progress_is_real_but_too_small(self):
        sim=EmberSimulation(3076)
        def scroll(amount,observation=None):sim.wheels.append(amount);sim.offset-=1
        sim.r.scroll=scroll
        with self.assertRaisesRegex(RuntimeError,'24-step'):sim.r.locate_ember('inspect')
        self.assertEqual(len(sim.wheels),24);sim.r.click.assert_not_called()
    def test_false_close_marker_or_body_copy_never_targets(self):
        sim=EmberSimulation();o=sim.observe()
        o['mission_header_close']=None
        self.assertEqual(sim.r.targets(o,'close_mission'),[])
        o['mission_header_close']='/app-close'
        self.assertEqual(sim.r.targets(o,'close_mission'),[])
    def test_unknown_notice_blocks_but_body_or_foreign_unknown_does_not(self):
        sim=EmberSimulation();o=sim.observe()
        for name,clip,path,expected in [('notice',[230,276,100,2],'/notice',True),
                                        ('body',[230,350,100,20],'/body',False),
                                        ('foreign',[230,276,100,2],'/foreign',False)]:
            changed=copy.deepcopy(o)
            if name!='foreign':changed['scoped_nodes'].append(node(path,'/frame',role=29,rect=clip))
            changed['unknown_text']=[dict(path=path,clip=clip,status='unsupported_unproven')]
            self.assertEqual(reader.unknown_notices('ember',changed,sim.r.plan),expected)


    def test_reopened_catalog_selection_uses_existing_bounded_navigation(self):
        sim=EmberSimulation(1460) # Captured catalog top -1182 after Mission surface reopen.
        sim.r.press('select')
        self.assertTrue(sim.wheels and all(w == -4 for w in sim.wheels))
        sim.r.click.assert_called_once()
        target=sim.r.click.call_args.args[0]
        self.assertEqual(target['node'],'/catalog')
        self.assertTrue(reader.inside(target['rect'],sim.r.scroll_rect))

    def test_initial_visible_catalog_selection_does_not_scroll(self):
        sim=EmberSimulation()
        sim.r.press('select')
        self.assertEqual(sim.wheels,[])
        self.assertEqual(sim.r.click.call_args.args[0]['node'],'/catalog')

    def test_reopened_catalog_rejects_foreign_missing_ambiguous_or_disabled(self):
        for mutation in ('duplicate','foreign_pid','foreign_owner','ancestry','disabled','missing','width'):
            sim=EmberSimulation(1460); original=sim.observe
            def observe(*a,**kw):
                o=original();n=next(n for n in o['scoped_nodes'] if n['path']=='/catalog')
                if mutation=='duplicate':o['scoped_nodes'].append(dict(n,path='/duplicate'))
                if mutation=='foreign_pid':n['pid']=99
                if mutation=='foreign_owner':n['owner']=':1.99'
                if mutation=='ancestry':n['parent']='/other'
                if mutation=='disabled':n['states'][0]&=~(1<<8)
                if mutation=='missing':o['scoped_nodes'].remove(n)
                if mutation=='width':n['rect'][0]=0
                return o
            sim.r.observe=observe
            with self.subTest(mutation=mutation),self.assertRaises(RuntimeError):sim.r.press('select')
            self.assertEqual(sim.wheels,[]);sim.r.click.assert_not_called()

    def test_reopened_catalog_no_scroll_progress_rejects_before_click(self):
        sim=EmberSimulation(1460)
        sim.r.scroll=lambda amount,observation=None:sim.wheels.append(amount)
        with self.assertRaisesRegex(RuntimeError,'no progress'):sim.r.press('select')
        self.assertEqual(sim.wheels,[-4]);sim.r.click.assert_not_called()

    def test_visible_catalog_must_settle_before_click(self):
        sim=EmberSimulation();original=sim.observe
        def observe(*a,**kw):
            o=original()
            if sim.sequence>1:next(n for n in o['scoped_nodes'] if n['path']=='/catalog')['rect'][0]+=1
            return o
        sim.r.observe=observe
        with self.assertRaisesRegex(RuntimeError,'settled'):sim.r.press('select')
        sim.r.click.assert_not_called()


    def test_select_waits_for_initial_catalog_but_does_not_input_while_absent(self):
        sim=EmberSimulation();original=sim.observe
        def observe(*a,**kw):
            o=original()
            if sim.sequence<=2:o['scoped_nodes']=[n for n in o['scoped_nodes'] if n['path']!='/catalog']
            return o
        sim.r.observe=observe
        with patch('reader.time.sleep') as pause:sim.r.press('select')
        self.assertEqual(pause.call_count,2);self.assertEqual(sim.wheels,[])
        sim.r.click.assert_called_once()

    def test_select_catalog_wait_has_ten_frame_bound_and_zero_input(self):
        sim=EmberSimulation();original=sim.observe
        def observe(*a,**kw):
            o=original();o['scoped_nodes']=[n for n in o['scoped_nodes'] if n['path']!='/catalog'];return o
        sim.r.observe=observe
        with patch('reader.time.sleep'),self.assertRaisesRegex(RuntimeError,'catalog did not become ready'):
            sim.r.press('select')
        self.assertEqual(sim.sequence,10);self.assertEqual(sim.wheels,[])
        sim.r.click.assert_not_called()

    def test_select_catalog_disappearing_after_wheel_is_not_readiness(self):
        sim=EmberSimulation(1460);original=sim.observe
        def observe(*a,**kw):
            o=original()
            if sim.wheels:o['scoped_nodes']=[n for n in o['scoped_nodes'] if n['path']!='/catalog']
            return o
        sim.r.observe=observe
        with patch('reader.time.sleep') as pause,self.assertRaisesRegex(RuntimeError,'No unique navigation control'):
            sim.r.press('select')
        pause.assert_not_called();self.assertEqual(sim.wheels,[-4])
        sim.r.click.assert_not_called()


    def test_wait_cannot_hide_illegal_first_matching_catalog(self):
        for mutation in ('disabled','foreign_pid','foreign_owner','duplicate'):
            sim=EmberSimulation();original=sim.observe
            def observe(*a,**kw):
                o=original();n=next(n for n in o['scoped_nodes'] if n['path']=='/catalog')
                if sim.sequence==1:o['scoped_nodes'].remove(n)
                elif sim.sequence==2:
                    if mutation=='disabled':n['states'][0]&=~(1<<8)
                    if mutation=='foreign_pid':n['pid']=99
                    if mutation=='foreign_owner':n['owner']=':1.99'
                    if mutation=='duplicate':o['scoped_nodes'].append(dict(n,path='/duplicate'))
                return o
            sim.r.observe=observe
            with patch('reader.time.sleep'),self.subTest(mutation=mutation),self.assertRaises(RuntimeError):
                sim.r.press('select')
            self.assertEqual(sim.sequence,2);self.assertEqual(sim.wheels,[])
            sim.r.click.assert_not_called()

    def test_missing_catalog_still_requires_owned_frame_and_viewport(self):
        for mutation in ('frame','viewport','pid','window'):
            sim=EmberSimulation();original=sim.observe
            def observe(*a,**kw):
                o=original();o['scoped_nodes']=[n for n in o['scoped_nodes'] if n['path']!='/catalog']
                if mutation=='frame':o['scoped_nodes']=[n for n in o['scoped_nodes'] if n['path']!='/frame']
                if mutation=='viewport':o['mission_content_rect']=[0,0,500,500]
                if mutation=='pid':o['pid']=99
                if mutation=='window':o['window']='99'
                return o
            sim.r.observe=observe
            with patch('reader.time.sleep') as pause,self.subTest(mutation=mutation),self.assertRaises(RuntimeError):
                sim.r.press('select')
            pause.assert_not_called();self.assertEqual(sim.sequence,1)
            self.assertEqual(sim.wheels,[]);sim.r.click.assert_not_called()


if __name__=='__main__':unittest.main()
