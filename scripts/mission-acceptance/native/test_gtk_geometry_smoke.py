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


def fixture():
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
    # Equal minimum/natural character widths and START alignment prevent the
    # label from filling the wide box. This is one logical line, WordChar-
    # wrapped by real GTK layout into several physical painted lines.
    wrapped=Gtk.Label(label=WRAPPED,xalign=0)
    wrapped.set_wrap(True);wrapped.set_wrap_mode(Pango.WrapMode.WORD_CHAR)
    wrapped.set_width_chars(18);wrapped.set_max_width_chars(18)
    wrapped.set_halign(Gtk.Align.START);wrapped.set_hexpand(False);box.append(wrapped)
    loop=GLib.MainLoop();window.connect('close-request',lambda *args:loop.quit())
    window.present();loop.run()


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
                        provider_execution='not_started',python=sys.version.split()[0])
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
                self.process=subprocess.Popen([sys.executable,'-B',str(Path(__file__).resolve()),'--fixture'],
                                              env=env,stdout=subprocess.DEVNULL,stderr=log)
                report['fixture_pid']=self.process.pid
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
                initial=None
                for _ in range(20):
                    try:
                        initial=observer.observe_atspi(self.process.pid,self.geometry(window,deadline),deadline=deadline)
                        break
                    except observer.ObservationNotReady:time.sleep(.1)
                self.assertIsNotNone(initial,'GTK accessibility did not settle')
                (out/'without-fallback.json').write_text(json.dumps(initial,indent=2)+'\n')
                unsupported=[d for d in initial['atspi_diagnostics'] if d['error_name']==observer.NOT_SUPPORTED]
                self.assertTrue(unsupported,'Fixture must exercise actual GTK4.14 NotSupported')
                self.assertTrue(initial['unknown_text'])
                self.assertTrue(all(d['interface']==observer.TEXT and d['method']=='GetRangeExtents' for d in unsupported))
                observed=None
                for attempt in range(10):
                    rect=self.geometry(window,deadline);frame_id='gtk-smoke-%d'%attempt;png=out/(frame_id+'.png')
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
                        observed=observer.observe_atspi(self.process.pid,rect,deadline=deadline,
                            text_geometry=geometry_from_pixels,frame_id=frame_id)
                    except observer.ObservationNotReady:time.sleep(.1);continue
                    if self.geometry(window,deadline)!=rect:observed=None;continue
                    break
                self.assertIsNotNone(observed,'GTK screenshot/tree never settled')
                (out/'with-fallback.json').write_text(json.dumps(observed,indent=2)+'\n')
                for text in VISIBLE:self.assertIn(text,observed['visible_text'])
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
                words=geometry_proofs[wrapped_path]['rows']
                self.assertGreaterEqual(len(words),2,'Wrapped fixture needs actual OCR word geometry')
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
    if sys.argv[1:]==['--fixture']:fixture()
    else:unittest.main()
