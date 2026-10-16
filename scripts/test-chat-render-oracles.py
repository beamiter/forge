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


class HorizontalClearance(unittest.TestCase):
    def report(self, end=90, painted=True):
        available = end if painted else 100
        return dict(vte_grid=[8, 20], cell=[10, 20], horizontal=dict(
            native_content_origin=[1,1], clip_width=100, grid_right=81,
            scrollbar_bounds=[end,0,10,100], scrollbar_paints=painted,
            unobscured_right=available, clearance=available-81, overlap=max(0,81-available)))
    def test_clear_grid(self):
        m.horizontal_clearance(self.report())
    def test_real_overlay_intersection_fails(self):
        with self.assertRaises(RuntimeError):
            m.horizontal_clearance(self.report(end=75))
    def test_exact_contact_passes(self):
        m.horizontal_clearance(self.report(end=81))
    def test_subpixel_intersection_fails(self):
        with self.assertRaises(RuntimeError):
            m.horizontal_clearance(self.report(end=80.5))
    def test_negative_subpixel_origin_fails(self):
        r=self.report();h=r['horizontal']
        h.update(native_content_origin=[-0.5,1], grid_right=79.5, clearance=10.5)
        with self.assertRaises(RuntimeError):
            m.horizontal_clearance(r)
    def test_unpainted_bar_does_not_obscure_cells(self):
        m.horizontal_clearance(self.report(end=75, painted=False))
    def test_untrusted_report_cannot_change_cell_extent(self):
        r=self.report();r['horizontal']['grid_right']=80
        with self.assertRaises(RuntimeError):
            m.horizontal_clearance(r)

unittest.main()
