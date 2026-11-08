"""Offline X11 parser contracts; no server, screenshot or provider is started."""
import unittest
import x11_geometry as geometry


def raw(wid=42, root=None, x=420, y=217, relative_x=1, relative_y=20,
        width=760, height=780, border=0, state='IsViewable', cls='InputOutput'):
    children = '' if root is None else ('  Root window id: %d (the root window)\n'
                                      '  Parent window id: 99 "window manager"\n' % root)
    return ('\nxwininfo: Window id: %d "fixture"\n\n%s'
            '  Absolute upper-left X:  %d\n  Absolute upper-left Y:  %d\n'
            '  Relative upper-left X:  %d\n  Relative upper-left Y:  %d\n'
            '  Width: %d\n  Height: %d\n  Border width: %d\n'
            '  Class: %s\n  Map State: %s\n') % (
            wid,children,x,y,relative_x,relative_y,width,height,border,cls,state)


ROOT = raw(wid=1,x=0,y=0,width=1600,height=1200,relative_x=0,relative_y=0)


class ClientGeometryTests(unittest.TestCase):
    def test_reparent_offset_is_not_counted_twice(self):
        rect,proof=geometry.client_geometry(raw(root=1),ROOT,42,[421,237,760,780])
        self.assertEqual(rect,[420,217,760,780])
        self.assertEqual(proof['client']['relative_y'],20)
        self.assertEqual(proof['xdotool_rect'],[421,237,760,780])

    def test_only_measured_border_is_added(self):
        rect,_=geometry.client_geometry(raw(root=1,border=2),ROOT,42,[999,999,760,780])
        self.assertEqual(rect,[422,219,760,780])

    def test_csd_zero_relative_offset_is_unchanged(self):
        rect,_=geometry.client_geometry(raw(root=1,x=122,y=248,relative_x=0,relative_y=0,
            width=1356,height=704),ROOT,42,[122,248,1356,704])
        self.assertEqual(rect,[122,248,1356,704])

    def test_missing_duplicate_malformed_fields_fail(self):
        original=raw(root=1)
        fields=('Window id: 42','Root window id: 1','Parent window id: 99',
                'Absolute upper-left X:  420','Absolute upper-left Y:  217',
                'Relative upper-left X:  1','Relative upper-left Y:  20',
                'Width: 760','Height: 780','Border width: 0','Class: InputOutput','Map State: IsViewable')
        for field in fields:
            line=next(line for line in original.splitlines() if field in line)
            for altered in (original.replace(line+'\n',''),original+'\n'+line+'\n',
                            original+'\n'+line.replace(field,field.replace(':',': invalid',1))+'\n',
                            original.replace(field,field.replace(':',': invalid',1))):
                with self.subTest(field=field,altered=altered),self.assertRaises(RuntimeError):
                    geometry.client_geometry(altered,ROOT,42,[421,237,760,780])

    def test_wrong_window_root_or_dimensions_fail(self):
        for source,root,wid,legacy in (
            (raw(root=1),ROOT,43,[0,0,760,780]),
            (raw(root=2),ROOT,42,[0,0,760,780]),
            (raw(wid=1,root=1),ROOT,1,[0,0,760,780]),
            (raw(root=1),ROOT,42,[0,0,761,780]),
            (raw(root=1),ROOT,42,[0,0,760,781]),
            (raw(root=1),ROOT.replace('Border width: 0','Border width: 1'),42,[0,0,760,780]),
            (raw(root=1),ROOT.replace('Absolute upper-left X:  0','Absolute upper-left X:  1'),42,[0,0,760,780])):
            with self.subTest(source=source,root=root,wid=wid),self.assertRaises(RuntimeError):
                geometry.client_geometry(source,root,wid,legacy)

    def test_unmapped_inputonly_zero_or_outside_root_fail(self):
        for kw in ({'state':'IsUnMapped'},{'state':'IsUnviewable'},{'cls':'InputOnly'},
                   {'width':0},{'height':0},{'border':-1},{'x':-1},{'y':-1},
                   {'x':841},{'y':421},{'x':840,'border':1}):
            source=raw(root=1,**kw)
            with self.subTest(kw=kw),self.assertRaises(RuntimeError):
                geometry.client_geometry(source,ROOT,42,[0,0,kw.get('width',760),kw.get('height',780)])

    def test_large_output_rejected_before_parsing(self):
        with self.assertRaisesRegex(RuntimeError,'output'):
            geometry.client_geometry(raw(root=1)+' '*131073,ROOT,42,[0,0,760,780])


    def test_protocol_range_and_root_relative_coordinates_are_not_coerced(self):
        for kw in ({'wid':0x100000000},{'root':0x100000000},{'width':65536},
                   {'height':65536},{'border':65536},{'x':32768},{'y':-32769},
                   {'relative_x':32768},{'relative_y':-32769}):
            values=dict(root=1);values.update(kw)
            with self.subTest(kw=kw),self.assertRaises(RuntimeError):
                geometry.client_geometry(raw(**values),ROOT,values.get('wid',42),
                    [0,0,values.get('width',760),values.get('height',780)])
        with self.assertRaises(RuntimeError):
            geometry.client_geometry(raw(root=1),ROOT.replace('Relative upper-left X:  0','Relative upper-left X:  1'),
                                     42,[0,0,760,780])


class PointerBindingTests(unittest.TestCase):
    def fixture(self):
        import copy
        from types import SimpleNamespace
        from unittest.mock import Mock
        import reader
        r=reader.Reader.__new__(reader.Reader)
        r.app='forge';r.plan={};r.process=SimpleNamespace(pid=21)
        r.window='42';r.window_title='Forge';r.window_rect=[420,217,760,780]
        _,proof=geometry.client_geometry(raw(root=1),ROOT,42,[421,237,760,780])
        r.window_probe=dict(candidates=[dict(window='42',x11_geometry=proof)])
        r.inputs=[];r.run=Mock();r.bind_window=Mock()
        observation=dict(evidence='second-frame',pid=21,window='42',window_title='Forge',
                         window_rect=r.window_rect[:],x11_geometry=copy.deepcopy(proof))
        target=dict(label=reader.REFRESH['forge'],rect=[430,230,200,30],node='/button')
        return r,observation,target

    def test_unchanged_complete_second_frame_allows_one_pointer(self):
        from unittest.mock import patch
        r,o,t=self.fixture()
        with patch('reader.time.sleep'):r.click(t,'refresh','second-frame',o)
        r.run.assert_called_once();self.assertEqual(len(r.inputs),1)

    def test_small_move_is_rejected_even_when_old_point_still_inside(self):
        r,o,t=self.fixture()
        r.bind_window.side_effect=lambda:r.window_rect.__setitem__(0,r.window_rect[0]+1)
        with self.assertRaisesRegex(RuntimeError,'binding changed'):r.click(t,'refresh','second-frame',o)
        r.run.assert_not_called();self.assertEqual(r.inputs,[])

    def test_identity_title_root_and_parent_changes_reject_before_pointer(self):
        for mutation in ('pid','window','title','root','parent'):
            r,o,t=self.fixture()
            def changed():
                if mutation=='pid':r.process.pid=22
                if mutation=='window':
                    r.window='43';r.window_probe['candidates'][0]['window']='43'
                if mutation=='title':r.window_title='Other'
                if mutation in ('root','parent'):
                    r.window_probe['candidates'][0]['x11_geometry']['client'][mutation]=999
            r.bind_window.side_effect=changed
            with self.subTest(mutation=mutation),self.assertRaisesRegex(RuntimeError,'binding changed'):
                r.click(t,'refresh','second-frame',o)
            r.run.assert_not_called();self.assertEqual(r.inputs,[])

    def test_missing_stale_or_partial_observation_never_sends_pointer(self):
        for mutation in ('missing','evidence','pid','window','window_title','window_rect','x11_geometry'):
            r,o,t=self.fixture()
            if mutation=='missing':o=None
            elif mutation=='evidence':o['evidence']='old-frame'
            else:del o[mutation]
            with self.subTest(mutation=mutation),self.assertRaisesRegex(RuntimeError,'complete current observation'):
                r.click(t,'refresh','second-frame',o)
            r.run.assert_not_called();self.assertEqual(r.inputs,[])

    def test_missing_current_geometry_never_sends_pointer(self):
        r,o,t=self.fixture();r.window_probe['candidates']=[]
        with self.assertRaisesRegex(RuntimeError,'missing X11'):r.click(t,'refresh','second-frame',o)
        r.run.assert_not_called();self.assertEqual(r.inputs,[])



if __name__=='__main__':
    unittest.main()
