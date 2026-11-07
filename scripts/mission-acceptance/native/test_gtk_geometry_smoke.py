#!/usr/bin/env python3
"""Opt-in GTK4.14/AT-SPI/OCR smoke, never a product or provider run.

In an existing private X11/D-Bus session:
 JTERM_GTK_GEOMETRY_SMOKE=1 /usr/bin/python3 -B -m unittest discover \
   -s scripts/mission-acceptance/native -p test_gtk_geometry_smoke.py

Existing dependencies: python3-gi/Gtk4, python3-dbus, xdotool, ImageMagick,
tesseract. No installs, display/bus startup, product/host/provider or input.
JTERM_GTK_GEOMETRY_SMOKE_OUT receives only JSON/PNG. Default discovery skips;
explicit opt-in with missing dependencies fails and leaves a failure receipt.
"""
from contextlib import contextmanager
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest

import observer

VISIBLE=('GEOMETRY FIXTURE VISIBLE','ENTRY GEOMETRY VISIBLE','TEXTVIEW GEOMETRY VISIBLE')
HIDDEN='HIDDEN_SENTINEL_73963'
WRAPPED='WRAPPED GEOMETRY MUST KEEP EACH PHYSICAL LINE SEPARATE WITHOUT JOINING THE LOGICAL SENTENCE'


@contextmanager
def fixture_temporary_directory():
    """Temporary cleanup is secondary to an existing fixture failure too."""
    temporary=tempfile.TemporaryDirectory(prefix='gtk-geometry-smoke-')
    primary=None
    try:
        yield temporary.name
    except BaseException as error:
        primary=error
        raise
    finally:
        try:temporary.cleanup()
        except Exception as error:
            if primary is not None:
                primary.add_note('Fixture temporary cleanup failed: '+str(error))
            else:
                raise


def fixture_environment(source):
    """Pass only this inherited private X11 session's existing authority.

    Never read/copy an authorization cookie, discover a personal .Xauthority,
    disable X access control, or broaden the caller's environment allowlist.
    xvfb-run creates its private authority directory beneath TMPDIR.
    """
    for key in ('XAUTHORITY','TMPDIR'):
        if not isinstance(source.get(key),str) or not source[key]:
            raise RuntimeError('Fixture requires inherited private xvfb-run '+key)
    env={k:v for k,v in source.items() if k in ('PATH','HOME','DISPLAY','DBUS_SESSION_BUS_ADDRESS',
        'XDG_RUNTIME_DIR','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME',
        'XDG_DATA_DIRS','TMPDIR','LANG','LC_ALL','GSK_RENDERER','LIBGL_ALWAYS_SOFTWARE')}
    authority=Path(source['XAUTHORITY']);temporary=Path(source['TMPDIR'])
    if (not authority.is_absolute() or not temporary.is_absolute()
            or authority.resolve(strict=True)!=authority or temporary.resolve(strict=True)!=temporary
            or authority==temporary or not authority.is_relative_to(temporary)):
        raise RuntimeError('Fixture XAUTHORITY is outside the canonical private temporary session')
    info=temporary.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid!=os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Fixture TMPDIR must be an owned private directory')
    info=authority.stat()
    if (not stat.S_ISREG(info.st_mode) or info.st_uid!=os.getuid()
            or info.st_mode & 0o022):
        raise RuntimeError('Fixture XAUTHORITY must be an owned, non-writable-by-others regular file')
    # A private parent protects a cookie even when the file was created
    # with a less restrictive umask. Validate all intermediate ancestors.
    parent=authority.parent
    if parent==temporary or parent.stat().st_mode & 0o077:
        raise RuntimeError('Fixture XAUTHORITY needs its private xvfb-run directory')
    while parent!=temporary:
        info=parent.stat()
        if not stat.S_ISDIR(info.st_mode) or info.st_uid!=os.getuid() or info.st_mode & 0o022:
            raise RuntimeError('Fixture XAUTHORITY has an unsafe temporary ancestor')
        parent=parent.parent
    env['XAUTHORITY']=str(authority)
    env.update(NO_AT_BRIDGE='0',GTK_A11Y='atspi',GDK_BACKEND='x11')
    return env


def fixture_exit_receipt(process):
    """Keep natural exit separate from the signal used for owned cleanup."""
    before=process.poll()
    terminated=before is None
    killed=False
    if terminated:
        process.terminate()
        try:process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            killed=True;process.kill();process.wait(timeout=5)
    return dict(fixture_exit_code_before_cleanup=before,fixture_exit_code=process.poll(),
                fixture_terminated_for_cleanup=terminated,fixture_killed_for_cleanup=killed)


def fixture_stderr_receipt(stream):
    """Read only a bounded tail from this fixture's already-open stderr FD."""
    limit=8192
    stream.flush();stream.seek(0,os.SEEK_END);size=stream.tell()
    stream.seek(max(0,size-limit));raw=stream.read(limit)
    return dict(fixture_stderr_tail=raw.decode('utf-8',errors='replace'),
                fixture_stderr_total_bytes=size,fixture_stderr_truncated=size>limit)


def finalize_fixture_report(report,process,stream,path,active_error=None):
    """Preserve the primary failure even when cleanup or receipt I/O fails."""
    errors=[]
    def attempt(key,operation):
        try:
            operation()
        except Exception as error:
            errors.append(error)
            report.update(status='failed',**{key:str(error)})
    if process is not None:
        attempt('fixture_cleanup_error',lambda:report.update(fixture_exit_receipt(process)))
    if stream is not None:
        attempt('fixture_stderr_error',lambda:report.update(fixture_stderr_receipt(stream)))
        attempt('fixture_stderr_close_error',stream.close)
    attempt('fixture_receipt_write_error',lambda:path.write_text(json.dumps(report,indent=2)+'\n'))
    if errors:
        if active_error is not None:
            # If the receipt itself is unwritable, the original traceback can
            # still retain bounded secondary diagnostics without replacing it.
            active_error.add_note('Fixture finalization errors: '+'; '.join(str(error) for error in errors))
        else:
            raise errors[0]


READY_LIMIT=1024*1024
TRACE_LIMIT=8192


def fixture_allocations(window):
    """Read actual GTK allocations, including internal children, after paint."""
    rows=[];pending=[(window,'0')]
    while pending:
        if len(rows)>=2048:raise RuntimeError('Fixture widget allocation budget exhausted')
        widget,path=pending.pop();rect=widget.get_allocation()
        rows.append(dict(path=path,type=widget.__gtype__.name,
            allocation=[rect.x,rect.y,rect.width,rect.height],
            content_size=[widget.get_width(),widget.get_height()],
            realized=bool(widget.get_realized()),visible=bool(widget.get_visible()),mapped=bool(widget.get_mapped())))
        children=[];child=widget.get_first_child()
        while child is not None:
            children.append((child,path+'/'+str(len(children))));child=child.get_next_sibling()
            if len(children)>2048:raise RuntimeError('Fixture child allocation budget exhausted')
        pending.extend(reversed(children))
    return rows


def validate_fixture_ready(record,pid):
    """The handshake is bounded preparation metadata, never visible evidence."""
    if (not isinstance(record,dict) or set(record)!={'schema','status','pid','frame_ids','allocations'}
            or type(record['schema']) is not int or record['schema']!=1 or record['status']!='ready'
            or type(record['pid']) is not int or record['pid']!=pid):
        raise RuntimeError('Invalid fixture readiness version, status or PID')
    frames=record['frame_ids'];rows=record['allocations']
    if (not isinstance(frames,list) or len(frames)!=3 or any(type(v) is not int or v<0 for v in frames)
            or frames!=list(range(frames[0],frames[0]+3))
            or not isinstance(rows,list) or not 1<=len(rows)<=2048):
        raise RuntimeError('Invalid fixture readiness frames or allocations')
    seen=set()
    for row in rows:
        if (not isinstance(row,dict) or set(row)!={'path','type','allocation','content_size','realized','visible','mapped'}
                or not isinstance(row['path'],str) or not re.fullmatch(r'0(?:/[0-9]+)*',row['path'])
                or row['path'] in seen or len(row['path'])>1024
                or (row['path']!='0' and row['path'].rsplit('/',1)[0] not in seen)
                or not isinstance(row['type'],str) or not 1<=len(row['type'])<=128 or '\x00' in row['type']
                or any(type(row[key]) is not bool for key in ('realized','visible','mapped'))):
            raise RuntimeError('Invalid fixture readiness widget shape')
        for key,length in (('allocation',4),('content_size',2)):
            values=row[key]
            if (not isinstance(values,list) or len(values)!=length
                    or any(type(v) is not int or not -(1<<31)<=v<(1<<31) for v in values)
                    or any(v<0 for v in values[-2:])):
                raise RuntimeError('Invalid fixture readiness allocation geometry')
        seen.add(row['path'])
    root=rows[0]
    if (root['path']!='0' or not all(root[k] for k in ('realized','visible','mapped'))
            or not all(v>0 for v in root['content_size'])):
        raise RuntimeError('Fixture readiness root has not mapped and allocated')
    return record


class FixturePaintReadiness:
    def __init__(self):
        self.last_frame=None;self.signature=None;self.frames=[];self.sample_count=0
    def sample(self,frame,allocations):
        if type(frame) is not int or frame<0 or (self.last_frame is not None and frame<self.last_frame):
            raise RuntimeError('Invalid fixture paint frame counter')
        if frame==self.last_frame:return None
        self.sample_count+=1
        if self.sample_count>240:raise RuntimeError('Fixture allocations did not stabilize within 240 frames')
        signature=json.dumps(allocations,sort_keys=True,separators=(',',':'))
        if self.last_frame is None or frame!=self.last_frame+1 or signature!=self.signature:self.frames=[]
        self.frames.append(frame);self.frames=self.frames[-3:]
        self.signature=signature;self.last_frame=frame
        if len(self.frames)!=3:return None
        return validate_fixture_ready(dict(schema=1,status='ready',pid=os.getpid(),
            frame_ids=self.frames[:],allocations=allocations),os.getpid())


def fixture_ready_location(path):
    path=Path(path);parent=path.parent
    if (not path.is_absolute() or path.name!='fixture-ready.json'
            or parent.resolve(strict=True)!=parent or not parent.name.startswith('gtk-geometry-smoke-')):
        raise RuntimeError('Fixture readiness path is not in its canonical private directory')
    info=parent.stat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid!=os.getuid() or info.st_mode & 0o077:
        raise RuntimeError('Fixture readiness directory is not owned and private')
    return path


def write_fixture_ready(path,record):
    path=fixture_ready_location(path);validate_fixture_ready(record,os.getpid())
    data=(json.dumps(record)+'\n').encode()
    if len(data)>READY_LIMIT:raise RuntimeError('Fixture readiness record is too large')
    if path.exists() or path.is_symlink():raise RuntimeError('Fixture readiness record already exists')
    temporary=path.with_name('.fixture-ready.pending.json')
    fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    with os.fdopen(fd,'wb') as stream:stream.write(data)
    os.replace(temporary,path)


def wait_fixture_ready(path,process,deadline):
    path=fixture_ready_location(path)
    while True:
        if process.poll() is not None:raise RuntimeError('Owned GTK fixture exited before readiness')
        remaining=deadline-time.monotonic()
        if remaining<=0:raise RuntimeError('Fixture paint/allocation readiness deadline exhausted')
        if path.exists() or path.is_symlink():
            fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
            with os.fdopen(fd,'rb') as stream:
                info=os.fstat(stream.fileno())
                if (not stat.S_ISREG(info.st_mode) or info.st_uid!=os.getuid() or info.st_mode & 0o077
                        or not 0<info.st_size<=READY_LIMIT):
                    raise RuntimeError('Unsafe or oversized fixture readiness record')
                raw=stream.read(READY_LIMIT+1)
            if len(raw)>READY_LIMIT:raise RuntimeError('Fixture readiness record grew past its bound')
            return validate_fixture_ready(json.loads(raw),process.pid)
        time.sleep(min(.02,remaining))


@contextmanager
def trace_fixture_extents(dbus_module,trace,phase,frame_id):
    """Transparent recording of real calls, never replacement replies/retries."""
    original=dbus_module.Interface
    def trace_error(error):
        try:
            if len(trace['errors'])<20:trace['errors'].append(type(error).__name__+': '+str(error)[:500])
        except Exception:pass  # A diagnostic cannot replace the observed failure.
    def safe(operation):
        try:operation()
        except Exception as error:trace_error(error)
    def value(item):
        if item is None or isinstance(item,(str,bool)):return item
        if isinstance(item,int):return int(item)
        if isinstance(item,float):return repr(item)
        return dict(type=type(item).__name__)
    def interface(obj,name):
        endpoint=original(obj,name)
        if name!=observer.COMPONENT:return endpoint
        class TracedComponent:
            def __getattr__(self,method):
                actual=getattr(endpoint,method)
                if method!='GetExtents':return actual
                def read(*args,**kwargs):
                    event=dict(phase=phase,frame_id=frame_id,interface=name,method=method)
                    safe(lambda:event.update(owner=str(endpoint.bus_name),path=str(endpoint.object_path),
                        arguments=[dict(value=value(a),type=type(a).__name__) for a in args],
                        timeout=kwargs.get('timeout'),started_ns=time.monotonic_ns()))
                    try:
                        result=actual(*args,**kwargs)
                    except BaseException as error:
                        def failure():
                            event.update(error_type=type(error).__name__,error=str(error)[:1000])
                            getter=getattr(error,'get_dbus_name',None)
                            if callable(getter):event['error_name']=getter()
                        safe(failure)
                        raise
                    else:
                        safe(lambda:event.update(result_type=type(result).__name__,
                            rect=[value(v) for v in result] if isinstance(result,(list,tuple)) else value(result),
                            value_types=[type(v).__name__ for v in result] if isinstance(result,(list,tuple)) else []))
                        return result
                    finally:
                        def finish():
                            event['finished_ns']=time.monotonic_ns()
                            if len(trace['events'])<TRACE_LIMIT:trace['events'].append(event)
                            else:trace['omitted']+=1
                        safe(finish)
                return read
        return TracedComponent()
    dbus_module.Interface=interface
    try:yield
    finally:dbus_module.Interface=original


def fixture_observation(dbus_module,report,phase,diagnostic_frame,*args,**kwargs):
    report['phase']=phase
    with trace_fixture_extents(dbus_module,report['component_trace'],phase,diagnostic_frame):
        return observer.observe_atspi(*args,**kwargs)


def configure_fixture_text(entry,view,wrapped,center):
    """Place fixture ink inside strict OCR clips; never alter observed boxes.

    GtkEntry is still focusable when non-editable, so exclude its GtkText
    descendant from focus before presenting. Alignment moves real ink without
    guessing theme padding. TextView left/right margins are internal padding.
    Label justification centers physical lines, not the widget allocation.
    """
    entry.set_can_focus(False);entry.set_alignment(0.5)
    view.set_can_focus(False);view.set_left_margin(8);view.set_right_margin(8)
    wrapped.set_xalign(0.5);wrapped.set_justify(center)


def fixture(ready_path):
    ready_path=fixture_ready_location(ready_path)
    import gi
    gi.require_version('Gtk','4.0')
    gi.require_version('Pango','1.0')
    from gi.repository import Gtk,GLib,Gdk,Pango
    Gtk.init()
    css=Gtk.CssProvider()
    css.load_from_data(b'* { font-family: DejaVu Sans; font-size: 20px; color: #000; background: #fff; }')
    Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(),css,Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
    window=Gtk.Window(title='Read-only AT-SPI geometry fixture');window.set_default_size(640,650)
    box=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=20)
    for side in ('top','bottom','start','end'):getattr(box,'set_margin_'+side)(24)
    window.set_child(box);box.append(Gtk.Label(label=VISIBLE[0],xalign=0))
    entry=Gtk.Entry();entry.set_text(VISIBLE[1]);entry.set_editable(False);box.append(entry)
    view=Gtk.TextView();view.set_editable(False);view.set_cursor_visible(False)
    view.get_buffer().set_text(VISIBLE[2]);view.set_size_request(-1,50);box.append(view)
    clipped=Gtk.ScrolledWindow();clipped.set_policy(Gtk.PolicyType.NEVER,Gtk.PolicyType.ALWAYS)
    clipped.set_min_content_height(50);clipped.set_max_content_height(50)
    clipped.set_propagate_natural_height(False);clipped.set_vexpand(False)
    clipped.set_child(Gtk.Label(label='CLIPPED FIRST LINE\nSECOND LINE\nTHIRD LINE\nFOURTH LINE\n'+HIDDEN,
                                xalign=0,yalign=0));box.append(clipped)
    # max-width-chars is a natural-size hint, not an allocation limit:
    # GTK 4.14.5 gtklabel.c get_width_for_height recalculates the width for a
    # given height. A homogeneous two-child row allocates a real half width
    # (gtkboxlayout.c gtk_box_layout_allocate), including height-for-width.
    # This remains one logical line with several physical painted lines.
    wrapped_row=Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL,homogeneous=True)
    box.append(wrapped_row)
    wrapped=Gtk.Label(label=WRAPPED,xalign=0)
    wrapped.set_wrap(True);wrapped.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
    wrapped.set_width_chars(18);wrapped.set_max_width_chars(18)
    wrapped.set_halign(Gtk.Align.FILL);wrapped.set_hexpand(False)
    wrapped_row.append(wrapped);wrapped_row.append(Gtk.Box())
    configure_fixture_text(entry,view,wrapped,Gtk.Justification.CENTER)
    loop=GLib.MainLoop();window.connect('close-request',lambda *args:loop.quit())
    window.present()
    clock=window.get_frame_clock()
    if clock is None:raise RuntimeError('Mapped fixture has no frame clock')
    readiness=FixturePaintReadiness();failures=[]
    # after-paint ends the real GTK frame, after layout/paint processing.
    # The tick requests repaint only for this fixture, until preparation ends.
    # https://github.com/GNOME/gtk/blob/4.14.5/gdk/gdkframeclock.c#L225-L240
    def tick(widget,frame_clock):
        widget.queue_draw();frame_clock.request_phase(Gdk.FrameClockPhase.AFTER_PAINT)
        return True
    def after_paint(frame_clock):
        try:
            record=readiness.sample(int(frame_clock.get_frame_counter()),fixture_allocations(window))
            if record is None:return
            write_fixture_ready(ready_path,record)
            window.remove_tick_callback(tick_id);clock.disconnect(paint_id)
        except Exception as error:
            failures.append(error);loop.quit()
    paint_id=clock.connect('after-paint',after_paint)
    tick_id=window.add_tick_callback(tick)
    loop.run()
    if failures:raise failures[0]


@unittest.skipUnless(os.environ.get('JTERM_GTK_GEOMETRY_SMOKE')=='1',
                    'opt-in real GTK4.14 smoke; not exercised by ordinary local discovery')
class GtkGeometrySmoke(unittest.TestCase):
    def command(self,args,deadline):
        if self.process is not None and self.process.poll() is not None:raise RuntimeError('Owned GTK fixture exited')
        remaining=deadline-time.monotonic()
        if remaining<=0:raise RuntimeError('GTK smoke deadline exhausted')
        return subprocess.run(args,check=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,
                              text=True,timeout=min(10,remaining)).stdout

    def geometry(self,window,deadline):
        raw=self.command(['xdotool','getwindowgeometry','--shell',str(window)],deadline)
        values={}
        for line in raw.splitlines():
            match=re.fullmatch(r'(WINDOW|X|Y|WIDTH|HEIGHT|SCREEN)=(-?[0-9]+)',line)
            if match is None or match[1] in values:raise RuntimeError('Malformed fixture geometry')
            values[match[1]]=int(match[2])
        if set(values)!={'WINDOW','X','Y','WIDTH','HEIGHT','SCREEN'} or values['WINDOW']!=window:
            raise RuntimeError('Fixture window identity mismatch')
        rect=[values[k] for k in ('X','Y','WIDTH','HEIGHT')]
        if rect[2]<=0 or rect[3]<=0:raise RuntimeError('Empty fixture window')
        if self.command(['xdotool','getwindowpid',str(window)],deadline).strip()!=str(self.process.pid):
            raise RuntimeError('Foreign fixture window')
        return rect

    def binding(self,window,rect,deadline):
        title=self.command(['xdotool','getwindowname',str(window)],deadline).strip()
        if not title:raise RuntimeError('Fixture native window title is empty')
        return dict(window=window,pid=self.process.pid,title=title,rect=rect[:])

    def assert_interior_words(self,node,proof,frame_id):
        self.assertEqual(proof['frame_id'],frame_id)
        words=proof['rows'];self.assertTrue(words,'Fixture needs actual OCR word geometry')
        clip=node['text_geometry']['clip']
        inner=[clip[0]+1,clip[1]+1,clip[2]-2,clip[3]-2]
        for word in words:
            self.assertGreaterEqual(word['conf'],80,'Fixture proof must be high-confidence')
            self.assertTrue(observer.inside([word[k] for k in ('left','top','width','height')],inner),
                            'Fixture proof must be inside the unchanged one-pixel clip')
        return words

    def test_real_gtk14_capability_and_same_frame_geometry(self):
        self.process=None
        with fixture_temporary_directory() as temporary:
            private=Path(temporary)
            out=Path(os.environ.get('JTERM_GTK_GEOMETRY_SMOKE_OUT',str(private/'evidence')))
            ocr_private=private/'ocr';ocr_private.mkdir(mode=0o700)
            if not out.is_absolute():raise RuntimeError('Smoke evidence path must be absolute')
            out.mkdir(mode=0o700,parents=True,exist_ok=True)
            if out.resolve()!=out or out.stat().st_uid!=os.getuid():raise RuntimeError('Smoke evidence path is not owned/canonical')
            report=dict(schema=1,status='failed',scope='standalone GTK4.14 fixture only',
                        provider_execution='not_started',python=sys.version.split()[0],phase='prerequisites',
                        component_trace=dict(events=[],errors=[],omitted=0))
            deadline=time.monotonic()+60;log=None
            try:
                for tool in ('xdotool','import','convert','tesseract'):
                    if shutil.which(tool) is None:raise RuntimeError('Opt-in GTK smoke missing existing tool: '+tool)
                if not re.fullmatch(r':\d+(?:\.\d+)?',os.environ.get('DISPLAY','')):
                    raise RuntimeError('Opt-in GTK smoke needs existing local X11')
                if not os.environ.get('DBUS_SESSION_BUS_ADDRESS','').startswith('unix:'):
                    raise RuntimeError('Opt-in GTK smoke needs existing private Unix D-Bus')
                import dbus
                import gi
                gi.require_version('Gtk','4.0')
                from gi.repository import Gtk
                from reader import Reader
                report['gtk']=[Gtk.get_major_version(),Gtk.get_minor_version(),Gtk.get_micro_version()]
                report['dbus_python']=dbus.__version__
                if report['gtk'][:2]!=[4,14]:raise RuntimeError('This regression specifically requires GTK4.14.x')
                report['tesseract']=self.command(['tesseract','--version'],deadline).splitlines()[0]
                report['imagemagick']=self.command(['convert','-version'],deadline).splitlines()[0]
                env=fixture_environment(os.environ)
                log=open(private/'fixture.stderr','w+b')
                ready=private/'fixture-ready.json'
                self.assertFalse(ready.exists())
                report['phase']='fixture_launch'
                self.process=subprocess.Popen([sys.executable,'-B',str(Path(__file__).resolve()),'--fixture',str(ready)],
                                              env=env,stdout=subprocess.DEVNULL,stderr=log)
                report['fixture_pid']=self.process.pid
                report['phase']='window_mapping'
                window=None
                for _ in range(80):
                    if self.process.poll() is not None:
                        raise RuntimeError('GTK fixture failed before map (exit %s)'%self.process.returncode)
                    remaining=deadline-time.monotonic()
                    if remaining<=0:raise RuntimeError('GTK smoke mapping deadline exhausted')
                    result=subprocess.run(['xdotool','search','--onlyvisible','--pid',str(self.process.pid)],
                        text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=min(2,remaining))
                    if result.returncode not in (0,1):raise RuntimeError('GTK fixture window search failed')
                    found=result.stdout.split()
                    if found:
                        self.assertEqual(len(found),1);self.assertRegex(found[0],r'^[1-9][0-9]*$')
                        window=int(found[0]);break
                    time.sleep(.1)
                self.assertIsNotNone(window,'Owned fixture window did not map')
                report['phase']='fixture_paint_readiness'
                report['fixture_readiness']=wait_fixture_ready(ready,self.process,min(deadline,time.monotonic()+8))
                report['diagnostic_frames']=[]
                initial=None
                for attempt in range(20):
                    try:
                        report['phase']='initial_capture'
                        rect=self.geometry(window,deadline);binding=self.binding(window,rect,deadline)
                        diagnostic_frame='initial-%d'%attempt;png=out/(diagnostic_frame+'.png')
                        x,y,w,h=rect
                        self.command(['import','-window','root','-crop','%dx%d%+d%+d'%(w,h,x,y),str(png)],deadline)
                        report['diagnostic_frames'].append(dict(frame_id=diagnostic_frame,screenshot=png.name,
                            window_binding=binding,scope='diagnostic only; not OCR or visible evidence'))
                        initial=fixture_observation(dbus,report,'initial_atspi',diagnostic_frame,
                            self.process.pid,rect,deadline=deadline,window_binding=binding)
                        break
                    except observer.ObservationNotReady:time.sleep(.1)
                self.assertIsNotNone(initial,'GTK accessibility did not settle')
                report['phase']='initial_assertions'
                (out/'without-fallback.json').write_text(json.dumps(initial,indent=2)+'\n')
                unsupported=[d for d in initial['atspi_diagnostics'] if d['error_name']==observer.NOT_SUPPORTED]
                self.assertTrue(unsupported,'Fixture must exercise actual GTK4.14 NotSupported')
                self.assertTrue(initial['unknown_text'])
                self.assertTrue(all(d['interface']==observer.TEXT and d['method']=='GetRangeExtents' for d in unsupported))
                observed=None
                for attempt in range(10):
                    report['phase']='ocr_capture'
                    rect=self.geometry(window,deadline);frame_id='gtk-smoke-%d'%attempt;png=out/(frame_id+'.png')
                    binding=self.binding(window,rect,deadline)
                    x,y,w,h=rect
                    self.command(['import','-window','root','-crop','%dx%d%+d%+d'%(w,h,x,y),str(png)],deadline)
                    # Exercise the delivered reader callback, not a separate
                    # OCR algorithm. Only its intermediate TSV stays private.
                    adapter=Reader.__new__(Reader);adapter.out=ocr_private
                    adapter.run=lambda args:self.command(args,deadline)
                    actual_geometry=adapter.clip_text_geometry(png,frame_id,rect)
                    counter=[0];geometry_proofs={}
                    def geometry_from_pixels(node_path,clip,*,deadline):
                        counter[0]+=1
                        proof=actual_geometry(node_path,clip,deadline=deadline)
                        geometry_proofs[node_path]=proof
                        (out/('%s-node-%d.json'%(frame_id,counter[0]))).write_text(json.dumps(
                            dict(path=node_path,clip=clip,**proof),indent=2)+'\n')
                        image=ocr_private/(frame_id+'.atspi-ocr.png')
                        if image.exists():shutil.copyfile(image,out/image.name)
                        return proof
                    try:
                        observed=fixture_observation(dbus,report,'ocr_atspi',frame_id,
                            self.process.pid,rect,deadline=deadline,
                            text_geometry=geometry_from_pixels,frame_id=frame_id,window_binding=binding)
                    except observer.ObservationNotReady:time.sleep(.1);continue
                    if (self.geometry(window,deadline)!=rect
                            or self.binding(window,rect,deadline)!=binding):observed=None;continue
                    break
                self.assertIsNotNone(observed,'GTK screenshot/tree never settled')
                report['phase']='ocr_assertions'
                self.assertEqual(report['component_trace']['errors'],[])
                self.assertEqual(report['component_trace']['omitted'],0)
                (out/'with-fallback.json').write_text(json.dumps(observed,indent=2)+'\n')
                # Keep actual rejection reasons and raw rows in the main
                # receipt even when an early positive assertion fails.
                report['text_geometry']=[dict(path=n['path'],text=n['text'],rect=n.get('rect'),
                    focused=observer.state(n,12),status=n.get('text_geometry_status'),
                    clip=n.get('text_geometry',{}).get('clip'),
                    reason=n.get('text_geometry',{}).get('reason'),
                    rows=geometry_proofs.get(n['path'],{}).get('rows',[]))
                    for n in observed['nodes'] if 'text' in n]
                self.assertEqual(observed.get('coordinate_geometry',{}).get('mode'),'window_to_screen')
                for node in observed['nodes']:
                    if 'rect' not in node:continue
                    raw=node['component_extents']
                    self.assertEqual(raw['screen'][:2],[0,0])
                    self.assertEqual(node['rect'],observer.project_window_rect(raw['window'],rect))
                for text in VISIBLE:
                    candidates=[n for n in observed['nodes'] if n.get('text')==text]
                    self.assertEqual(len(candidates),1)
                    node=candidates[0]
                    if text==VISIBLE[1]:
                        self.assertFalse(observer.state(node,12),'Entry fixture must not have keyboard focus')
                    self.assert_interior_words(node,geometry_proofs[node['path']],frame_id)
                    self.assertEqual(node.get('text_geometry_status'),'ocr_verified')
                    self.assertIn(text,observed['visible_text'])
                self.assertNotIn(HIDDEN,observed['visible_text'])
                self.assertTrue(any(n.get('text_geometry',{}).get('status')=='ocr_verified' for n in observed['nodes']))
                self.assertTrue(any(HIDDEN in n.get('text','') for n in observed['nodes']),
                                'Hidden text must exist in the tree for the clipping regression')
                wrapped_nodes=[n for n in observed['nodes'] if n.get('text')==WRAPPED]
                self.assertEqual(len(wrapped_nodes),1,'Wrapped logical text must have one actual Text node')
                wrapped=wrapped_nodes[0];wrapped_path=wrapped['path']
                self.assertNotIn('\n',wrapped['text'])
                self.assertTrue(wrapped['visible'],'Wrapped label must be wholly in the actual viewport')
                self.assertLessEqual(wrapped['rect'][2],rect[2]//2,'Fixture label must remain narrowly allocated')
                self.assertEqual(wrapped.get('text_geometry_status'),'unknown')
                unsupported=wrapped.get('text_geometry',{}).get('unsupported',{})
                self.assertEqual((unsupported.get('path'),unsupported.get('interface'),
                                  unsupported.get('method'),unsupported.get('error_name')),
                                 (wrapped_path,observer.TEXT,'GetRangeExtents',observer.NOT_SUPPORTED))
                self.assertTrue(any(n['path']==wrapped_path for n in observed['unknown_text']))
                self.assertEqual(wrapped['visible_text'],'')
                self.assertNotIn(WRAPPED,observed['visible_text'])
                # This negative must reach non-joining of physical lines, not
                # pass merely because a caret/edge/low-confidence word failed.
                words=self.assert_interior_words(wrapped,geometry_proofs[wrapped_path],frame_id)
                self.assertGreaterEqual(len(words),2,'Wrapped fixture needs actual OCR word geometry')
                # Test completeness only: never feed this joined string to the
                # observer or promote it into visible_text as accepted evidence.
                groups={}
                for word in words:
                    key=tuple(word[k] for k in ('page_num','block_num','par_num','line_num'))
                    groups.setdefault(key,[]).append(word)
                physical_lines=[' '.join(w['text'] for w in sorted(line,key=lambda w:w['left']))
                    for line in sorted(groups.values(),key=lambda line:min(w['top'] for w in line))]
                self.assertGreater(len(physical_lines),1)
                self.assertEqual(' '.join(physical_lines),WRAPPED,'Wrapped OCR must contain the complete fixture text')
                self.assertEqual(wrapped['text_geometry'].get('reason'),
                                 'OCR line is not an exact unique complete text line')
                self.assertGreater(max(r['top'] for r in words)-min(r['top'] for r in words),
                                   max(r['height'] for r in words),
                                   'Actual painted words must span multiple physical lines')
                report['wrapped_logical_line']='confirmed_unknown_without_cross_line_joining'
                report['status']='passed'
            except Exception as error:
                report.update(error_type=type(error).__name__,error=str(error),
                              diagnostics=getattr(error,'atspi_diagnostics',[]))
                raise
            finally:
                finalize_fixture_report(report,self.process,log,out/'result.json',sys.exc_info()[1])


if __name__=='__main__':
    if len(sys.argv)==3 and sys.argv[1]=='--fixture':fixture(Path(sys.argv[2]))
    else:unittest.main()
