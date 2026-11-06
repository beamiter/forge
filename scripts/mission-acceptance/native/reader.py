#!/usr/bin/env python3
"""Read one existing host-owned Mission through four actual native applications.

This is NOT a headless adapter test, fixture lifecycle, provider run, or delivery
claim. The controlled caller supplies installed binaries, safe fresh config,
X11/Openbox/private D-Bus, and an already running independently built host.
Only xdotool pointer/keyboard input operates the applications. AT-SPI and OCR
are read-only observers. Missing, ambiguous, clipped, low-confidence or stale
visible evidence fails closed. No database is opened and no host RPC is sent.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import threading
import time
import tomllib

from observer import inside, observe_atspi, ocr_rows, phrases, state

APPS = ('anvil', 'forge', 'ember', 'frost')
REPOS = ('jterm_core', *APPS)
CORE = '4ffa774138ee89dc2916ce21786c45e411196ca4'
REFRESH = {'anvil': 'Refresh saved Missions', 'forge': 'Refresh saved Missions and expire old evidence',
           'ember': 'Refresh saved Missions', 'frost': 'Refresh'}
INSPECT = {'anvil': 'Inspect saved candidates', 'forge': 'Inspect saved repository candidates',
           'ember': 'Inspect repositories (read-only)', 'frost': 'Inspect delivery'}
REASONS = ('Validation failed', 'Validation blocked', 'Validation needs rechecking',
           'Inspect current delivery', 'Complete the acceptance plan', 'Locate the repository',
           'Select an exact candidate', 'Review candidate changes', 'Correct the core dependency pin',
           'Run the required validation', 'Verify the pushed candidate', 'Verify target integration',
           'Review incomplete evidence')
FRAME = {'anvil': 'Missions · delivery plans', 'ember': 'Missions · delivery evidence'}
ENV_KEYS = {'HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME',
            'XDG_RUNTIME_DIR', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS'}


def require(test, message):
    if not test:
        raise RuntimeError(message)


class WindowNotReady(RuntimeError):
    """A transient window/tool observation, never a process or host guard error."""


def decimal_id(raw, label):
    value = raw.strip()
    require(re.fullmatch(r'[1-9][0-9]*', value) is not None, 'Invalid ' + label + ': ' + repr(raw[:200]))
    return int(value)


def window_geometry(raw, wid):
    # Parse xdotool --shell output as data, never eval it. Also bind the
    # returned geometry to the requested window instead of trusting coordinates.
    fields = {}
    for line in raw.splitlines():
        match = re.fullmatch(r'(WINDOW|X|Y|WIDTH|HEIGHT|SCREEN)=(-?[0-9]+)', line)
        require(match is not None, 'Invalid xdotool geometry line: ' + repr(line[:200]))
        key, value = match.groups()
        require(key not in fields, 'Duplicate xdotool geometry field: ' + key)
        fields[key] = int(value)
    require(set(fields) == {'WINDOW','X','Y','WIDTH','HEIGHT','SCREEN'}, 'Incomplete xdotool geometry')
    require(fields['WINDOW'] == int(wid), 'Geometry belongs to a different window')
    require(fields['WIDTH'] > 0 and fields['HEIGHT'] > 0 and fields['SCREEN'] >= 0, 'Invalid window dimensions/screen')
    return [fields[k] for k in ('X', 'Y', 'WIDTH', 'HEIGHT')]


def sha256(path):
    with open(path, 'rb') as f:
        return hashlib.file_digest(f, 'sha256').hexdigest()


def private_path(value, directory=False):
    p = Path(value)
    require(p.is_absolute() and p.resolve(strict=True) == p, 'Path is not canonical: ' + str(p))
    st = p.stat()
    require(st.st_uid == os.getuid() and not st.st_mode & 0o022, 'Path is not owner-controlled: ' + str(p))
    require(p.is_dir() if directory else p.is_file(), 'Wrong file type: ' + str(p))
    return p


def validate_plan(plan):
    require(isinstance(plan, dict), 'Mission must be an object')
    for key in ('id', 'title', 'goal', 'required_environment'):
        value = plan.get(key)
        require(isinstance(value, str) and 0 < len(value) <= 2048 and '\x00' not in value,
                'Invalid Mission ' + key)
    require('\n' not in plan['title'] and len(plan['title']) <= 110, 'Title must fit the native catalog')
    require(type(plan.get('revision')) is int and plan['revision'] > 0, 'Invalid exact revision')
    repos = plan.get('repositories')
    require(isinstance(repos, list) and len(repos) == 5, 'Expected exactly five repositories')
    names = [r['identity']['full_name'] for r in repos]
    require(set(names) == {'beamiter/' + n for n in REPOS}, 'Repository identities differ')
    for repo in repos:
        for field in ('candidate', 'baseline'):
            require(isinstance(repo.get(field), str) and re.fullmatch('[0-9a-f]{40}', repo[field]),
                    'Full frozen SHA required: ' + field)
        require(isinstance(repo.get('path'), str) and Path(repo['path']).is_absolute(), 'Absolute repository path required')
        require(repo.get('tasks') == [], 'Reader must not receive Task/run references')
        require(isinstance(repo.get('target_ref'), str) and repo['target_ref'], 'Exact target ref required')
        require(isinstance(repo.get('required_checks'), list) and repo['required_checks']
                and all(isinstance(c, str) and c for c in repo['required_checks']), 'Exact checks required')
        if repo['identity']['full_name'] != 'beamiter/jterm_core':
            require(repo.get('required_core_pin') == CORE, 'Frozen common core pin differs')
    return plan


def safe_config(app, path):
    cfg = tomllib.loads(Path(path).read_text())
    exact = dict(ai_enabled=False, eva_companion_enabled=False, agent_workspace_enabled=True,
                 shell='/bin/bash', terminal_mode='block')
    if app in ('anvil', 'forge'):
        # Both published loaders map an absent startup_commands to None.
        # Forge rejects an empty string and then ignores the whole config,
        # including AI/Eva disabling. No startup_commands key is permitted.
        exact.update(jsh_update_check='never')
    else:
        exact.update(restore_session=False, initial_width=1200, initial_height=850)
        if app == 'frost':
            exact['inspector_width'] = 520.0
    require(set(cfg) == set(exact) and all(type(cfg[k]) is type(v) and cfg[k] == v for k,v in exact.items()),
            'Config differs from the audited safe schema for ' + app)
    return sha256(path)


def proc_identity(pid):
    base = Path('/proc') / str(pid)
    # comm can contain spaces and parentheses; fields after the last ')' start at 3.
    rest = (base / 'stat').read_text().rsplit(')', 1)[1].split()
    require(rest[0] not in ('Z', 'X'), 'Owned process is no longer live')
    environ = dict(part.split(b'=', 1) for part in (base / 'environ').read_bytes().split(b'\0') if b'=' in part)
    return dict(pid=pid, start_ticks=rest[19], executable=os.readlink(base / 'exe'),
                state_dir=os.fsdecode(environ.get(b'JTERM_WORKSPACE_STATE_DIR', b'')))


class HostGuard:
    """PID birth, executable and state binding sampled throughout all UI work."""
    def __init__(self, expected, digest):
        self.expected = expected
        self.error = None
        self.samples = 0
        self.stop = threading.Event()
        self.check()
        require(sha256('/proc/%s/exe' % expected['pid']) == digest, 'Running host hash differs')
        self.thread = threading.Thread(target=self.watch, daemon=True)
        self.thread.start()

    def check(self):
        if self.error:
            raise RuntimeError('Continuous host guard failed: ' + self.error)
        actual = proc_identity(self.expected['pid'])
        require(actual == self.expected, 'Independent host PID/birth/executable/state changed')
        self.samples += 1
        return actual

    def watch(self):
        while not self.stop.wait(.1):
            try:
                self.check()
            except Exception as error:
                self.error = str(error)
                return

    def close(self):
        self.stop.set(); self.thread.join(timeout=2)
        self.check()


def safe_environment(manifest, state_dir):
    source = manifest['environment']
    require(set(source) == ENV_KEYS, 'Unexpected or missing environment keys')
    require(re.fullmatch(r':\d+(?:\.\d+)?', source['DISPLAY']) is not None, 'Only local X11 display permitted')
    require(source['DBUS_SESSION_BUS_ADDRESS'].startswith('unix:'), 'Only private Unix D-Bus permitted')
    for key in ENV_KEYS - {'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS'}:
        private_path(source[key], directory=True)
    # Never inherit credentials, provider variables, loader hooks, shell hooks,
    # toolkit configuration, host PATH, proxies or a fallback host-state path.
    env = dict(source, PATH='/usr/bin:/bin', LANG='C.UTF-8', LC_ALL='C.UTF-8',
               SHELL='/bin/bash', XDG_DATA_DIRS='/usr/local/share:/usr/share',
               JTERM_WORKSPACE_STATE_DIR=str(state_dir), NO_AT_BRIDGE='0',
               GTK_A11Y='atspi', GDK_BACKEND='x11', WINIT_UNIX_BACKEND='x11',
               LIBGL_ALWAYS_SOFTWARE='1', WGPU_BACKEND='vulkan')
    return env


def allowed_control(app, label, plan, purpose):
    if purpose == 'open':
        patterns = {'anvil': r'Missions · (delivery plans|\d+ need review)',
                    'forge': r'Mission workbench · linked delivery',
                    'ember': r'Missions · \d+ need you', 'frost': r'Missions(?: · \d+ need review)?'}
        return re.fullmatch(patterns[app], label) is not None
    if purpose == 'menu':
        return app == 'forge' and label == '＋ New'
    if purpose == 'select':
        return label.startswith(plan['title'] + ' · ') or (app == 'frost' and label == plan['title'])
    if purpose in ('refresh', 'inspect'):
        return label == (REFRESH if purpose == 'refresh' else INSPECT)[app]
    if purpose == 'expand':
        return app == 'ember' and label in {'beamiter/' + n for n in REPOS}
    if purpose == 'close_mission':
        return (app == 'frost' and label == 'Back') or (app == 'ember' and label == 'Close')
    return False


def expected_fields(plan):
    fields = {'identity': plan['id'], 'title': plan['title'], 'goal': plan['goal'],
              'required_environment': plan['required_environment']}
    for repo in plan['repositories']:
        prefix = repo['identity']['full_name']
        fields[prefix + '.name'] = prefix
        for k in ('path', 'candidate', 'baseline', 'target_ref'):
            fields[prefix + '.' + k] = repo[k]
        if repo.get('required_core_pin'):
            fields[prefix + '.required_core_pin'] = repo['required_core_pin']
        for check in repo['required_checks']:
            fields[prefix + '.check.' + check] = check
    return fields


def exact_visible(text, value):
    # Whitespace wrapping is acceptable; OCR corrections, prefix hashes and
    # string concatenation across observations are never acceptable.
    pattern = r'(?<![\w/.-])' + r'\s+'.join(re.escape(t) for t in value.split()) + r'(?![\w/.-])'
    return bool(re.search(pattern, text))


def bound_candidates(text, plan):
    """Candidate evidence must share a visible repository section.

    Bare occurrence of a SHA (for example in Baseline or Required core) does not
    prove that repository's Candidate field. Each frame starts without context;
    a repository header clipped off-screen is not filled from an expected plan.
    """
    expected = {r['identity']['full_name']: r['candidate'] for r in plan['repositories']}
    found = set(); current = None; want_candidate = False
    for line in text.splitlines():
        line = line.strip()
        if re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+',line):
            current = line if line in expected else None; want_candidate = False
            continue
        match = re.fullmatch(r'Candidate:?\s+([0-9a-f]{40})', line)
        if current and match and match.group(1) == expected[current]:
            found.add(current)
        if current and want_candidate and line == expected[current]:
            found.add(current)
        want_candidate = line in ('Candidate', 'Candidate:')
    return found


def bound_identity(text, plan):
    """Require the exact ID and its own revision in one actual visible block."""
    mission = re.escape(plan['id']); rev = str(plan['revision'])
    patterns = (
        r'(?<![\w/.-])' + mission + r'\s+·\s+revision\s+' + rev + r'(?![\w/.-])',
        r'"id"\s*:\s*"' + mission + r'"\s*,\s*"revision"\s*:\s*' + rev + r'(?![\w/.-])',
        r'Mission ID:?\s+' + mission + r'\s+Plan revision\s+' + rev + r'(?![\w/.-])',
    )
    return any(re.search(pattern, text) for pattern in patterns)


def bound_core_pins(text, plan):
    """Same-frame repository and explicit Required core or manifest+lock pins."""
    wanted = {r['identity']['full_name']: r['required_core_pin'] for r in plan['repositories']
              if r.get('required_core_pin')}
    current = None; fields = {}; pending = None; found = {}
    labels = {'Required core':'required', 'Required core pin':'required',
              'Cargo manifest pin':'manifest', 'Manifest core':'manifest',
              'Cargo lock pin':'lock', 'Lock core':'lock'}
    for line in map(str.strip,text.splitlines()):
        if re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+',line):
            current = line if line in wanted else None; fields = {}; pending = None
            continue
        if current not in wanted:
            continue
        if pending:
            if line == wanted[current]:
                fields[pending] = line
            pending = None
        for label,key in labels.items():
            match = re.fullmatch(re.escape(label)+r':?\s+([0-9a-f]{40})',line)
            if match and match.group(1) == wanted[current]:
                fields[key] = match.group(1)
            if line in (label,label+':'):
                pending = key
        if 'required' in fields:
            found[current] = 'explicit_required_core'
        elif 'manifest' in fields and 'lock' in fields:
            found[current] = 'observed_manifest_and_lock_core'
    return found


# Core notices and invalidated-report wording, not validation failures. A
# required validation may legitimately be Not run, but the Inspect RPC itself
# must have completed without leaving a busy, error or stale-report surface.
INSPECTION_BUSY = ('Reading the host', 'Waiting for the host', 'Waiting for the session host')
INSPECTION_INVALID = (
    'Mission request:', 'The host returned an unexpected Mission reply',
    'The saved plan changed in another view', 'Saved observation requires reinspection',
    'No inspection is available for this saved plan revision', 'Inspect current delivery',
    'No current inspection.', 'Delivery has not been inspected for this plan revision',
    'Not inspected. Inspect the saved plan', 'Not inspected. Saved plans are not proof',
)


def inspection_obstacles(text):
    return [message for message in (*INSPECTION_BUSY, *INSPECTION_INVALID) if message in text]


def inspection_validation(text, plan):
    """Read actual missing-validation evidence, scoped to a visible repository.

    This parses visible text only. It neither fills a clipped header from the
    plan nor borrows an earlier frame's reason. Candidate/identity bindings are
    independently required in this same inspected stage by missing_coverage.
    """
    if inspection_obstacles(text):
        return []
    expected = {r['identity']['full_name']: r for r in plan['repositories']}
    lines = [line.strip() for line in text.splitlines()]
    found = []
    current = None
    for index, line in enumerate(lines):
        if re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+',line):
            current = line if line in expected else None
        # Ember/Forge and Frost attention buttons include the exact repository
        # in the same visible line. Frost intentionally omits the check name.
        if 'Run the required validation' in line:
            names = [name for name in expected if exact_visible(line, name)]
            if len(names) == 1:
                found.append(dict(repository=names[0], kind='missing_validation_reason', text=line))
            # Anvil displays a multiline card, explicitly binding its title,
            # revision, check, environment and repository in adjoining lines.
            elif index + 1 < len(lines) and exact_visible(line, plan['title']) and re.search(
                    r'\brevision\s+' + str(plan['revision']) + r'\b', line):
                detail = lines[index + 1]
                names = [name for name, repo in expected.items() if exact_visible(detail, name)
                         and exact_visible(detail, plan['required_environment'])
                         and any(exact_visible(detail, check) for check in repo['required_checks'])]
                if len(names) == 1:
                    found.append(dict(repository=names[0], kind='missing_validation_reason',
                                      text=line+'\n'+detail))
        if current:
            for check in expected[current]['required_checks']:
                # Native report strings include exact check and environment.
                # Frost's label and value are adjacent lines in one detail row.
                check_line = exact_visible(line, check) and exact_visible(line, plan['required_environment'])
                not_run = bool(re.search(r'\bNot run\b', line))
                following_not_run = index+1 < len(lines) and lines[index+1] == 'Not run'
                if check_line and (not_run or following_not_run):
                    found.append(dict(repository=current, kind='not_run_check', check=check,
                                      environment=plan['required_environment'],
                                      text=line+('\nNot run' if following_not_run else '')))
    return found


def frost_catalog_bounds(rows, plan):
    """Real catalog band: after Needs-you checkbox, before selected-plan fields."""
    anchors=phrases(rows,'Needs you: unfinished repositories')
    if len(anchors)!=1:
        return []
    bottom=anchors[0][1]+anchors[0][3]
    markers=phrases(rows,'Mission ID:')+phrases(rows,'Mission ID')+phrases(rows,'Plan revision '+str(plan['revision']))
    limit=min((r[1] for r in markers if r[1]>=bottom),default=float('inf'))
    candidates=[r for r in phrases(rows,plan['title']) if bottom<=r[1]<limit]
    return candidates if len(candidates)==1 else []


def clean_inspection_status(app, observation, plan):
    """Require a visible, source-defined notice-free status/header region."""
    text = observation['visible_text']
    if inspection_obstacles(text):
        return False
    if app == 'anvil':
        return bool(re.search(r'\b\d+ Mission\(s\) need review\. Only inspected, version-bound receipts count as passed\.', text))
    if app == 'forge':
        return bool(re.search(r'\b\d+ saved Missions · \d+ need you\b', text))
    if app == 'ember':
        nodes = observation.get('scoped_nodes', [])
        refresh = [n for n in nodes if n.get('visible') and n['role'] in (43,62)
                   and n['name'] == REFRESH[app] and state(n,8) and state(n,24)]
        catalog = [n for n in nodes if n.get('visible') and n['role'] in (43,62)
                   and allowed_control(app,n['name'],plan,'select')]
        if len(refresh) != 1 or len(catalog) != 1:
            return False
        bottom = refresh[0]['rect'][1] + refresh[0]['rect'][3]
        top = catalog[0]['rect'][1]
        if bottom > top:
            return False
        # Optional MissionWorkbench.notice is rendered exactly in this band,
        # between Refresh and the catalog. Unknown notices also reject, rather
        # than relying only on an error-keyword denylist.
        return not any(n.get('visible_text','').strip() and n.get('rect')
                       and bottom <= n['rect'][1] < top for n in nodes)
    rows = observation.get('rows', [])
    anchors = phrases(rows, 'Needs you: unfinished repositories')
    catalog = frost_catalog_bounds(rows,plan)
    if len(anchors) != 1 or len(catalog)!=1:
        return False
    bottom,top=anchors[0][1]+anchors[0][3],catalog[0][1]
    # In the real Frost drawer the optional busy/notice text is between this
    # checkbox and catalog. Do not discard low-confidence words in this band.
    return bottom <= top and not any(bottom <= r['top'] < top for r in rows)


def missing_coverage(app, plan, coverage):
    """Fail-closed final contract, independent from any runner's scan flags."""
    fields = coverage['fields']; candidates = coverage['candidate_bindings']
    stages = coverage['stages']
    required_stages = ['initial','inspected','refreshed','app_reopened']
    if stages.get('mission_close') is True:
        required_stages.append('mission_reopened')
    missing = []
    for stage in required_stages:
        for repository in plan['repositories']:
            name = repository['identity']['full_name']
            if not any(e['stage'] == stage for e in candidates.get(name, [])):
                missing.append(stage+'.'+name+'.candidate_binding')
        if not any(e['stage'] == stage for e in coverage.get('identity_bindings',[])):
            missing.append(stage+'.identity_revision_binding')
        for repository in plan['repositories']:
            if repository.get('required_core_pin'):
                name = repository['identity']['full_name']
                if not any(e['stage'] == stage for e in coverage.get('core_pin_bindings',{}).get(name,[])):
                    missing.append(stage+'.'+name+'.common_core_pin_binding')
    if not coverage['reasons']:
        missing.append('visible_needs_you_reason')
    if app != 'forge' and stages.get('mission_close') is not True:
        missing.append('mission_close_reopen')
    if app != 'frost' and stages.get('freeze_after_refresh',{}).get('status') != 'disabled':
        missing.append('refresh_freeze_disabled')
    if stages.get('inspection_ready',{}).get('status') != 'settled':
        missing.append('inspection_settled')
    if not coverage.get('inspection_status'):
        missing.append('inspection_notice_free_status')
    if coverage.get('inspection_errors'):
        missing.append('inspection_busy_error_or_stale')
    for repository in plan['repositories']:
        name = repository['identity']['full_name']
        if not any(e.get('stage') == 'inspected' for e in coverage.get('inspection_validation',{}).get(name,[])):
            missing.append('inspected.'+name+'.missing_validation_evidence')
    if stages.get('inspected') != 'scan_complete':
        missing.append('inspect_read_only')
    return missing


class Reader:
    def __init__(self, app, spec, env, plan, host, out):
        self.app, self.spec, self.env, self.plan = app, spec, env, plan
        self.host, self.out = host, out
        self.sequence = 0
        self.process = None
        self.process_identity = None
        self.window = None
        self.window_rect = None
        self.window_probe = {}
        self.last_command = None
        self.mission_open = False
        self.deadline = time.monotonic() + 300
        self.inputs = []
        self.coverage = {'fields': {}, 'identity_bindings': [], 'candidate_bindings': {}, 'core_pin_bindings': {},
                         'supplementary_values': {}, 'stages': {}, 'reasons': [],
                         'inspection_validation': {}, 'inspection_status': [], 'inspection_errors': []}
        self.fields = expected_fields(plan)
        self.coverage['supplementary_values'] = {key:dict(status='not_observed',observations=[])
            for key in self.fields if key != 'identity'}
        self.log = None

    def checkpoint(self):
        self.host.check()
        require(time.monotonic() < self.deadline, 'Per-app 300-second budget exhausted')
        if self.process:
            require(self.process.poll() is None, 'Native application exited unexpectedly')
            require(self.process_identity is not None, 'Owned app identity is not bound')
            require(self.process_identity['executable'] == self.spec['binary'], 'Wrong app executable')
            require(self.process_identity['state_dir'] == self.env['JTERM_WORKSPACE_STATE_DIR'], 'Wrong shared state')
            require(proc_identity(self.process.pid) == self.process_identity, 'Owned app process identity changed')

    def run(self, command, timeout=15):
        self.checkpoint()
        self.last_command = dict(argv=command, status='started')
        try:
            result = subprocess.run(command, env=self.env, text=True, capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            self.last_command.update(status='timed_out', timeout=timeout)
            raise RuntimeError('Command timed out after %s seconds: %s' % (timeout, ' '.join(command))) from error
        self.last_command = dict(argv=command, returncode=result.returncode,
                                 stdout=result.stdout[:4096], stderr=result.stderr[:4096])
        self.checkpoint()
        if result.returncode != 0:
            raise WindowNotReady('Command failed (exit %d): %s: %s' %
                                 (result.returncode, ' '.join(command), result.stderr[:400]))
        return result.stdout

    def windows(self):
        self.window_probe = {'pid': self.process.pid, 'candidates': []}
        # xdotool defaults to OR. Without --all, --name .* also selects the
        # unnamed root/Openbox windows; getwindowpid then fails on the root
        # before we ever reach our app. Keep the independent PID read below.
        raw = self.run(['xdotool', 'search', '--all', '--onlyvisible', '--pid', str(self.process.pid), '--name', '.*'])
        self.window_probe['search_stdout'] = raw
        found = []
        seen = set()
        for wid in raw.split():
            xid = decimal_id(wid, 'search window ID')
            require(xid not in seen, 'Duplicate search window ID')
            seen.add(xid)
            candidate = {'window': wid}
            self.window_probe['candidates'].append(candidate)
            candidate['pid'] = decimal_id(self.run(['xdotool', 'getwindowpid', wid]), 'window PID')
            require(candidate['pid'] == self.process.pid, 'Search returned a window with a foreign PID')
            name = self.run(['xdotool', 'getwindowname', wid]).strip()
            candidate['name'] = name
            raw_geom = self.run(['xdotool', 'getwindowgeometry', '--shell', wid])
            candidate['geometry_stdout'] = raw_geom
            rect = window_geometry(raw_geom, wid)
            candidate['rect'] = rect
            if rect[2] >= 300 and rect[3] >= 200:
                found.append((wid, name, rect))
        return found

    def bind_window(self):
        found = self.windows()
        if self.app == 'anvil' and self.mission_open:
            found = [w for w in found if w[1] == FRAME['anvil']]
        elif self.app == 'anvil':
            found = [w for w in found if w[1] != FRAME['anvil']]
        if len(found) != 1:
            raise WindowNotReady('Missing or ambiguous owned native window: %d candidates' % len(found))
        window, _, rect = found[0]
        active = decimal_id(self.run(['xdotool', 'getactivewindow']), 'active window ID')
        self.window_probe['active_window'] = active
        if active != int(window):
            raise WindowNotReady('Owned target window %s is not the active visible window %s' % (window, active))
        self.window, self.window_rect = window, rect

    def startup_failure(self, cycle, attempts, error):
        report = dict(status='failed', error=str(error), attempts=attempts,
                      process_identity=self.process_identity, window_probe=self.window_probe,
                      last_command=self.last_command, launch_log='launch-%d.log' % cycle,
                      scope='startup diagnostics only; not Mission coverage')
        # The private desktop screenshot can explain missing/ambiguous windows.
        # It never supplies UI coverage or bypasses host/process guards.
        png = self.out / ('startup-%d-failed.png' % cycle)
        try:
            self.run(['import', '-window', 'root', str(png)], timeout=5)
            require(png.is_file() and png.stat().st_size > 0, 'Startup screenshot was not produced')
            report['screenshot'] = png.name
        except Exception as capture_error:
            report['screenshot_error'] = str(capture_error)
        (self.out / ('startup-%d.json' % cycle)).write_text(json.dumps(report, indent=2)+'\n')

    def launch(self, cycle):
        self.host.check()
        require(sha256(self.spec['binary']) == self.spec['sha256'], 'App binary hash changed before launch')
        safe_config(self.app, self.spec['config'])
        argv = [self.spec['binary']] + (['--no-restore'] if self.app in ('anvil', 'forge') else [])
        self.log = (self.out / ('launch-%d.log' % cycle)).open('w')
        self.process = subprocess.Popen(argv, env=self.env, cwd=self.env['HOME'], stdout=self.log,
                                        stderr=subprocess.STDOUT, start_new_session=True)
        self.process_identity = None
        self.window, self.window_rect = None, None
        self.window_probe, self.last_command = {}, None
        attempts = []
        try:
            require(self.process.poll() is None, 'App exited during startup')
            # Capture birth/executable/state once. Retrying a window probe must
            # never replace the reference identity after exec or PID reuse.
            self.process_identity = proc_identity(self.process.pid)
            require(self.process_identity['executable'] == self.spec['binary'], 'Wrong app executable')
            require(self.process_identity['state_dir'] == self.env['JTERM_WORKSPACE_STATE_DIR'], 'Wrong shared state')
            for attempt in range(100):
                self.checkpoint()
                try:
                    self.bind_window()
                    break
                except WindowNotReady as error:
                    attempts.append(dict(attempt=attempt+1, error=str(error)))
                    time.sleep(.2)
            else:
                raise RuntimeError('No owned app window within startup bound; last failure: ' + attempts[-1]['error'])
        except Exception as error:
            self.startup_failure(cycle, attempts, error)
            raise
        self.coverage['stages']['launch_%d' % cycle] = dict(pid=self.process.pid, identity=self.process_identity)
        time.sleep(.5)

    def observe(self, label, control=False):
        self.bind_window()
        self.sequence += 1
        stem = '%03d-%s' % (self.sequence, re.sub('[^a-zA-Z0-9_-]', '-', label))
        png = self.out / (stem + '.png')
        x, y, w, h = self.window_rect
        self.run(['import', '-window', 'root', '-crop', '%dx%d+%d+%d' % (w, h, x, y), str(png)])
        if self.app != 'frost':
            observation = observe_atspi(self.process.pid, self.window_rect,
                FRAME['ember'] if self.app=='ember' and self.mission_open else None)
            self.checkpoint()
            if self.app in ('anvil', 'ember') and self.mission_open:
                frames = [n for n in observation['nodes'] if n['role'] == 23 and n['name'] == FRAME[self.app]]
                require(len(frames) == 1 and inside(frames[0]['rect'], self.window_rect), 'No unique owned Mission frame')
                frame = frames[0]
                by_path = {n['path']: n for n in observation['nodes']}
                def scoped(n):
                    while n is not None:
                        if n['path'] == frame['path']:
                            return True
                        n = by_path.get(n['parent'])
                    return False
                observation['scoped_nodes'] = [n for n in observation['nodes'] if scoped(n)]
                observation['visible_text'] = '\n'.join(n.get('visible_text', '') for n in observation['scoped_nodes'])
                self.scroll_rect = observation.get('mission_content_rect',frame['rect'])
            else:
                observation['scoped_nodes'] = observation['nodes']
                self.scroll_rect = self.window_rect
        else:
            transformed = self.out / (stem + '.ocr.png')
            transform = ['-threshold', '22%'] if control else ['-negate']
            self.run(['convert', str(png), '-colorspace', 'Gray', *transform, '-resize', '200%', str(transformed)])
            raw = self.run(['tesseract', str(transformed), 'stdout', '--psm', '11', 'tsv'])
            (self.out / (stem + '.tsv')).write_text(raw)
            rows = ocr_rows(raw)
            lines = {}
            for r in rows:
                if r['conf'] >= 80:
                    lines.setdefault(tuple(r[k] for k in ('page_num','block_num','par_num','line_num')), []).append(r['text'])
            observation = dict(rows=rows, visible_text='\n'.join(' '.join(words) for words in lines.values()))
            self.scroll_rect = self.window_rect
        if self.app != 'frost':
            spatial = sorted(observation['scoped_nodes'], key=lambda n: (n.get('rect',[0,0])[1], n.get('rect',[0,0])[0]))
            observation['visible_text'] = '\n'.join(dict.fromkeys(n.get('visible_text','') for n in spatial if n.get('visible_text')))
        observation.update(evidence=stem, screenshot=png.name, window=self.window, window_rect=self.window_rect,
                           captured_at_ns=time.time_ns(), pid=self.process.pid)
        (self.out / (stem + '.json')).write_text(json.dumps(observation, indent=2) + '\n')
        return observation

    def targets(self, observation, purpose):
        if self.app != 'frost':
            result = []
            for n in observation['scoped_nodes']:
                if (n['role'] in (43, 62) and n.get('visible') and state(n, 8) and state(n, 24)
                        and allowed_control(self.app, n['name'], self.plan, purpose)):
                    result.append(dict(label=n['name'], rect=n['rect'], node=n['path']))
            return result
        rows = observation['rows']
        if purpose in ('refresh', 'inspect'):
            labels = [(REFRESH if purpose == 'refresh' else INSPECT)[self.app]]
        elif purpose == 'open':
            # Exact whole observed line must identify the entry; no expected
            # numeric badge or synthetic position is substituted.
            grouped = {}
            for row in rows:
                grouped.setdefault(tuple(row[k] for k in ('page_num','block_num','par_num','line_num')), []).append(row)
            labels = [' '.join(w['text'] for w in group) for group in grouped.values()]
            labels = [s for s in labels if allowed_control(self.app, s, self.plan, purpose)]
        elif purpose == 'select':
            return [dict(label=self.plan['title'],
                         rect=[r[0]+self.window_rect[0],r[1]+self.window_rect[1],*r[2:]],node=None)
                    for r in frost_catalog_bounds(rows,self.plan)]
        elif purpose == 'close_mission':
            labels = ['Back']
        else:
            labels = []
        result = []
        for label in labels:
            for rect in phrases(rows, label):
                rect = [rect[0]+self.window_rect[0], rect[1]+self.window_rect[1], *rect[2:]]
                result.append(dict(label=label, rect=rect, node=None))
        return result

    def click(self, target, purpose, evidence):
        self.bind_window()
        require(allowed_control(self.app, target['label'], self.plan, purpose), 'Forbidden semantic input')
        require(inside(target['rect'], self.window_rect), 'Target lies outside owned window')
        x,y,w,h = target['rect']
        entry = dict(kind='pointer', purpose=purpose, target=target, evidence=evidence,
                     window=self.window, pid=self.process.pid, x=x+w//2, y=y+h//2)
        self.inputs.append(entry)
        self.run(['xdotool', 'mousemove', str(entry['x']), str(entry['y']), 'click', '1'])
        time.sleep(.3)

    def scroll(self, amount):
        self.bind_window()
        x,y,w,h = getattr(self, 'scroll_rect', self.window_rect)
        # Public wheel over the observed viewport's gutter avoids editing and
        # avoids nested multiline Goal/JSON text inputs consuming the wheel.
        point = [x+w-12, y+h//2]
        require(inside([*point,1,1], self.window_rect), 'Scroll gutter escaped owned window')
        self.inputs.append(dict(kind='wheel', amount=amount, x=point[0],y=point[1], pid=self.process.pid))
        self.run(['xdotool', 'mousemove', *map(str, point), 'click', '--repeat', str(abs(amount)),
                  '--delay', '40', '5' if amount > 0 else '4'])
        time.sleep(.2)

    def press(self, purpose, attempts=10, label=None):
        for _ in range(attempts):
            first = self.observe(purpose, control=True)
            choices = self.targets(first, purpose)
            if label is not None:
                choices = [t for t in choices if t['label'] == label]
            require(len(choices) <= 1, 'Ambiguous visible input target: ' + purpose)
            if choices:
                second = self.observe(purpose+'-settled', control=True)
                again = self.targets(second, purpose)
                if label is not None:
                    again = [t for t in again if t['label'] == label]
                if again == choices and not any(s in second['visible_text'] for s in ('Reading the host', 'Waiting for the host')):
                    self.click(choices[0], purpose, second['evidence'])
                    return
            if purpose in ('select', 'refresh', 'open', 'menu'):
                time.sleep(.2)
            else:
                self.scroll(3)
        raise RuntimeError('No unique settled public control: ' + purpose)

    def open(self):
        if self.app == 'forge':
            self.press('menu')
        self.press('open')
        self.mission_open = True
        self.press('select')
        self.coverage['stages']['selected_existing'] = True

    def absorb(self, observation, stage):
        text = observation['visible_text']
        if stage == 'inspected':
            for problem in inspection_obstacles(text):
                self.coverage['inspection_errors'].append(dict(problem=problem,evidence=observation['evidence']))
            if clean_inspection_status(self.app, observation, self.plan):
                self.coverage['inspection_status'].append(dict(stage=stage,evidence=observation['evidence']))
            for entry in inspection_validation(text, self.plan):
                self.coverage['inspection_validation'].setdefault(entry['repository'],[]).append(
                    dict(entry,stage=stage,evidence=observation['evidence']))
        if bound_identity(text, self.plan):
            self.coverage['identity_bindings'].append(dict(stage=stage,evidence=observation['evidence']))
        for name,basis in bound_core_pins(text,self.plan).items():
            self.coverage['core_pin_bindings'].setdefault(name,[]).append(
                dict(stage=stage,evidence=observation['evidence'],basis=basis))
        for name in bound_candidates(text, self.plan):
            self.coverage['candidate_bindings'].setdefault(name, []).append(dict(stage=stage,evidence=observation['evidence']))
        for key, value in self.fields.items():
            if self.app == 'frost':
                present = bool(phrases(observation['rows'], value))
            else:
                present = exact_visible(text, value)
            if present:
                record = dict(stage=stage,evidence=observation['evidence'])
                if key == 'identity':
                    self.coverage['fields'].setdefault(key,[]).append(record)
                else:
                    self.coverage['supplementary_values'][key]['status'] = 'value_observed_unbound_not_field_proof'
                    self.coverage['supplementary_values'][key]['observations'].append(record)
        revision = self.plan['revision']
        revision_patterns = (r'\brevision\s+'+str(revision)+r'\b', r'"revision"\s*:\s*'+str(revision)+r'\b')
        if any(re.search(p, text, re.I) for p in revision_patterns):
            self.coverage['fields'].setdefault('revision', []).append(dict(stage=stage,evidence=observation['evidence']))
        for line in text.splitlines():
            if any(exact_visible(line, reason) for reason in REASONS):
                self.coverage['reasons'].append(dict(text=line, stage=stage,evidence=observation['evidence']))

    def scan(self, stage):
        self.scroll(-24)
        seen = set()
        for index in range(24):
            obs = self.observe(stage+'-%02d' % index)
            self.absorb(obs, stage)
            fingerprint = hashlib.sha256(obs['visible_text'].encode()).hexdigest()
            if fingerprint in seen and index > 1:
                break
            seen.add(fingerprint)
            self.scroll(4)
        self.coverage['stages'][stage] = 'scan_complete' if stage == 'inspected' else True

    def close_mission(self):
        self.scroll(-30)
        if self.app == 'anvil':
            self.key_close()
            self.mission_open = False
            time.sleep(.3)
            self.bind_window()
        elif self.app in ('ember', 'frost'):
            self.press('close_mission')
            self.mission_open = False
        else:
            # Forge has no Mission close/back control in the frozen source.
            # Do not invent a target or create a terminal to manufacture one.
            self.coverage['stages']['mission_close'] = 'not_proven_no_close_control'
            return False
        self.coverage['stages']['mission_close'] = True
        return True

    def key_close(self):
        self.bind_window()
        self.inputs.append(dict(kind='keyboard', key='alt+F4', purpose='native_close', window=self.window,pid=self.process.pid))
        self.checkpoint()
        command = subprocess.run(['xdotool', 'key', '--clearmodifiers', 'alt+F4'], env=self.env,
                                 text=True, capture_output=True, timeout=5)
        require(command.returncode == 0, 'Native close key failed')
        self.host.check()

    def close_app(self):
        # Closing the separate Anvil Mission window is not closing its app.
        if self.app == 'anvil' and self.mission_open:
            self.close_mission()
        self.key_close()
        for _ in range(50):
            self.host.check()
            if self.process.poll() is not None:
                require(self.process.returncode == 0, 'Native app did not exit normally')
                self.process = None
                self.mission_open = False
                self.log.close()
                return
            time.sleep(.1)
        raise RuntimeError('Normal native close not proven; unexpected confirmation is not bypassed')

    def wait_inspection_settled(self):
        """Observe completion without sending a second Inspect action."""
        if self.app == 'frost':
            self.scroll(-30)
        for attempt in range(20):
            first = self.observe('inspect-completion-%02d' % attempt, control=True)
            obstacles = inspection_obstacles(first['visible_text'])
            fatal = [p for p in obstacles if p in INSPECTION_INVALID and p != 'Inspect current delivery']
            ready = (clean_inspection_status(self.app, first, self.plan) if self.app == 'frost'
                     else len(self.targets(first, 'inspect')) == 1)
            if ready:
                require(not fatal, 'Inspection rejected: ' + '; '.join(fatal))
            if ready and not obstacles:
                second = self.observe('inspect-completion-%02d-settled' % attempt, control=True)
                ready_again = (clean_inspection_status(self.app, second, self.plan) if self.app == 'frost'
                               else self.targets(second,'inspect') == self.targets(first,'inspect'))
                if ready_again and not inspection_obstacles(second['visible_text']):
                    self.coverage['stages']['inspection_ready'] = dict(status='settled',
                        evidence=[first['evidence'],second['evidence']],
                        basis='notice_free_header' if self.app=='frost' else 'enabled_sensitive_inspect_control')
                    return
            time.sleep(.2)
        raise RuntimeError('Inspect completion did not become visibly settled')

    def freeze_after_refresh(self):
        labels = {'anvil':'Freeze inspected commits','forge':'Freeze inspected clean commits as candidates',
                  'ember':'Freeze observed candidates','frost':'Use observed commits'}
        self.scroll(-30)
        for index in range(16):
            obs = self.observe('refresh-freeze-state-%02d' % index, control=True)
            if self.app == 'frost':
                if phrases(obs['rows'], labels[self.app]):
                    self.coverage['stages']['freeze_after_refresh'] = dict(status='visible_enablement_unverified',evidence=obs['evidence'])
                    return
            else:
                choices = [n for n in obs['scoped_nodes'] if n['name']==labels[self.app]
                           and n['role'] in (43,62) and n.get('visible')]
                require(len(choices)<=1, 'Ambiguous freeze control')
                if choices:
                    disabled = not state(choices[0],8) and not state(choices[0],24)
                    self.coverage['stages']['freeze_after_refresh'] = dict(status='disabled' if disabled else 'unexpected_enabled', evidence=obs['evidence'])
                    require(disabled, 'Refresh did not visibly disable adoption')
                    return
            self.scroll(3)
        self.coverage['stages']['freeze_after_refresh'] = dict(status='not_observed')
        require(self.app == 'frost', 'Post-refresh adoption disabled state was not observed')

    def execute(self):
        self.launch(1)
        self.open()
        if self.app == 'ember':
            for name in REPOS:
                self.scroll(-30)
                self.press('expand', label='beamiter/' + name)
        self.scan('initial')
        # No inspection can be attributed to the exact plan until its visible
        # identity and revision have been established in this app.
        if any(e['stage']=='initial' for e in self.coverage['identity_bindings']):
            self.scroll(-30); self.press('inspect'); self.wait_inspection_settled(); self.scan('inspected')
        else:
            self.coverage['stages']['inspected'] = 'not_run_identity_not_visible'
        self.scroll(-30); self.press('refresh'); self.scan('refreshed'); self.freeze_after_refresh()
        if self.close_mission():
            self.open(); self.scan('mission_reopened')
        self.close_app()
        self.coverage['stages']['normal_close_1'] = True
        self.launch(2); self.open()
        if self.app == 'ember':
            for name in REPOS:
                self.scroll(-30)
                self.press('expand', label='beamiter/' + name)
        self.scan('app_reopened'); self.close_app()
        self.coverage['stages']['normal_close_2'] = True
        require(not (missing := missing_coverage(self.app, self.plan, self.coverage)),
                'Visible coverage incomplete: ' + ', '.join(missing))

    def cleanup(self):
        if self.process and self.process.poll() is None:
            # Emergency cleanup never counts as a normal quit or successful UI
            # stage, and never signals the independent host's process group.
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL); self.process.wait(timeout=5)
        if self.log and not self.log.closed:
            self.log.close()
        (self.out / 'inputs.json').write_text(json.dumps(self.inputs, indent=2)+'\n')
        (self.out / 'coverage.json').write_text(json.dumps(self.coverage, indent=2)+'\n')


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', required=True, type=Path)
    parser.add_argument('--receipt', required=True, type=Path)
    parser.add_argument('--plan', required=True, type=Path)
    parser.add_argument('--state-dir', required=True, type=Path)
    parser.add_argument('--host-pid', required=True, type=int)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args(argv)
    out = args.output.resolve(); out.mkdir(mode=0o700, parents=True, exist_ok=False)
    result = dict(schema=1, status='failed', applications={a:dict(status='not_run') for a in APPS},
                  provider_execution='skipped_by_user', full_delivery='not_claimed',
                  scope='actual_native_existing_mission_read_only',
                  mandatory_ui_contract=['exact Mission ID and own revision in the same frame',
                    'five repository-bound candidate SHAs and four common core pins per required stage',
                    'fresh settled inspected-stage repository-bound missing-validation evidence',
                    'Refresh, actual Mission-surface reopening when supported, and normal app reopening'],
                  supplementary_coverage='Unbound value occurrences do not prove field semantics; clipped goal, path, baseline, target and check fields may remain not_observed. Full-plan integrity is checked separately by the host helper.',
                  limitations=['X11 only; software rendering requested, physical GPU and Wayland not certified',
                               'UI observations do not certify publication, integration or validation'])
    host = None
    try:
        manifest = json.loads(args.manifest.read_text()); receipt = json.loads(args.receipt.read_text())
        plan = validate_plan(json.loads(args.plan.read_text()))
        require(receipt['schema'] == 1 and receipt['mission'] == plan and receipt['core'] == CORE,
                'Plan does not equal the frozen production-host receipt')
        require(manifest['schema_version'] == 1 and set(manifest['applications']) == set(APPS), 'Four exact applications required')
        state_dir = private_path(args.state_dir, directory=True)
        expected_host = receipt['host']
        require(expected_host['pid'] == args.host_pid and expected_host['state_dir'] == str(state_dir), 'Host receipt binding differs')
        require(manifest['host']['binary'] == expected_host['executable'], 'Host manifest executable differs')
        env = safe_environment(manifest, state_dir)
        # Observer must connect only to the caller's private accessibility bus.
        os.environ['DBUS_SESSION_BUS_ADDRESS'] = env['DBUS_SESSION_BUS_ADDRESS']
        os.environ['DISPLAY'] = env['DISPLAY']
        configs = set()
        for app in APPS:
            spec = manifest['applications'][app]
            binary = private_path(spec['binary']); config = private_path(spec['config'])
            require(os.access(binary, os.X_OK) and sha256(binary) == spec['sha256'], 'Trusted binary hash differs: '+app)
            require(config == Path(env['XDG_CONFIG_HOME']) / app / 'config.toml', 'Unsupported config discovery path')
            safe_config(app, config); configs.add(config)
        for root in {env[k] for k in ('HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_STATE_HOME','XDG_CACHE_HOME')}:
            require(all(p in configs for p in Path(root).rglob('*') if p.is_file() or p.is_symlink()),
                    'App home/config/state is not fresh and config-only: ' + root)
        host = HostGuard(expected_host, manifest['host']['sha256'])
        result.update(mission_id=plan['id'], mission_revision=plan['revision'], host=expected_host,
                      plan_sha256=sha256(args.plan), manifest_sha256=sha256(args.manifest))
        for app in APPS:
            directory = out / app; directory.mkdir(mode=0o700)
            reader = Reader(app, manifest['applications'][app], env, plan, host, directory)
            entry = result['applications'][app]
            entry.update(binary_sha256=manifest['applications'][app]['sha256'], observer='screenshot-ocr' if app=='frost' else 'read_only_atspi', status='failed',
                         not_covered=(['freeze-disabled enablement; OCR only proves the control text'] if app=='frost' else [])
                         + (['separate Mission-surface close/back; whole-app close/reopen is required instead'] if app=='forge' else []))
            try:
                reader.execute(); entry['status'] = 'passed'
            except Exception as error:
                entry['error'] = str(error)
            finally:
                reader.cleanup()
                entry['coverage'] = reader.coverage
            host.check()
            (out / 'result.json').write_text(json.dumps(result, indent=2)+'\n')
        host.close(); result['host_guard_samples'] = host.samples; host = None
        if all(r['status'] == 'passed' for r in result['applications'].values()):
            result['status'] = 'passed'
    except Exception as error:
        result['error'] = str(error)
    finally:
        if host:
            try:
                host.close(); result['host_guard_samples'] = host.samples
            except Exception as error:
                result['error'] = str(error); result['status'] = 'failed'
        (out / 'result.json').write_text(json.dumps(result, indent=2)+'\n')
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
