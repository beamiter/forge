#!/usr/bin/env python3
"""Offscreen native Pango/Cairo, not a GTK display or compositor test.
Consumes the very same paint::markup function used by both GTK applications.
"""
import ctypes as C,json,sys,pathlib
frames=json.load(open(sys.argv[1]));out=pathlib.Path(sys.argv[2]);out.mkdir(parents=True,exist_ok=True)
ca=C.CDLL('libcairo.so.2');pc=C.CDLL('libpangocairo-1.0.so.0');pa=C.CDLL('libpango-1.0.so.0');go=C.CDLL('libgobject-2.0.so.0')
def bind(lib,name,ret,*args):
 f=getattr(lib,name);f.restype=ret;f.argtypes=list(args);return f
ptr=C.c_void_p;I=C.c_int;D=C.c_double;S=C.c_char_p
surface=bind(ca,'cairo_image_surface_create',ptr,I,I,I);ctx=bind(ca,'cairo_create',ptr,ptr);rgb=bind(ca,'cairo_set_source_rgb',None,ptr,D,D,D);paint=bind(ca,'cairo_paint',None,ptr);move=bind(ca,'cairo_move_to',None,ptr,D,D);write=bind(ca,'cairo_surface_write_to_png',I,ptr,S);destroy=bind(ca,'cairo_destroy',None,ptr);destroy_surface=bind(ca,'cairo_surface_destroy',None,ptr)
layout=bind(pc,'pango_cairo_create_layout',ptr,ptr);markup=bind(pa,'pango_layout_set_markup',None,ptr,S,I);size=bind(pa,'pango_layout_get_pixel_size',None,ptr,C.POINTER(I),C.POINTER(I));show=bind(pc,'pango_cairo_show_layout',None,ptr,ptr);unref=bind(go,'g_object_unref',None,ptr)
get_context=bind(pa,'pango_layout_get_context',ptr,ptr);resolution=bind(pc,'pango_cairo_context_set_resolution',None,ptr,D)
DPI=96
def draw(c,text,x,y):
 p=layout(c);resolution(get_context(p),DPI);b=text.encode();markup(p,b,len(b));w,h=I(),I();size(p,C.byref(w),C.byref(h));move(c,x,y);show(c,p);unref(p);return w.value,h.value
results=[]
for DPI in [96,144,192]:
 scale=DPI/96;W,H=int(320*scale),int(350*scale)
 sheet=surface(0,W*8,H*13) if DPI==96 else None
 sc=ctx(sheet) if sheet else None
 if sc:rgb(sc,.035,.055,.065);paint(sc)
 for i,f in enumerate(frames):
  im=surface(0,W,H);c=ctx(im);rgb(c,.035,.055,.065);paint(c)
  title=f'<span foreground="#d7f4ee" font_family="monospace" size="11000">{f["skin"]} / {f["pose"]}</span>'
  meta=f'<span foreground="#91a6ac" font_family="monospace" size="8000">Full · seed 7 · 350 ms · {DPI} DPI</span>'
  draw(c,title,12*scale,8*scale);draw(c,meta,12*scale,31*scale);w,h=draw(c,f['markup'],20*scale,60*scale)
  assert 20*scale+w<=W and 60*scale+h<=H,(f['skin'],f['pose'],DPI,w,h)
  # Admission uses measured text size plus the same two-pixel guard as GTK.
  measured=(w+2,h+2);viewport=(int(1000*scale),int(800*scale));candidate=(viewport[0]-measured[0]-24,viewport[1]-measured[1]-86,*measured)
  input_rect=(0,viewport[1]-70,viewport[0],70);approval_rect=(0,0,120*scale,55*scale)
  def overlaps(a,b):return a[0]<b[0]+b[2] and b[0]<a[0]+a[2] and a[1]<b[1]+b[3] and b[1]<a[1]+a[3]
  assert candidate[0]>=0 and candidate[1]>=0 and not overlaps(candidate,input_rect) and not overlaps(candidate,approval_rect)
  if sc:
   x=(i%8)*W;y=(i//8)*H;draw(sc,title,x+12,y+8);draw(sc,meta,x+12,y+31);draw(sc,f['markup'],x+20,y+60)
  name=f'{f["skin"]}-{f["pose"]}-{DPI}dpi.png'
  if DPI==96 or f['pose']=='idle':assert write(im,str(out/name).encode())==0
  destroy(c);destroy_surface(im)
  results.append({k:f[k] for k in ['skin','pose','seed','elapsed_ms','frame','alt']}|{'dpi':DPI,'width_px':w,'height_px':h,'admitted_rect':candidate,'input_rect':input_rect,'approval_rect':approval_rect,'native_backend':'Pango/Cairo offscreen','file':name if DPI==96 or f['pose']=='idle' else None})
 if sheet:
  assert write(sheet,str(out/'native-contact-sheet.png').encode())==0;destroy(sc);destroy_surface(sheet)
assert len(results)==312
(out/'measurements.json').write_text(json.dumps({'claims':'Native offscreen text shaping/draw only; NOT display/IPC/compositor/IME/GPU acceptance','scales_percent':[100,150,200],'contact_sheet_cells_nonoverlapping':True,'all_measured_admissions_avoid_input_and_approval':True,'frames':results},ensure_ascii=False,indent=2))
print(f'{len(results)} native Pango/Cairo frames at 100/150/200%; measured bounds and input/approval exclusions passed.')
