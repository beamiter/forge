#!/usr/bin/env python3
"""Pure negative/contract tests. These never launch native apps or providers."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import reader
import observer
import provenance


def plan():
    return dict(id='mission-exact-id', revision=2, title='SharedMissionAcceptance',
                goal='Inspect exact candidates without provider execution',required_environment='ubuntu-24.04',
                repositories=[dict(identity={'full_name':'beamiter/'+name}, path='/tmp/checkouts/'+name,
                    candidate=('%040x' % (index+1)), baseline='f'*40, target_ref='refs/heads/master',
                    required_core_pin=None if index==0 else reader.CORE, required_checks=['validation'],tasks=[])
                    for index,name in enumerate(reader.REPOS)])


def node(path, parent=None, children=(), **kw):
    n=dict(path=path,parent=parent,children=list(children),owner=':1.20',pid=21,role=43,name='safe',
           states=[(1<<8)|(1<<24)|(1<<25)|(1<<30),0], interfaces=[observer.COMPONENT],rect=[10,10,20,20])
    n.update(kw)
    return n


def tree():
    root='/org/gtk/gtk4/accessible/root'; child='/org/gtk/gtk4/accessible/1'
    return dict(pid=21,owner=':1.20',root=root,nodes=[node(root,children=[child],role=23,rect=[0,0,100,100]),node(child,root)])


class PlanTests(unittest.TestCase):
    def test_exact_five_repository_contract(self):
        self.assertEqual(reader.validate_plan(plan()), plan())

    def test_partial_or_duplicate_or_task_bearing_plan_fails(self):
        for mutate in [lambda p:p['repositories'].pop(),
                       lambda p:p['repositories'].__setitem__(1,p['repositories'][0]),
                       lambda p:p['repositories'][0].update(candidate='123abcd'),
                       lambda p:p['repositories'][0].update(tasks=[{'task_id':1}]),
                       lambda p:p['repositories'][1].update(required_core_pin='a'*40),
                       lambda p:p.update(revision=True)]:
            value=plan(); mutate(value)
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                reader.validate_plan(value)

    def test_expected_candidate_fields_are_exact(self):
        values=reader.expected_fields(plan())
        self.assertEqual(values['beamiter/forge.candidate'], '%040x' % 3)
        self.assertEqual(values['identity'], 'mission-exact-id')
        self.assertEqual(values['beamiter/anvil.required_core_pin'],reader.CORE)

    def test_candidates_require_visible_correct_repository_and_field(self):
        p=plan(); repo=p['repositories'][1]; candidate=repo['candidate']; name=repo['identity']['full_name']
        self.assertEqual(reader.bound_candidates(name+'\nCandidate: '+candidate,p),{name})
        self.assertEqual(reader.bound_candidates(name+'\nCandidate\n'+candidate,p),{name})
        self.assertEqual(reader.bound_candidates(name+'\nBaseline: '+candidate,p),set())
        self.assertEqual(reader.bound_candidates('Candidate: '+candidate,p),set())
        self.assertEqual(reader.bound_candidates('beamiter/forge\nCandidate: '+candidate,p),set())

    def test_exact_match_rejects_truncated_hash_substring_and_extra_revision_digit(self):
        sha='a'*40
        self.assertFalse(reader.exact_visible('Candidate: '+sha[:12],sha))
        self.assertFalse(reader.exact_visible('Candidate: '+sha+'b',sha))
        self.assertFalse(reader.exact_visible('mission-exact-id-wrong','mission-exact-id'))
        self.assertTrue(reader.exact_visible('Candidate: '+sha,sha))


class PermissionTests(unittest.TestCase):
    def test_exact_allowlist_rejects_all_mutations(self):
        forbidden=['Create Mission','Save acceptance plan','Freeze observed candidates','Use observed commits',
                   'Link selected Task run','Start','Validate','New terminal','Keep my draft against current revision']
        for app in reader.APPS:
            for label in forbidden:
                for purpose in ('open','menu','select','refresh','inspect','expand','close_mission'):
                    self.assertFalse(reader.allowed_control(app,label,plan(),purpose),(app,label,purpose))

    def test_readonly_inputs_and_selection_are_exact(self):
        for app in reader.APPS:
            self.assertTrue(reader.allowed_control(app,reader.INSPECT[app],plan(),'inspect'))
            self.assertTrue(reader.allowed_control(app,reader.REFRESH[app],plan(),'refresh'))
            self.assertFalse(reader.allowed_control(app,'OtherMission · Not inspected',plan(),'select'))
        self.assertTrue(reader.allowed_control('anvil','Missions · 2 need review',plan(),'open'))
        self.assertFalse(reader.allowed_control('anvil','Missions · 2 need review bogus',plan(),'open'))
        self.assertTrue(reader.allowed_control('forge','＋ New',plan(),'menu'))
        self.assertFalse(reader.allowed_control('forge','New terminal',plan(),'menu'))

    def test_config_requires_exact_supported_safe_schema(self):
        with tempfile.TemporaryDirectory() as td:
            path=Path(td)/'config.toml'
            base='ai_enabled=false\neva_companion_enabled=false\nagent_workspace_enabled=true\nshell="/bin/bash"\nterminal_mode="block"\n'
            for app in reader.APPS:
                extra='jsh_update_check="never"\n' if app in ('anvil','forge') else 'restore_session=false\ninitial_width=1200\ninitial_height=850\n'
                if app=='frost': extra+='inspector_width=520.0\n'
                path.write_text(base+extra); self.assertEqual(len(reader.safe_config(app,path)),64)
                for bad in [(base+extra).replace('ai_enabled=false','ai_enabled=true'),
                            (base+extra).replace('ai_enabled=false','ai_enabled=0'),
                            base+extra+'unknown_hook="bad"\n',base+extra+'[provider]\nenabled=true\n',
                            (base+extra).replace('shell="/bin/bash"','shell="/tmp/unsafe"')]:
                    path.write_text(bad)
                    with self.assertRaises(RuntimeError): reader.safe_config(app,path)

    def test_generated_config_disables_ai_eva_without_startup_commands(self):
        with tempfile.TemporaryDirectory() as td:
            root=Path(td); (root/'evidence').mkdir()
            (root/'evidence/native-seed.json').write_text(json.dumps({'mission':plan()}))
            host=root/'host';host.write_bytes(b'not an executable')
            for app in reader.APPS:
                binary=root/'installed'/app/'.local/bin'/app
                binary.parent.mkdir(parents=True);binary.write_bytes(b'not an executable')
            env={key:td for key in reader.ENV_KEYS}
            env.update(XDG_CONFIG_HOME=str(root/'config'),JTERM_WORKSPACE_STATE_DIR=str(root/'state'))
            with patch.dict(os.environ,env,clear=True):
                provenance.reader_manifest(root,root/'state',host)
            for app in reader.APPS:
                config=root/'config'/app/'config.toml';original=config.read_text()
                reader.safe_config(app,config)
                parsed=reader.tomllib.loads(original)
                self.assertIs(parsed['ai_enabled'],False)
                self.assertIs(parsed['eva_companion_enabled'],False)
                self.assertNotIn('startup_commands',parsed)
                for invalid in ('startup_commands=""\n','startup_commands="echo not-allowed"\n'):
                    config.write_text(original+invalid)
                    with self.assertRaises(RuntimeError):reader.safe_config(app,config)

    def test_manifest_does_not_allow_ambient_injection(self):
        with tempfile.TemporaryDirectory() as td:
            os.chmod(td,0o700)
            source={k:td for k in reader.ENV_KEYS}
            source.update(DISPLAY=':94',DBUS_SESSION_BUS_ADDRESS='unix:path=/tmp/private-bus')
            with patch.dict(os.environ,{'OPENAI_API_KEY':'not-a-real-secret','LD_PRELOAD':'/tmp/not-real','BASH_ENV':'/tmp/not-real'}):
                env=reader.safe_environment({'environment':source},td)
            for key in ('OPENAI_API_KEY','LD_PRELOAD','BASH_ENV'):
                self.assertNotIn(key,env)
            source['BASH_ENV']='unsafe'
            with self.assertRaises(RuntimeError): reader.safe_environment({'environment':source},td)


class ObservationTests(unittest.TestCase):
    def test_owned_connected_tree_supports_gtk_and_rejects_foreign_nodes(self):
        observer.validate_tree(tree())
        for field,value in [('owner',':1.99'),('pid',99),('parent','missing')]:
            t=tree();t['nodes'][1][field]=value
            with self.assertRaises(RuntimeError): observer.validate_tree(t)
        t=tree();t['nodes'][0]['children'].append(t['nodes'][1]['path'])
        with self.assertRaises(RuntimeError): observer.validate_tree(t)

    def test_visible_requires_full_bounds_showing_and_clipping(self):
        t=tree(); nodes=observer.validate_tree(t); n=t['nodes'][1]
        self.assertTrue(observer.visible(n,nodes,[0,0,100,100]))
        n['rect']=[90,90,20,20]
        self.assertFalse(observer.visible(n,nodes,[0,0,100,100]))
        n['rect']=[10,10,20,20];n['states'][0] &= ~(1<<25)
        self.assertFalse(observer.visible(n,nodes,[0,0,100,100]))

    def test_hidden_text_does_not_enter_coverage(self):
        with tempfile.TemporaryDirectory() as td:
            r=reader.Reader('anvil',{}, {},plan(),None,Path(td))
            r.absorb({'visible_text':'A different Mission','evidence':'actual-frame',
                      'nodes':[{'text':plan()['id']}]},'initial')
            self.assertNotIn('identity',r.coverage['fields'])
            r.absorb({'visible_text':plan()['id']+' · revision 21','evidence':'other-frame'},'initial')
            self.assertNotIn('revision',r.coverage['fields'])

    def test_ocr_exact_confidence_and_line_boundaries(self):
        def word(text,x,conf=90,line='1'):
            return dict(text=text,left=x,top=10,width=20,height=10,conf=conf,page_num='1',block_num='1',par_num='1',line_num=line)
        words=[word('Inspect',10),word('delivery',40)]
        self.assertEqual(observer.phrases(words,'Inspect delivery'),[[10,10,50,10]])
        words[1]['conf']=79
        self.assertEqual(observer.phrases(words,'Inspect delivery'),[])
        words[1]['conf']=90;words[1]['line_num']='2'
        self.assertEqual(observer.phrases(words,'Inspect delivery'),[])

    def test_ocr_ambiguity_is_preserved_not_guessed(self):
        words=[]
        for line in ('1','2'):
            words.extend([dict(text='Refresh',left=1,top=10*int(line),width=40,height=10,conf=99,
                               page_num='1',block_num='1',par_num='1',line_num=line)])
        self.assertEqual(len(observer.phrases(words,'Refresh')),2)

    def test_static_warning_is_not_a_visible_reason(self):
        with tempfile.TemporaryDirectory() as td:
            r=reader.Reader('anvil',{}, {},plan(),None,Path(td))
            r.absorb({'visible_text':'Needs you · Saved plans are not proof of publication or validation.','evidence':'frame'},'initial')
            self.assertEqual(r.coverage['reasons'],[])
            r.absorb({'visible_text':'beamiter/anvil · Run the required validation · validation','evidence':'reason-frame'},'initial')
            self.assertEqual(len(r.coverage['reasons']),1)

    def test_frost_missing_id_stays_missing(self):
        with tempfile.TemporaryDirectory() as td:
            r=reader.Reader('frost',{}, {},plan(),None,Path(td))
            r.absorb({'visible_text':'Plan revision 2 SharedMissionAcceptance','rows':[], 'evidence':'real-frame'},'initial')
            self.assertNotIn('identity',r.coverage['fields'])


class FailClosedInspectionTests(unittest.TestCase):
    def complete(self, app='anvil'):
        stages=['initial','inspected','refreshed','app_reopened']
        if app!='forge': stages.append('mission_reopened')
        records=lambda: [dict(stage=s,evidence='actual-'+s) for s in stages]
        return dict(fields={},identity_bindings=records(),
                    candidate_bindings={r['identity']['full_name']:records() for r in plan()['repositories']},
                    core_pin_bindings={r['identity']['full_name']:records() for r in plan()['repositories'] if r['required_core_pin']},
                    stages=dict(inspected='scan_complete',mission_close=True if app!='forge' else 'not_proven_no_close_control',
                        inspection_ready={'status':'settled'},freeze_after_refresh={'status':'disabled'}),
                    reasons=[{'stage':'inspected','text':'Run the required validation'}],inspection_status=[{'stage':'inspected'}],
                    inspection_errors=[],inspection_validation={r['identity']['full_name']:[dict(stage='inspected',evidence='current')]
                        for r in plan()['repositories']})

    def test_complete_contract_and_forge_surface_exception(self):
        for app in reader.APPS:
            self.assertEqual(reader.missing_coverage(app,plan(),self.complete(app)),[])

    def test_mission_reopen_wrong_or_blank_cannot_borrow_initial_evidence(self):
        coverage=self.complete()
        coverage['identity_bindings']=[e for e in coverage['identity_bindings'] if e['stage']!='mission_reopened']
        self.assertIn('mission_reopened.identity_revision_binding',reader.missing_coverage('anvil',plan(),coverage))
        coverage=self.complete()
        for name in coverage['candidate_bindings']:
            coverage['candidate_bindings'][name]=[e for e in coverage['candidate_bindings'][name] if e['stage']!='mission_reopened']
        self.assertEqual(sum('.candidate_binding' in e for e in reader.missing_coverage('anvil',plan(),coverage)),5)

    def test_every_reopen_requires_each_repo_candidate_and_core_pin(self):
        for stage in ('initial','inspected','refreshed','app_reopened','mission_reopened'):
            coverage=self.complete('ember')
            name='beamiter/ember'
            coverage['candidate_bindings'][name]=[e for e in coverage['candidate_bindings'][name] if e['stage']!=stage]
            coverage['core_pin_bindings'][name]=[e for e in coverage['core_pin_bindings'][name] if e['stage']!=stage]
            missing=reader.missing_coverage('ember',plan(),coverage)
            self.assertIn(stage+'.'+name+'.candidate_binding',missing)
            self.assertIn(stage+'.'+name+'.common_core_pin_binding',missing)

    def test_freeze_not_observed_fails_three_atspi_apps_only(self):
        for app in reader.APPS:
            coverage=self.complete(app);coverage['stages']['freeze_after_refresh']={'status':'not_observed'}
            self.assertEqual('refresh_freeze_disabled' in reader.missing_coverage(app,plan(),coverage),app!='frost')
            with tempfile.TemporaryDirectory() as td:
                r=reader.Reader(app,{}, {},plan(),None,Path(td))
                r.scroll=lambda amount:None
                r.observe=lambda *args,**kw:dict(rows=[],scoped_nodes=[],evidence='observed-absent')
                if app=='frost': r.freeze_after_refresh()
                elif app=='ember':
                    from test_reader_navigation import EmberSimulation
                    sim=EmberSimulation(); original=sim.observe
                    def absent(*args,**kw):
                        o=original();o['scoped_nodes']=[n for n in o['scoped_nodes'] if n['path']!='/freeze'];return o
                    sim.r.observe=absent
                    with self.assertRaisesRegex(RuntimeError,'No unique navigation control'):sim.r.freeze_after_refresh()
                    sim.r.click.assert_not_called()
                else:
                    with self.assertRaisesRegex(RuntimeError,'not observed'): r.freeze_after_refresh()

    def test_enabled_or_ambiguous_freeze_cannot_pass(self):
        for app in ('anvil','forge','ember'):
            if app=='ember':
                from test_reader_navigation import EmberSimulation
                for duplicate in (False,True):
                    sim=EmberSimulation(1440); original=sim.observe
                    def enabled(*args,**kw):
                        o=original();n=next(n for n in o['scoped_nodes'] if n['path']=='/freeze')
                        n['states'][0]|=(1<<8)|(1<<24)
                        if duplicate:o['scoped_nodes'].append(dict(n,path='/other-freeze'))
                        return o
                    sim.r.observe=enabled
                    message='No unique navigation control' if duplicate else 'did not visibly disable'
                    with self.assertRaisesRegex(RuntimeError,message):sim.r.freeze_after_refresh()
                    sim.r.click.assert_not_called()
                continue
            with tempfile.TemporaryDirectory() as td:
                r=reader.Reader(app,{}, {},plan(),None,Path(td));r.scroll=lambda amount:None
                label={'anvil':'Freeze inspected commits','forge':'Freeze inspected clean commits as candidates','ember':'Freeze observed candidates'}[app]
                n=node('/button',name=label,visible=True)
                r.observe=lambda *args,**kw:dict(scoped_nodes=[n],evidence='enabled')
                with self.assertRaisesRegex(RuntimeError,'did not visibly disable'):r.freeze_after_refresh()
                r.observe=lambda *args,**kw:dict(scoped_nodes=[n,n],evidence='ambiguous')
                with self.assertRaisesRegex(RuntimeError,'Ambiguous'):r.freeze_after_refresh()

    def test_scan_complete_and_initial_reason_cannot_prove_inspection(self):
        coverage=self.complete()
        for records in coverage['inspection_validation'].values():records[0]['stage']='initial'
        coverage['reasons']=[dict(stage='initial',text='Inspect current delivery')]
        missing=reader.missing_coverage('anvil',plan(),coverage)
        self.assertEqual(sum('missing_validation_evidence' in e for e in missing),5)
        for kind in ('inspection_status','inspection_errors'):
            coverage=self.complete()
            coverage[kind]=[] if kind=='inspection_status' else [{'problem':'Mission request: timeout'}]
            self.assertTrue(reader.missing_coverage('anvil',plan(),coverage))
        coverage=self.complete();coverage['stages'].pop('inspection_ready')
        self.assertIn('inspection_settled',reader.missing_coverage('anvil',plan(),coverage))

    def test_validation_reason_and_not_run_bind_exact_repo_check_environment(self):
        p=plan();name='beamiter/anvil';env=p['required_environment']
        forms=[name+' · Run the required validation · validation',
               'Run the required validation · '+name,
               p['title']+' · revision 2 · Run the required validation\n'+name+' · validation · environment '+env,
               name+'\nvalidation · '+env+' · Not run',
               name+'\nValidation · validation · '+env+'\nNot run']
        for text in forms:
            self.assertEqual(reader.inspection_validation(text,p)[0]['repository'],name)
        for text in ['Inspect current delivery', 'Not run',name+'\nvalidation · WRONG · Not run',
                     'beamiter/unknown · Run the required validation',
                     p['title']+' · revision 3 · Run the required validation\n'+name+' · validation · environment '+env,
                     name+'\nbeamiter/unknown\nvalidation · '+env+' · Not run']:
            self.assertEqual(reader.inspection_validation(text,p),[],text)

    def test_busy_error_or_stale_rejects_even_real_not_run_text(self):
        text='beamiter/anvil\nvalidation · '+plan()['required_environment']+' · Not run'
        for obstacle in (*reader.INSPECTION_BUSY,*reader.INSPECTION_INVALID):
            self.assertEqual(reader.inspection_validation(text+'\n'+obstacle,plan()),[])

    def test_identity_and_revision_must_belong_to_same_visible_block(self):
        p=plan()
        for text in [p['id']+' · revision 2', '"id": "'+p['id']+'",\n"revision": 2,',
                     'Mission ID: '+p['id']+'\nPlan revision 2']:
            self.assertTrue(reader.bound_identity(text,p),text)
            self.assertFalse(reader.bound_identity(text.replace('2','3'),p))
        self.assertFalse(reader.bound_identity(p['id']+' · revision 21',p))
        self.assertFalse(reader.bound_identity('OtherMission · revision 2\n'+p['id'],p))
        self.assertFalse(reader.bound_identity('wrong'+p['id']+' · revision 2',p))
        self.assertFalse(reader.bound_identity(p['id']+' · revision 2abc',p))
        self.assertFalse(reader.bound_identity('Mission ID: '+p['id']+'\nPlan revision 2wrong',p))
        self.assertFalse(reader.bound_identity('"id": "'+p['id']+'",\n"revision": 2abc',p))

    def test_unbound_same_sha_values_are_supplementary_not_field_proof(self):
        p=plan();repo=p['repositories'][1];repo['baseline']=repo['candidate']
        with tempfile.TemporaryDirectory() as td:
            r=reader.Reader('anvil',{}, {},p,None,Path(td))
            r.absorb(dict(visible_text='beamiter/anvil\nCandidate: '+repo['candidate'],evidence='candidate-only'),'initial')
            self.assertNotIn('beamiter/anvil.baseline',r.coverage['fields'])
            self.assertEqual(r.coverage['supplementary_values']['beamiter/anvil.baseline']['status'],
                             'value_observed_unbound_not_field_proof')
            self.assertEqual(r.coverage['core_pin_bindings'],{})

    def test_core_pins_require_actual_repo_and_explicit_labels(self):
        p=plan();core=reader.CORE
        self.assertEqual(reader.bound_core_pins('beamiter/anvil\nRequired core: '+core,p),
                         {'beamiter/anvil':'explicit_required_core'})
        self.assertEqual(reader.bound_core_pins('beamiter/forge\nCargo manifest pin: '+core+'\nCargo lock pin: '+core,p),
                         {'beamiter/forge':'observed_manifest_and_lock_core'})
        for text in [core, 'beamiter/anvil\nCandidate: '+core,
                     'beamiter/anvil\nCargo manifest pin: '+core,
                     'beamiter/anvil\nbeamiter/unknown\nRequired core: '+core]:
            self.assertEqual(reader.bound_core_pins(text,p),{},text)

    def test_notice_free_status_requires_actual_source_defined_status(self):
        p=plan()
        from test_status_scope import status_fixture
        self.assertTrue(reader.clean_inspection_status('anvil',status_fixture('anvil'),p))
        self.assertTrue(reader.clean_inspection_status('forge',status_fixture('forge'),p))
        self.assertFalse(reader.clean_inspection_status('forge',dict(visible_text='Mission request: failed'),p))
        refresh=node('/refresh',name='Refresh saved Missions',visible=True,rect=[0,10,100,20],visible_text='Refresh saved Missions')
        catalog=node('/catalog',name=p['title']+' · Needs attention',visible=True,rect=[0,60,100,20],visible_text=p['title'])
        good=dict(visible_text='',scoped_nodes=[refresh,catalog])
        self.assertTrue(reader.clean_inspection_status('ember',good,p))
        unknown=node('/notice',role=29,visible=True,rect=[0,35,100,20],visible_text='An unfamiliar host failure')
        self.assertFalse(reader.clean_inspection_status('ember',dict(good,scoped_nodes=[refresh,unknown,catalog]),p))


    def test_pending_old_inspection_can_settle_but_settled_error_fails(self):
        with tempfile.TemporaryDirectory() as td:
            r=reader.Reader('ember',{}, {},plan(),None,Path(td))
            target={'label':reader.INSPECT['ember'],'rect':[10,20,100,20],'node':'/inspect'}
            frames=[dict(visible_text='Saved observation requires reinspection',choices=[],evidence='pending'),
                    dict(visible_text='',choices=[target],evidence='ready1'),
                    dict(visible_text='',choices=[target],evidence='ready2')]
            r.targets=lambda obs,purpose:obs['choices']
            with patch.object(r,'observe',side_effect=frames),patch('reader.time.sleep'):
                r.wait_inspection_settled()
            self.assertEqual(r.coverage['stages']['inspection_ready']['evidence'],['ready1','ready2'])
            with patch.object(r,'observe',return_value=dict(visible_text='Mission request: failed',choices=[target],evidence='failed')):
                with self.assertRaisesRegex(RuntimeError,'Inspection rejected'):r.wait_inspection_settled()

    def test_frost_catalog_binding_excludes_duplicate_editable_title(self):
        p=plan();rows=[]
        def line(text,y):
            for i,word in enumerate(text.split()):
                rows.append(dict(text=word,left=10+i*80,top=y,width=70,height=10,conf=95,
                                 page_num='1',block_num='1',par_num='1',line_num=str(y)))
        line('Needs you: unfinished repositories',20)
        line(p['title'],60)
        line('Mission ID: '+p['id'],100)
        line('Plan revision 2',130)
        line(p['title'],180)
        self.assertEqual(reader.frost_catalog_bounds(rows,p)[0][1],60)
        self.assertTrue(reader.clean_inspection_status('frost',dict(visible_text='',rows=rows),p))
        line('Unfamiliar failure notice',40)
        self.assertFalse(reader.clean_inspection_status('frost',dict(visible_text='',rows=rows),p))

    def test_frost_config_requires_exact_supported_float_width(self):
        with tempfile.TemporaryDirectory() as td:
            path=Path(td)/'config.toml'
            base='ai_enabled=false\neva_companion_enabled=false\nagent_workspace_enabled=true\nshell="/bin/bash"\nterminal_mode="block"\nrestore_session=false\ninitial_width=1200\ninitial_height=850\n'
            for extra in ('','inspector_width=288.0\n','inspector_width=520\n'):
                path.write_text(base+extra)
                with self.assertRaises(RuntimeError):reader.safe_config('frost',path)
            path.write_text(base+'inspector_width=520.0\n')
            reader.safe_config('frost',path)

    def test_password_text_role_is_never_an_input_control(self):
        with tempfile.TemporaryDirectory() as td:
            r=reader.Reader('forge',{}, {},plan(),None,Path(td))
            n=node('/password',name=reader.INSPECT['forge'],role=40,visible=True)
            self.assertEqual(r.targets({'scoped_nodes':[n]},'inspect'),[])

    def test_ember_content_excludes_titlebar_but_real_close_stays_visible(self):
        root='/org/a11y/atspi/accessible/root';main=root+'/main';frame=root+'/mission';bar=frame+'/bar'
        content=frame+'/content';close=bar+'/close'
        t=dict(pid=21,owner=':1.20',root=root,nodes=[
            node(root,children=[main],role=75,rect=[0,0,800,600]),
            node(main,root,children=[frame],role=23,rect=[0,0,800,600]),
            node(frame,main,children=[bar,content],role=23,name=reader.FRAME['ember'],rect=[20,20,700,500]),
            node(bar,frame,children=[close],role=67,rect=[20,20,700,30]),
            node(content,frame,role=29,rect=[30,25,500,20]),
            node(close,bar,name='Close',rect=[690,25,20,20])])
        nodes=observer.validate_tree(t)
        scope=observer.mission_content_scope(t,reader.FRAME['ember'],[0,0,800,600])
        self.assertEqual(scope['content_rect'],[20,50,700,462])
        for path,expected in ((content,False),(close,True)):
            bounds=observer.node_observation_bounds(nodes[path],nodes,[0,0,800,600],scope)
            self.assertEqual(observer.visible(nodes[path],nodes,bounds),expected)
        t['nodes'][2]['parent']=root
        with self.assertRaises(RuntimeError):observer.mission_content_scope(t,reader.FRAME['ember'],[0,0,800,600])


class NativeStartupTests(unittest.TestCase):
    """Replay the installed xdotool 3.20160805.1 output/exit contracts.

    cmd_search.c defaults SEARCH_ANY; xdo_search.c visits the unnamed root
    first and matches its empty name to .*. cmd_getwindowpid.c returns exit 1
    for that root. No X server, app, host, shell or provider is started here.
    """
    def setUp(self):
        temporary=tempfile.TemporaryDirectory();self.addCleanup(temporary.cleanup)
        self.out=Path(temporary.name)
        self.identity=dict(pid=21,start_ticks='100',executable='/tmp/installed/anvil',state_dir='/tmp/shared')
        self.process=Mock(pid=21);self.process.poll.return_value=None
        self.host=Mock()
        self.r=reader.Reader('anvil',dict(binary=self.identity['executable'],sha256='a'*64,
                                        config='/tmp/config.toml'),
                             dict(HOME='/tmp/fresh',JTERM_WORKSPACE_STATE_DIR='/tmp/shared'),
                             plan(),self.host,self.out)
        self.r.process=self.process;self.r.process_identity=self.identity.copy()
        self.calls=[];self.search_count=0;self.empty_searches=0
        self.window_ids=['4194307'];self.window_pids={'4194307':21};self.active='4194307'
        self.names={'4194307':'Anvil'};self.geometry_overrides={}
        self.addCleanup(lambda:self.r.log.close() if self.r.log and not self.r.log.closed else None)

    def tool(self,command,**kwargs):
        self.calls.append(command)
        out='';err='';code=0
        if command[0]=='import':
            Path(command[-1]).write_bytes(b'diagnostic screenshot stub, not UI evidence')
        elif command[1]=='search':
            self.search_count+=1
            if self.search_count<=self.empty_searches:
                code=1
            else:
                # 0x21f is the real CI Xvfb root, serialized as decimal by xdotool.
                ids=self.window_ids if '--all' in command else ['543',*self.window_ids]
                out=''.join(wid+'\n' for wid in ids)
                code=0 if ids else 1
        elif command[1]=='getwindowpid':
            pid=self.window_pids.get(command[-1])
            if pid is None:
                code=1;err='window %s has no pid associated with it.\n' % command[-1]
            else:out=str(pid)+'\n'
        elif command[1]=='getwindowname':out=self.names.get(command[-1],'Other owned window')+'\n'
        elif command[1]=='getwindowgeometry':
            wid=command[-1]
            out=self.geometry_overrides.get(wid,'WINDOW=%s\nX=80\nY=40\nWIDTH=1200\nHEIGHT=850\nSCREEN=0\n' % wid)
        elif command[1]=='getactivewindow':out=self.active+'\n'
        else:raise AssertionError('Unexpected command: '+repr(command))
        return reader.subprocess.CompletedProcess(command,code,out,err)

    def launch(self):
        with patch('reader.sha256',return_value='a'*64),patch('reader.safe_config'), \
             patch('reader.subprocess.Popen',return_value=self.process) as spawn,patch('reader.time.sleep'):
            self.r.launch(1)
        spawn.assert_called_once()

    def test_pid_and_name_search_is_conjunctive_then_owner_and_active_are_rechecked(self):
        for app in reader.APPS:
            self.r.app=app;self.calls=[]
            with patch('reader.proc_identity',return_value=self.identity), \
                 patch('reader.subprocess.run',side_effect=self.tool):
                self.r.bind_window()
            self.assertEqual(self.calls[0],['xdotool','search','--all','--onlyvisible','--pid','21','--name','.*'])
            self.assertIn(['xdotool','getwindowpid','4194307'],self.calls)
            self.assertIn(['xdotool','getactivewindow'],self.calls)
            self.assertNotIn(['xdotool','getwindowpid','543'],self.calls)
            self.assertEqual(self.r.window,'4194307')
            self.assertEqual(self.r.window_rect,[80,40,1200,850])

    def test_real_shell_geometry_output_is_data_and_bound_to_window(self):
        raw='WINDOW=4194307\nX=-8\nY=40\nWIDTH=1200\nHEIGHT=850\nSCREEN=0\n'
        self.assertEqual(reader.window_geometry(raw,'4194307'),[-8,40,1200,850])
        for invalid in [raw.replace('WINDOW=4194307','WINDOW=99'),raw.replace('WIDTH=1200','WIDTH=0'),
                        raw+'X=1\n',raw.replace('SCREEN=0\n',''),raw+'HOOK=$(touch /tmp/unsafe)\n',
                        raw.replace('Y=40','Y=not-a-number')]:
            with self.subTest(raw=invalid),self.assertRaises(RuntimeError):
                reader.window_geometry(invalid,'4194307')

    def test_search_pid_change_and_pidless_window_never_bind(self):
        for value,message in [(99,'foreign PID'),(None,'has no pid associated')]:
            self.window_pids['4194307']=value
            with patch('reader.proc_identity',return_value=self.identity), \
                 patch('reader.subprocess.run',side_effect=self.tool):
                with self.assertRaisesRegex(RuntimeError,message):self.r.bind_window()
            self.assertIsNone(self.r.window)
            self.assertEqual(self.r.inputs,[])

    def test_bad_or_duplicate_search_ids_fail_closed(self):
        for ids in [['4194307','4194307'],['not-a-window'],['0'],['-1']]:
            self.window_ids=ids
            with patch('reader.proc_identity',return_value=self.identity), \
                 patch('reader.subprocess.run',side_effect=self.tool):
                with self.assertRaises(RuntimeError):self.r.bind_window()
            self.assertIsNone(self.r.window)

    def test_missing_ambiguous_small_or_inactive_window_does_not_bind(self):
        with patch('reader.proc_identity',return_value=self.identity), \
             patch('reader.subprocess.run',side_effect=self.tool):
            self.window_ids=[]
            with self.assertRaises(reader.WindowNotReady):self.r.bind_window()
            self.window_ids=['4194307','4194308'];self.window_pids['4194308']=21
            with self.assertRaisesRegex(RuntimeError,'2 candidates'):self.r.bind_window()
            self.window_ids=['4194307'];self.active='543'
            with self.assertRaisesRegex(reader.WindowNotReady,'not the active'):self.r.bind_window()
            self.active='4194307'
            self.geometry_overrides['4194307']='WINDOW=4194307\nX=0\nY=0\nWIDTH=1\nHEIGHT=1\nSCREEN=0\n'
            with self.assertRaisesRegex(reader.WindowNotReady,'0 candidates'):self.r.bind_window()
        self.assertIsNone(self.r.window);self.assertEqual(self.r.inputs,[])

    def test_anvil_separate_mission_window_still_requires_exact_active_frame(self):
        self.window_ids=['4194307','4194308'];self.window_pids['4194308']=21
        self.names['4194308']=reader.FRAME['anvil']
        with patch('reader.proc_identity',return_value=self.identity), \
             patch('reader.subprocess.run',side_effect=self.tool):
            self.r.bind_window();self.assertEqual(self.r.window,'4194307')
            self.r.mission_open=True
            with self.assertRaisesRegex(reader.WindowNotReady,'not the active'):self.r.bind_window()
            self.active='4194308';self.r.bind_window();self.assertEqual(self.r.window,'4194308')

    def test_launch_retries_empty_search_without_replacing_process_identity(self):
        self.empty_searches=2
        with patch('reader.proc_identity',return_value=self.identity), \
             patch('reader.subprocess.run',side_effect=self.tool):self.launch()
        self.assertEqual(self.search_count,3)
        self.assertEqual(self.r.process_identity,self.identity)
        self.assertEqual(self.r.coverage['stages']['launch_1']['identity'],self.identity)
        self.assertEqual(self.r.inputs,[])

    def test_exhausted_startup_preserves_last_error_and_guarded_diagnostic(self):
        self.active='543'
        with patch('reader.proc_identity',return_value=self.identity), \
             patch('reader.subprocess.run',side_effect=self.tool):
            with self.assertRaisesRegex(RuntimeError,'startup bound; last failure: .*not the active'):self.launch()
        report=json.loads((self.out/'startup-1.json').read_text())
        self.assertEqual(len(report['attempts']),100)
        self.assertEqual(report['window_probe']['active_window'],543)
        self.assertEqual(report['last_command']['argv'],['xdotool','getactivewindow'])
        self.assertTrue((self.out/report['screenshot']).is_file())
        self.assertEqual(self.r.coverage['stages'],{})
        self.assertEqual(self.r.inputs,[])

    def test_tool_failure_details_survive_failed_screenshot(self):
        self.window_pids['4194307']=None
        def failing_capture(command,**kwargs):
            if command[0]=='import':return reader.subprocess.CompletedProcess(command,1,'','capture unavailable')
            return self.tool(command,**kwargs)
        with patch('reader.proc_identity',return_value=self.identity), \
             patch('reader.subprocess.run',side_effect=failing_capture):
            with self.assertRaisesRegex(RuntimeError,'getwindowpid.*has no pid associated'):self.launch()
        report=json.loads((self.out/'startup-1.json').read_text())
        self.assertEqual(report['last_command']['argv'],['xdotool','getwindowpid','4194307'])
        self.assertEqual(report['last_command']['returncode'],1)
        self.assertIn('has no pid associated',report['last_command']['stderr'])
        self.assertIn('capture unavailable',report['screenshot_error'])
        self.assertNotIn('screenshot',report)

    def test_changed_birth_executable_or_shared_state_is_fatal_without_retry_or_input(self):
        for field,value in [('start_ticks','101'),('executable','/tmp/replacement'),('state_dir','/tmp/other')]:
            with self.subTest(field=field):
                changed=dict(self.identity,**{field:value});self.calls=[]
                with patch('reader.proc_identity',side_effect=[self.identity,changed,changed]), \
                     patch('reader.subprocess.run',side_effect=self.tool):
                    with self.assertRaisesRegex(RuntimeError,'identity changed'):self.launch()
                report=json.loads((self.out/'startup-1.json').read_text())
                self.assertEqual(report['attempts'],[])
                self.assertEqual(report['process_identity'],self.identity)
                self.assertIn('identity changed',report['screenshot_error'])
                self.assertEqual(self.calls,[])
                self.r.log.close()

    def test_wrong_initial_executable_or_state_fails_before_window_search(self):
        for field,value,message in [('executable','/tmp/replacement','Wrong app executable'),
                                    ('state_dir','/tmp/other','Wrong shared state')]:
            with self.subTest(field=field),patch('reader.proc_identity',return_value=dict(self.identity,**{field:value})), \
                 patch('reader.subprocess.run',side_effect=self.tool):
                with self.assertRaisesRegex(RuntimeError,message):self.launch()
            self.assertEqual(self.calls,[])
            self.assertEqual(self.r.coverage['stages'],{})
            self.r.log.close()

    def test_host_failure_is_not_downgraded_to_a_window_retry(self):
        self.host.check.side_effect=[None,RuntimeError('Independent host identity changed'),RuntimeError('Independent host identity changed')]
        with patch('reader.proc_identity',return_value=self.identity), \
             patch('reader.subprocess.run',side_effect=self.tool):
            with self.assertRaisesRegex(RuntimeError,'Independent host identity changed'):self.launch()
        report=json.loads((self.out/'startup-1.json').read_text())
        self.assertEqual(report['attempts'],[])
        self.assertEqual(self.calls,[])

    def test_exited_app_fails_before_window_search(self):
        self.process.poll.side_effect=[None,9,9]
        with patch('reader.proc_identity',return_value=self.identity), \
             patch('reader.subprocess.run',side_effect=self.tool):
            with self.assertRaisesRegex(RuntimeError,'exited unexpectedly'):self.launch()
        self.assertEqual(self.calls,[])
        self.assertEqual(self.r.coverage['stages'],{})

    def test_hung_tool_records_exact_command_without_retry_or_budget_expansion(self):
        def timeout_search(command,**kwargs):
            if command[1]=='search':raise reader.subprocess.TimeoutExpired(command,kwargs['timeout'])
            return self.tool(command,**kwargs)
        with patch('reader.proc_identity',return_value=self.identity), \
             patch('reader.subprocess.run',side_effect=timeout_search):
            with self.assertRaisesRegex(RuntimeError,'timed out after 15 seconds: xdotool search'):self.launch()
        report=json.loads((self.out/'startup-1.json').read_text())
        self.assertEqual(report['attempts'],[])
        self.assertEqual(report['last_command']['status'],'timed_out')
        self.assertEqual(report['last_command']['timeout'],15)
        self.assertEqual(self.r.coverage['stages'],{})


class HostTests(unittest.TestCase):
    def test_proc_identity_reads_current_process_without_database(self):
        identity=reader.proc_identity(os.getpid())
        self.assertEqual(identity['pid'],os.getpid())
        self.assertTrue(identity['start_ticks'].isdigit())
        self.assertTrue(Path(identity['executable']).is_absolute())

    def test_host_guard_rejects_replaced_pid_birth_before_actions(self):
        expected=dict(pid=1,start_ticks='100',executable='/bin/jterm-sessiond',state_dir='/tmp/shared')
        with patch('reader.proc_identity',return_value=dict(expected,start_ticks='101')):
            with self.assertRaisesRegex(RuntimeError,'changed'):
                reader.HostGuard(expected,'a'*64)

    def test_preflight_failure_writes_all_four_not_run_without_launch(self):
        with tempfile.TemporaryDirectory() as td:
            root=Path(td); manifest=root/'manifest.json';receipt=root/'receipt.json';p=root/'plan.json'
            manifest.write_text('{}');receipt.write_text('{}');p.write_text(json.dumps(plan()))
            with patch('reader.subprocess.Popen') as spawn:
                status=reader.main(['--manifest',str(manifest),'--receipt',str(receipt),'--plan',str(p),
                                    '--state-dir',td,'--host-pid','1','--output',str(root/'output')])
            self.assertEqual(status,1);spawn.assert_not_called()
            result=json.loads((root/'output/result.json').read_text())
            self.assertEqual(set(result['applications']),set(reader.APPS))
            self.assertTrue(all(v['status']=='not_run' for v in result['applications'].values()))
            self.assertEqual(result['provider_execution'],'skipped_by_user')


if __name__=='__main__':
    unittest.main()
