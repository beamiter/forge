import copy
import importlib.util
from pathlib import Path
import unittest
p = Path(__file__).with_name('test-chat-render-matrix.py')
spec = importlib.util.spec_from_file_location('matrix', p)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class GeometryOracles(unittest.TestCase):
    def setup_geometry(self, scale=1.5):
        self.outputs = [dict(active=True, name='HEADLESS-1', scale=scale, transform='normal',
            current_mode=dict(width=2400, height=1800),
            rect=dict(x=0,y=0,width=int(2400/scale),height=int(1800/scale)))]
        self.node = dict(name='Forge F13 native render', pid=42, focused=True,
            border='none', id=5, rect=dict(x=10,y=10,width=480,height=640))
        self.tree = dict(floating_nodes=[self.node])
    def check(self, scale=1.5):
        return m.measured(self.outputs,self.tree,42,scale,[480,640])
    def test_all_actual_scales(self):
        for scale in (1,1.5,2):
            self.setup_geometry(scale)
            self.assertEqual(self.check(scale)['output_scale'],scale)
    def test_fractional_cannot_be_substituted_by_buffer_integer(self):
        self.setup_geometry();self.outputs[0]['scale']=2
        with self.assertRaises(RuntimeError):self.check()
    def test_rejects_wrong_physical_and_logical_sizes(self):
        for path in ['current_mode','rect']:
            self.setup_geometry();self.outputs[0][path]['width']+=1
            with self.assertRaises(RuntimeError):self.check()
    def test_rejects_wrong_owned_identity_focus_size_border(self):
        for key,value in [('pid',43),('focused',False),('border','normal'),('name','other')]:
            self.setup_geometry();self.node[key]=value
            with self.assertRaises(RuntimeError):self.check()
        self.setup_geometry();self.node['rect']['width']+=1
        with self.assertRaises(RuntimeError):self.check()
    def test_rejects_ambiguous_or_nonfloating_surface(self):
        self.setup_geometry();self.tree['floating_nodes'].append(copy.deepcopy(self.node))
        with self.assertRaises(RuntimeError):self.check()
        self.setup_geometry();self.tree={'nodes':[self.node]}
        with self.assertRaises(RuntimeError):self.check()
    def test_rejects_off_output_or_wrong_transform(self):
        self.setup_geometry();self.node['rect']['x']=1400
        with self.assertRaises(RuntimeError):self.check()
        self.setup_geometry();self.outputs[0]['transform']='90'
        with self.assertRaises(RuntimeError):self.check()

unittest.main()
