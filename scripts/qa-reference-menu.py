#!/usr/bin/env python3
"""Actual Tools popover: closed/open/closed pixels and read-only frame state."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def run(*argv):
    return subprocess.run(argv, check=True, text=True, capture_output=True)


def main():
    p = argparse.ArgumentParser()
    for name in ['binary', 'runtime', 'output']:
        p.add_argument('--' + name, required=True)
    p.add_argument('--organism', action='store_true')
    a = p.parse_args()
    out = Path(a.output).resolve()
    out.mkdir(parents=True, exist_ok=True)
    private = out / 'private'
    private.mkdir(exist_ok=True)
    private.chmod(0o700)
    env = os.environ.copy()
    for key in ['config', 'data', 'state', 'cache', 'runtime']:
        folder = private / key
        folder.mkdir(exist_ok=True)
        folder.chmod(0o700)
        env['XDG_' + key.upper() + ('_DIR' if key == 'runtime' else '_HOME')] = str(folder)
    env.update(GDK_BACKEND='x11', GSK_RENDERER='cairo', GTK_A11Y='none', GTK_USE_PORTAL='0', JTERM_REFERENCE_FIXTURE='0', GDK_SCALE='1', GDK_DPI_SCALE='1', HISTFILE=str(private/'bash_history'))
    rc = private/'reference.bashrc'
    rc.write_text("PS1='> '\nsource " + str(Path(__file__).resolve().parent/'shell-integration/forge.bash') + '\n')
    shell = private/'bash'
    shell.write_text('#!/bin/sh\nexec /bin/bash --noprofile --rcfile ' + str(rc) + ' -i\n')
    shell.chmod(0o700)
    config = private/'config/forge'
    config.mkdir(exist_ok=True)
    config.chmod(0o700)
    (config/'config.toml').write_text(f'shell="{shell}"\njsh_update_check="never"\nfont="DejaVu Sans Mono 11"\nopacity=1.0\nchrome_font_size="10px"\n')
    (config/'config.toml').chmod(0o600)
    geometry = out/'geometry.json'
    trace = out/'after-paint.jsonl'
    env.update(FORGE_SHELL=str(shell), FORGE_REFERENCE_GEOMETRY=str(geometry), FORGE_REFERENCE_STATE_TRACE=str(trace))
    manifest = {'source':run('git','rev-parse','HEAD').stdout.strip(), 'core':run('git','-C','../jterm_core','rev-parse','HEAD').stdout.strip(), 'dirty_diff_sha256':hashlib.sha256(subprocess.check_output(['git','diff','--binary','HEAD'])).hexdigest(), 'binary_sha256':hashlib.sha256(Path(a.binary).read_bytes()).hexdigest(), 'logical_window':[1356,704], 'dpr':1, 'sequences':[]}
    def state():
        if not trace.exists():
            return None
        for line in reversed(trace.read_text().splitlines()):
            try:
                return json.loads(line)
            except json.JSONDecodeError:
                pass
    def wait_state(predicate, description):
        deadline = time.monotonic()+12
        while time.monotonic()<deadline:
            value = state()
            if value and predicate(value):
                return value
            time.sleep(.04)
        raise RuntimeError(description + ': ' + str(state()))
    with (out/'native.log').open('w') as log:
        app = subprocess.Popen([a.runtime,a.binary,'--mode',('block' if a.organism else 'chat'),'--no-restore'],env=env,stdout=log,stderr=log)
        try:
            window=None
            for _ in range(100):
                found=subprocess.run(['xdotool','search','--onlyvisible','--pid',str(app.pid)],text=True,capture_output=True)
                if found.returncode==0 and found.stdout.strip():
                    window=found.stdout.splitlines()[0]
                    break
                time.sleep(.1)
            if not window:
                raise RuntimeError('No native window')
            run('xdotool','windowsize','--sync',window,'1366','714')
            run('xdotool','windowfocus','--sync',window)
            wait_state(lambda s:s['prompt_status']=='Ready','prompt not ready')
            # Geometry export is resize-driven. Refresh only before any
            # history/anchor is established; never resize between menu frames.
            for width in ['1365','1366']:
                run('xdotool','windowsize','--sync',window,width,'714')
                time.sleep(.15)
            deadline=time.monotonic()+5
            while time.monotonic()<deadline:
                allocation=json.loads(geometry.read_text()) if geometry.exists() else {}
                bounds=allocation.get('tools')
                if bounds and bounds['width']>0 and bounds['height']==28 and allocation.get('logical_window')=={'width':1356,'height':704}:break
                time.sleep(.04)
            else:raise RuntimeError('Tools has no current native allocation')
            if a.organism:
                fixture = private/'organism-failure.Makefile'
                proof = private/'observed-failure.txt'
                fixture.write_text("all:\n\t@printf 'Observed local failure fixture\\nActual terminal output remains readable\\n'\n\t@printf observed > " + str(proof) + '\n\t@false\n')
                run('xdotool','mousemove','500','220')
                run('xdotool','click','1')
                run('xdotool','type','--clearmodifiers','--delay','1','make -f ' + str(fixture))
                run('xdotool','key','Return')
                deadline=time.monotonic()+10
                while time.monotonic()<deadline and not proof.exists():time.sleep(.04)
                if not proof.exists():raise RuntimeError('Observed failure command did not run')
                wait_state(lambda s:s['prompt_status']=='Ready','failure command did not finish')
                speech=wait_state(lambda s:s['speech_dismiss'] is not None and s['organism'] is not None,'actual reaction speech not visible')
                manifest['speech_before']=speech
                run('import','-window','root','-crop','1356x704+5+5',str(out/'speech-before.png'))
                dismiss=speech['speech_dismiss']
                run('xdotool','mousemove',str(int(dismiss['x']+dismiss['width']/2)+5),str(int(dismiss['y']+dismiss['height']/2)+5))
                run('xdotool','click','1')
                closed=wait_state(lambda s:s['speech'] is None,'real click did not dismiss speech')
                manifest['speech_after_click']=closed
                time.sleep(1.2)
                manifest['speech_after_heartbeat']=state()
                run('import','-window','root','-crop','1356x704+5+5',str(out/'speech-after-heartbeat.png'))
                manifest['dismissed_across_heartbeat']=state()['speech'] is None
                (out/'manifest.json').write_text(json.dumps(manifest,indent=2))
                if not manifest['dismissed_across_heartbeat']:raise RuntimeError('Speech immediately reappeared')
                print(json.dumps({'manifest':str(out/'manifest.json'),'dismissed_across_heartbeat':True}))
                return
            run('xdotool','mousemove','450','632')
            run('xdotool','click','1')
            run('xdotool','key','ctrl+c')
            time.sleep(.3)
            proof = out/'history-proof.txt'
            run('xdotool','type','--clearmodifiers','--delay','1',"for n in {1..80}; do printf 'History row %s\\n' \"$n\"; done; printf menu-history-ok > " + str(proof))
            run('xdotool','key','Return')
            wait_state(lambda s:s['prompt_status']!='Ready','command did not start') if state()['prompt_status']!='Ready' else None
            deadline=time.monotonic()+10
            while time.monotonic()<deadline and (not proof.exists() or proof.read_text()!='menu-history-ok'):
                time.sleep(.05)
            if not proof.exists() or proof.read_text()!='menu-history-ok':
                run('import','-window','root','-crop','1356x704+5+5',str(out/'history-failed.png'))
                raise RuntimeError('Real history command proof missing')
            time.sleep(.3)
            wait_state(lambda s:s['prompt_status']=='Ready','history command not complete')
            # Multiple actual completed commands create document history;
            # a single capped output only exercises its internal VTE scroller.
            for card in range(1, 9):
                run('xdotool','type','--clearmodifiers','--delay','1',"printf 'History card " + str(card) + "\\nline two\\nline three\\n'; printf " + str(card) + " > " + str(proof))
                run('xdotool','key','Return')
                deadline=time.monotonic()+10
                while time.monotonic()<deadline and proof.read_text()!=str(card):
                    time.sleep(.04)
                if proof.read_text()!=str(card):raise RuntimeError('History card proof missing: '+str(card))
                time.sleep(.2)
                wait_state(lambda s:s['prompt_status']=='Ready','history card prompt incomplete')
            run('xdotool','mousemove','700','350')
            run('xdotool','click','--repeat','12','--delay','20','4')
            before=wait_state(lambda s:s['follow']['reading'] and not s['follow']['animating'] and s['reading_anchor'] is not None,'reading anchor not established')
            tools=json.loads(geometry.read_text())['tools']
            def open_menu():
                run('xdotool','mousemove',str(int(tools['x']+tools['width']/2)+5),str(int(tools['y']+tools['height']/2)+5))
                run('xdotool','click','1')
            def key(value):
                run('xdotool','key',value)
            def seq(name, triggers):
                folder=out/name
                folder.mkdir(exist_ok=True)
                start=time.monotonic()
                item={'name':name,'start_monotonic':start,'triggers':[],'frames':[]}
                for n in range(16):
                    if n in triggers:
                        description,action=triggers[n]
                        item['triggers'].append({'frame':n,'time':time.monotonic(),'action':description})
                        action()
                    path=folder/f'{n:02d}.png'
                    run('import','-window','root','-crop','1356x704+5+5',str(path))
                    item['frames'].append({'frame':n,'time':time.monotonic(),'path':str(path),'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'state':state()})
                    delay=start+(n+1)*.07-time.monotonic()
                    if delay>0:time.sleep(delay)
                manifest['sequences'].append(item)
                (out/'manifest.json').write_text(json.dumps(manifest,indent=2))
            seq('closed-open-escape', {3:('open Tools without selecting a command',open_menu),11:('Escape closes popover',lambda:key('Escape'))})
            seq('closed-open-outside', {3:('open Tools again',open_menu),11:('click empty titlebar outside popover',lambda:(run('xdotool','mousemove','450','25'),run('xdotool','click','1')))})
            seq('closed-open-keynav', {3:('open Tools',open_menu),6:('Down navigates enabled menu row',lambda:key('Down')),8:('Down navigates next enabled menu row',lambda:key('Down')),11:('Escape without activating action',lambda:key('Escape'))})
            after=state()
            manifest['initial_state']=before
            manifest['final_state']=after
            checks={}
            for item in manifest['sequences']:
                states=[f['state'] for f in item['frames'] if f['state']]
                checks[item['name']]={'actually_opened':any(s['tools_open'] for s in states),'actually_closed':not states[-1]['tools_open'],'focus_policy_restored':states[-1]['native_focus_reveal']==before['native_focus_reveal'],'anchor_unchanged':all(s['reading_anchor']==before['reading_anchor'] for s in states),'adjustment_unchanged':all(s['adjustment']==before['adjustment'] for s in states),'reading_intent_retained':all(s['follow']['reading'] for s in states),'output_adjustments_unchanged':all(s['outputs']==before['outputs'] for s in states)}
            keys=manifest['sequences'][-1]['frames']
            manifest['keynav_focus']=[f['state']['focused_widget'] for f in keys]
            checks['closed-open-keynav']['focus_moved_between_rows']=len(set(value for value in manifest['keynav_focus'] if value and value.startswith('reference-tool-')))>1
            manifest['action_probes']=[]
            for title, down_count in [('Terminal',0),('Command palette',1),('Search canvas',2)]:
                open_menu()
                wait_state(lambda s:s['tools_open'],'action popup did not open')
                for _ in range(down_count):key('Down')
                key('Return')
                result=wait_state(lambda s:not s['tools_open'],'selected action did not close menu')
                time.sleep(.1)
                screenshot=out/('action-'+title.lower().replace(' ','-')+'.png')
                run('import','-window','root','-crop','1356x704+5+5',str(screenshot))
                manifest['action_probes'].append({'action':title,'state':state(),'screenshot':str(screenshot)})
                key('Escape')
                time.sleep(.1)
            manifest['checks']=checks
            (out/'manifest.json').write_text(json.dumps(manifest,indent=2))
            print(json.dumps({'manifest':str(out/'manifest.json'),'checks':checks},indent=2))
            if not all(all(v.values()) for v in checks.values()):
                raise RuntimeError('Menu changed reading geometry or intent')
        finally:
            app.terminate()
            try:app.wait(timeout=8)
            except subprocess.TimeoutExpired:
                app.kill();app.wait()

if __name__=='__main__':main()
