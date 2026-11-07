#!/usr/bin/env python3
"""Source-structure status contracts, not production GUI observations."""
import copy
import json
import unittest
import reader
from test_reader import node,plan


def status_fixture(app):
    summary=('1 saved Missions · 1 need you' if app=='forge' else
             '1 Mission(s) need review. Only inspected, version-bound receipts count as passed.')
    app_root=node('/application',role=75,children=['/frame'])
    frame=node('/frame','/application',role=23,name=reader.FRAME.get(app,'Forge'),children=['/mission'],rect=[0,0,1200,850])
    names=(['Missions · linked repository delivery',
            "A Mission links existing tasks and exact repository candidates. Save edits only the plan. Inspection is read only. Start, validation, review and integration stay in each task's existing controls.",
            reader.REFRESH[app],'','New Mission',summary,'Needs you · 1 current reasons'] if app=='forge' else
           ['One goal, exact repositories and linked task runs. Inspection reads Git and saved evidence; it does not start work or publish code.',
            reader.REFRESH[app],'','Needs-you reasons for saved plans · read-only','',
            'Title, acceptance goal and required environment','','','',
            'Repository = exact absolute worktree path, one per line','','',summary,''])
    status_index=5 if app=='forge' else 12
    refresh_index=2 if app=='forge' else 1
    catalog_index=3 if app=='forge' else 2
    paths=['/status' if i==status_index else '/child%d'%i for i in range(len(names))]
    mission=node('/mission','/frame',role=20,children=paths,rect=[90,40,700,790])
    children=[]
    for i,name in enumerate(names):
        role=43 if i==refresh_index else 29
        if not name:role=20
        children.append(node(paths[i],'/mission',role=role,name=name,visible=True,rect=[100,60+i*30,600,24],
                             text=name,visible_text=name))
    selected=node('/selected',children[catalog_index]['path'],name=plan()['title']+' · revision 2',visible=True)
    children[catalog_index]['children']=['/selected']
    extra=[selected]
    if app=='anvil':
        children[11]['children']=['/inspect']
        extra.append(node('/inspect',children[11]['path'],name=reader.INSPECT[app],visible=True))
    body=node('/body','/mission',role=29,name='Example',visible=True,rect=[100,700,600,24],
              text='Example text: '+summary,visible_text='Example text: '+summary)
    # Body text is later, never allowed to replace the source-defined status.
    mission['children'].append('/body')
    nodes=[app_root,frame,mission,*children,*extra,body]
    return dict(root='/application',owner=':1.20',pid=21,window='42',window_rect=[0,0,1200,850],
                nodes=nodes,scoped_nodes=nodes[1:],visible_text=summary+'\n'+body['visible_text'],unknown_text=[])


class GtkStatusScopeTests(unittest.TestCase):
    def test_only_exact_source_status_complete_visible_text_is_positive(self):
        for app in ('anvil','forge'):
            o=status_fixture(app);status=reader.gtk_status_node(app,o)
            self.assertEqual(status['path'],'/status')
            self.assertTrue(reader.clean_inspection_status(app,o,plan()))
            self.assertFalse(reader.unknown_notices(app,o,plan()))
    def test_unknown_full_or_partial_status_cannot_be_completed_by_body_example(self):
        for app in ('anvil','forge'):
            for partial in (False,True):
                o=status_fixture(app);n=reader.gtk_status_node(app,o)
                if not partial:n['visible_text']=''
                else:n['text']+='\nUnrecognized failure'
                o['unknown_text']=[dict(path=n['path'],clip=n['rect'],status='ocr_partial' if partial else 'unavailable',unproven_lines=[1 if partial else 0])]
                self.assertTrue(reader.unknown_notices(app,o,plan()))
                self.assertFalse(reader.clean_inspection_status(app,o,plan()))
    def test_body_or_terminal_unknown_does_not_pollute_true_status(self):
        for app in ('anvil','forge'):
            for path in ('/body','/terminal'):
                o=status_fixture(app);o['unknown_text']=[dict(path=path,clip=[100,700,600,24],status='unavailable')]
                self.assertFalse(reader.unknown_notices(app,o,plan()))
                self.assertTrue(reader.clean_inspection_status(app,o,plan()))
    def test_body_summary_missing_prefix_partial_clipping_and_name_only_never_prove_status(self):
        for app in ('anvil','forge'):
            for mutation in ('body_only','prefix','partial','hidden','name_only','wrong_role'):
                o=status_fixture(app);n=reader.gtk_status_node(app,o)
                if mutation=='body_only':n['text']=n['visible_text']='Unfamiliar host error'
                if mutation=='prefix':n['text']=n['visible_text']='Example: '+n['text']
                if mutation=='partial':n['text']+='\nUnknown error'
                if mutation=='hidden':n['visible']=False
                if mutation=='name_only':n['visible_text']=''
                if mutation=='wrong_role':n['role']=79
                with self.subTest(app=app,mutation=mutation):self.assertFalse(reader.clean_inspection_status(app,o,plan()))
    def test_missing_or_reordered_structure_and_foreign_ancestry_never_bind(self):
        for app in ('anvil','forge'):
            for mutation in ('swap','duplicate','foreign_pid','foreign_owner','foreign_parent','missing_frame','missing_refresh'):
                o=status_fixture(app);root=next(n for n in o['nodes'] if n['path']=='/mission');status=reader.gtk_status_node(app,o)
                if mutation=='swap':root['children'][0],root['children'][-1]=root['children'][-1],root['children'][0]
                if mutation=='duplicate':root['children'].append(root['children'][0])
                if mutation=='foreign_pid':status['pid']=99
                if mutation=='foreign_owner':status['owner']=':1.99'
                if mutation=='foreign_parent':status['parent']='/body'
                if mutation=='missing_frame':next(n for n in o['nodes'] if n['path']=='/frame')['role']=20
                if mutation=='missing_refresh':next(n for n in o['nodes'] if n['name']==reader.REFRESH[app])['name']='Other'
                with self.subTest(app=app,mutation=mutation):
                    self.assertIsNone(reader.gtk_status_node(app,o))
                    self.assertFalse(reader.clean_inspection_status(app,o,plan()))
    def test_old_global_text_only_success_is_rejected(self):
        for app in ('anvil','forge'):
            o=status_fixture(app)
            self.assertFalse(reader.clean_inspection_status(app,dict(visible_text=o['visible_text']),plan()))
    def test_failure_details_are_retained_without_turning_failure_into_pass(self):
        error=RuntimeError('unsupported geometry')
        details=[dict(path='/status',method='GetRangeExtents',signature='iiu',error_name='org.freedesktop.DBus.Error.NotSupported')]
        error.atspi_diagnostics=details
        entry=dict(status='failed');reader.record_failure(entry,error)
        self.assertEqual(entry['status'],'failed');self.assertEqual(entry['atspi_diagnostics'],details)
        self.assertEqual(json.loads(json.dumps(entry))['atspi_diagnostics'],details)
        plain=dict(status='failed');reader.record_failure(plain,RuntimeError('plain'))
        self.assertEqual(plain,dict(status='failed',error='plain'))


if __name__=='__main__':unittest.main()
