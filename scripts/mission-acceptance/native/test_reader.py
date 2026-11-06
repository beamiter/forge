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
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import reader
import observer


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
                extra='startup_commands=""\njsh_update_check="never"\n' if app in ('anvil','forge') else 'restore_session=false\ninitial_width=1200\ninitial_height=850\n'
                if app=='frost': extra+='inspector_width=520.0\n'
                path.write_text(base+extra); self.assertEqual(len(reader.safe_config(app,path)),64)
                for bad in [(base+extra).replace('ai_enabled=false','ai_enabled=true'),
                            (base+extra).replace('ai_enabled=false','ai_enabled=0'),
                            base+extra+'unknown_hook="bad"\n',base+extra+'[provider]\nenabled=true\n',
                            (base+extra).replace('shell="/bin/bash"','shell="/tmp/unsafe"')]:
                    path.write_text(bad)
                    with self.assertRaises(RuntimeError): reader.safe_config(app,path)

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
                else:
                    with self.assertRaisesRegex(RuntimeError,'not observed'): r.freeze_after_refresh()

    def test_enabled_or_ambiguous_freeze_cannot_pass(self):
        for app in ('anvil','forge','ember'):
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
        self.assertTrue(reader.clean_inspection_status('anvil',dict(visible_text='1 Mission(s) need review. Only inspected, version-bound receipts count as passed.'),p))
        self.assertTrue(reader.clean_inspection_status('forge',dict(visible_text='1 saved Missions · 1 need you'),p))
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
