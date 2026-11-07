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

from observer import ObservationNotReady, inside, observe_atspi, ocr_rows, phrases, state

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
OBSERVATION_ATTEMPTS = 20
OBSERVATION_SECONDS = 15
OBSERVATION_INTERVAL = .2
# One declared segmentation, independent of expected Mission values.
IDENTITY_OCR_SCALE = 3
FROST_THUMB_MASK = '(b>=128/255)*(b-r>=80/255)*(b-g>=50/255)*(g-r>=25/255)'
FROST_RAIL_MASK = '(b>=60/255)*(b-r>=40/255)*(b-g>=25/255)*(g-r>=10/255)'


def require(test, message):
    if not test:
        raise RuntimeError(message)


class WindowNotReady(ObservationNotReady):
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
                    'ember': r'Missions · \d+ need you',
                    'frost': r'Missions(?: · (?:0|[1-9][0-9]*) need review)?'}
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
        return (app == 'frost' and label == 'Back') or (app == 'ember' and label == 'Close window')
    return False


def frost_sidebar_open_targets(rows, window):
    """Recognize the painted sidebar entry, not arbitrary matching OCR text.

    The captured public sidebar has Agent Workspace above All / Running /
    Needs you and Missions. Bind that unique title to the owned image's upper
    left sidebar, then require its observed layout, high-confidence lines and
    compact word boxes. Only the small middle-dot glyph may read as '-' in
    these two badges. This does not normalize AT-SPI, Mission data or other
    controls. All target coordinates come from the actual Missions words.
    """
    grouped = {}
    for row in rows:
        grouped.setdefault(tuple(row[k] for k in ('page_num','block_num','par_num','line_num')), []).append(row)

    def line(words, separator=None):
        if any(not 80 <= w['conf'] <= 100 or w['width'] <= 0 or w['height'] <= 0 for w in words):
            return None
        letters = [w for i,w in enumerate(words) if i != separator]
        height = max(w['height'] for w in letters)
        bottom = max(w['top'] + w['height'] for w in letters)
        # OCR's scale conversion rounds each box independently, so allow one
        # pixel of quantization as well as the proportional baseline bound.
        if any(w['height'] < height*.6 or bottom-w['top']-w['height'] > height/3+1 for w in letters):
            return None
        if any(not 0 < b['left']-a['left']-a['width'] <= height
               for a,b in zip(words, words[1:])):
            return None
        if separator is not None:
            dot = words[separator]
            # A full-width hyphen, baseline period or tall punctuation is not
            # evidence of the tiny, midline dot visible in the native badge.
            if (max(dot['width'], dot['height']) > height*.4
                    or not .5 <= dot['width']/dot['height'] <= 2
                    or not bottom-height*.8 <= dot['top']+dot['height']/2 <= bottom-height*.2):
                return None
        x = min(w['left'] for w in words); y = min(w['top'] for w in words)
        return [x, y, max(w['left']+w['width'] for w in words)-x,
                max(w['top']+w['height'] for w in words)-y]

    heading_words = [words for words in grouped.values()
                     if [w['text'] for w in words[-2:]] == ['Agent', 'Workspace']]
    if len(heading_words) != 1:
        return []
    words = heading_words[0]
    heading = line(words[-2:])
    if heading is None or len(words) not in (2, 3):
        return []
    if len(words) == 3:
        # The captured collapse triangle is OCR'd as a lone yen glyph. It is
        # decorative, not a low-confidence word silently removed from a label.
        icon = words[0]
        if (icon['text'] not in ('¥', '▼', '▾') or not 0 < icon['width'] <= heading[3]
                or not inside([icon['left'], icon['top'], icon['width'], icon['height']],
                              [heading[0]-heading[3]*2, heading[1], heading[3]*2, heading[3]])):
            return []
    # This is only an exclusion bound, never a coordinate input fallback.
    # Relative word layout alone would also recognize a copy in body prose.
    if not inside(heading, [0, 0, window[2]/3, window[3]/4]):
        return []
    all_tabs, running_tabs, needs_tabs, missions = [], [], [], []
    for words in grouped.values():
        label = ' '.join(w['text'] for w in words)
        if label in ('All', 'Running'):
            rect = line(words)
            if rect:
                (all_tabs if label == 'All' else running_tabs).append(rect)
        elif re.fullmatch(r'Needs you [·-] (?:0|[1-9][0-9]*)', label):
            rect = line(words, 2)
            if rect:
                needs_tabs.append(rect)
        elif re.fullmatch(r'Missions [·-] (?:0|[1-9][0-9]*) need review', label):
            rect = line(words, 1)
            if rect:
                missions.append((label, rect, words))
    if any(len(anchors) != 1 for anchors in (all_tabs, running_tabs, needs_tabs)):
        return []
    left, middle, right = all_tabs[0], running_tabs[0], needs_tabs[0]
    height = max(r[3] for r in (left, middle, right))
    if (not 0 <= heading[0]-left[0] <= heading[3]*1.5
            or not heading[3]*8 <= left[1]-heading[1]-heading[3] <= heading[3]*16
            or any(not inside(r, [0, 0, window[2]/3, window[3]]) for r in (left,middle,right))
            or max(r[1] for r in (left, middle, right))-min(r[1] for r in (left, middle, right)) > height/2
            or any(not 0 < b[0]-a[0]-a[2] <= height*2 for a,b in ((left,middle),(middle,right)))):
        return []
    bottom = max(r[1]+r[3] for r in (left, middle, right))
    result = []
    for label, rect, words in missions:
        x,y,w,h = rect
        if (abs(x-left[0]) > height/4 or x+w > right[0]+right[2]
                or not height <= y-bottom <= height*3 or not height*.6 <= h <= height*1.4):
            continue
        # Extra words, including low-confidence words in a separate OCR block,
        # must not turn adjacent prose into a supposed standalone control.
        own = {id(word) for word in words}
        if any(id(row) not in own and row['top'] < y+h and row['top']+row['height'] > y
               and row['left'] < right[0]+right[2] and row['left']+row['width'] > left[0] for row in rows):
            continue
        result.append(dict(label=label.replace(' - ', ' · ', 1), observed_label=label,
                           rect=rect, node=None))
    return result


def frost_lines(rows, complete=False):
    """Keep separate OCR views/lines separate; never repair observed words."""
    lines = {}
    for row in rows:
        key = tuple(row[k] for k in ('page_num', 'block_num', 'par_num', 'line_num'))
        lines.setdefault(key, []).append(row)
    result = []
    for words in lines.values():
        if complete and any(not 80 <= w['conf'] <= 100 for w in words):
            continue
        text = ' '.join(w['text'] for w in words if 80 <= w['conf'] <= 100)
        if text:
            result.append((min(w['top'] for w in words), min(w['left'] for w in words), text))
    return '\n'.join(text for _, _, text in sorted(result))


def frost_panel_scope(rows, window):
    headings = []
    for rect in phrases(rows, 'Missions'):
        # A standalone heading, not the sidebar badge or matching body prose.
        peers = [r for r in rows if r['top'] < rect[1]+rect[3]
                 and r['top']+r['height'] > rect[1] and r['left'] >= window[2]/2]
        if len(peers) == 1 and peers[0]['text'] == 'Missions':
            headings.append(rect)
    descriptions = phrases(rows, 'Acceptance goals, exact versions and delivery receipts')
    if len(headings) != 1 or len(descriptions) != 1:
        return None
    a, b = headings[0], descriptions[0]
    if (a[0] < window[2]/2 or abs(a[0]-b[0]) > a[3]
            or not 0 < b[1]-a[1]-a[3] < a[3]*3):
        return None
    left = min(a[0], b[0]); top = max(0, a[1]-a[3])
    return [left, top, window[2]-left, window[3]-top]


def frost_components(raw, window):
    """Measured bright components from the actual threshold image, not targets."""
    result = []
    require(raw.startswith('Objects (id: bounding-box centroid area mean-color):'),
            'Invalid Frost image components')
    for line in raw.splitlines()[1:]:
        match = re.fullmatch(r'\s*\d+: (\d+)x(\d+)\+(\d+)\+(\d+) '
                             r'[0-9.]+,[0-9.]+ ([0-9]+(?:\.[0-9]+)?(?:e[+-]?[0-9]+)?) gray\((0|255)\)', line)
        require(match is not None, 'Invalid Frost image component')
        w, h, x, y = map(int, match.groups()[:4]); area = float(match[5])
        rect = [x, y, w, h]
        require(inside(rect, [0, 0, *window[2:]]) and 0 < area <= w*h,
                'Frost image component escaped captured window')
        if match[6] == '255':
            result.append(dict(rect=rect, fill=area/(w*h)))
    return result


def frost_identity_crop(rows, window):
    scope = frost_panel_scope(rows, window)
    identities, revisions = phrases(rows, 'Mission ID:'), phrases(rows, 'Plan revision')
    filters, titles = phrases(rows, 'Needs you: unfinished repositories'), phrases(rows, 'Title')
    if scope is None or any(len(anchors) != 1 for anchors in (identities, revisions, filters, titles)):
        return None
    a, b = identities[0], revisions[0]; pad = max(a[3], b[3])
    # Only the selected-plan header, after the catalog filter and before the
    # Title field. Identical prose in Goal/repository content is not identity.
    if (any(not inside(r, scope) for r in (a,b,filters[0],titles[0]))
            or abs(a[0]-scope[0]) > pad or abs(a[0]-b[0]) > pad
            or not filters[0][1]+filters[0][3] < a[1]
            or not 0 < b[1]-a[1]-a[3] <= pad*3
            or not 0 < titles[0][1]-b[1]-b[3] <= pad*3
            or abs(titles[0][0]-a[0]) > pad):
        return None
    x, y = a[0]-pad, a[1]-pad
    rect = [x, y, window[2]-x, b[1]+b[3]+pad-y]
    return rect if inside(rect, [0, 0, *window[2:]]) else None


def frost_toolbar_targets(observation, label, window):
    rows = observation['rows']
    descriptions = phrases(rows, 'Acceptance goals, exact versions and delivery receipts')
    filters = phrases(rows, 'Needs you: unfinished repositories')
    scope = observation.get('frost_panel_scope')
    if (not scope or frost_panel_scope(rows, window) is None
            or len(descriptions) != 1 or len(filters) != 1):
        return []
    top = descriptions[0][1]+descriptions[0][3]; bottom = filters[0][1]
    buttons = sorted((b for b in observation.get('frost_buttons', [])
        if inside(b['rect'], scope) and top <= b['rect'][1]
        and b['rect'][1]+b['rect'][3] <= bottom), key=lambda b:b['rect'][0])
    if [b['text'] for b in buttons] != ['Refresh', 'Back', 'New Mission']:
        return []
    a, b, c = [item['rect'] for item in buttons]
    if (len({(r[1], r[3]) for r in (a,b,c)}) != 1
            or any(not 0 < q[0]-p[0]-p[2] < p[3] for p,q in ((a,b),(b,c)))):
        return []
    return [dict(label=label, rect=r, node=None) for button in buttons if button['text'] == label
            for r in phrases(button['rows'], label)]


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


def observation_notice_text(observation):
    # Alternate segmentation must never erase a warning from the same image.
    # This combined channel is for rejection only, never semantic bindings.
    return observation['visible_text'] + '\n' + frost_lines(observation.get('frost_source_rows', []))


GTK_STATUS_SUMMARY = {
    'anvil': r'(?:0|[1-9][0-9]*) Mission\(s\) need review\. Only inspected, version-bound receipts count as passed\.',
    'forge': r'(?:0|[1-9][0-9]*) saved Missions · (?:0|[1-9][0-9]*) need you',
}


def gtk_status_node(app, observation):
    """Bind the status widget through the frozen production widget structure.

    GTK layout changes fail closed. Names locate source-defined anchors only;
    they never supply status visibility or replace a missing Text observation.
    """
    if app not in GTK_STATUS_SUMMARY:
        return None
    nodes = observation.get('nodes', observation.get('scoped_nodes', []))
    by_path = {n['path']: n for n in nodes}
    if len(by_path) != len(nodes): return None
    scoped = {n['path'] for n in observation.get('scoped_nodes', [])}
    refresh = [n for n in nodes if n['path'] in scoped and n['role']==43 and n['name']==REFRESH[app]]
    if len(refresh)!=1: return None
    root = by_path.get(refresh[0]['parent'])
    if root is None: return None
    paths = root.get('children',[])
    if len(set(paths)) != len(paths) or any(p not in by_path for p in paths): return None
    children = [by_path[p] for p in paths]
    owned = lambda n: n.get('pid') == observation.get('pid') and n.get('owner') == observation.get('owner')
    if not observation.get('pid') or not observation.get('owner') or not owned(root): return None
    if any(not owned(n) or n['parent']!=root['path'] for n in children): return None
    current=root; seen=set(); frame_seen=False
    while current is not None and current['path'] not in seen:
        if not owned(current): return None
        seen.add(current['path'])
        if current['role']==23 and (app!='anvil' or current['name']==FRAME['anvil']):
            if not inside(current.get('rect',[]),observation.get('window_rect',[])): return None
            frame_seen=True
        if current['path']==observation.get('root'): break
        current=by_path.get(current['parent'])
    if not frame_seen or current is None or current['path']!=observation.get('root'): return None
    def anchor(index,text):
        return children[index]['role']==29 and children[index]['name']==text
    if app=='forge':
        if (len(children)<7 or children[2]['path']!=refresh[0]['path']
                or not anchor(0,'Missions · linked repository delivery')
                or not anchor(1,"A Mission links existing tasks and exact repository candidates. Save edits only the plan. Inspection is read only. Start, validation, review and integration stay in each task's existing controls.")
                or children[4]['name']!='New Mission' or children[6]['role']!=29
                or not re.fullmatch(r'Needs you · (?:select a Mission|unsaved plan; evidence actions are unavailable|(?:0|[1-9][0-9]*) current reasons)',children[6]['name'])):
            return None
        status=children[5]
    else:
        if (len(children)<14 or children[1]['path']!=refresh[0]['path']
                or not anchor(0,'One goal, exact repositories and linked task runs. Inspection reads Git and saved evidence; it does not start work or publish code.')
                or not anchor(3,'Needs-you reasons for saved plans · read-only')
                or not anchor(5,'Title, acceptance goal and required environment')
                or not anchor(9,'Repository = exact absolute worktree path, one per line')):
            return None
        # FlowBox may expose an intermediate FlowBoxChild; follow only actual
        # same-owner edges to the unique source-defined Inspect action.
        descendants=[]; pending=[children[11]['path']]; visited=set()
        while pending:
            path=pending.pop()
            if path in visited or path not in by_path: return None
            visited.add(path); n=by_path[path]
            if not owned(n): return None
            descendants.append(n); pending.extend(n.get('children',[]))
        if len([n for n in descendants if n['role']==43 and n['name']==INSPECT['anvil']])!=1:
            return None
        status=children[12]
    return status if status['role']==29 and status['path'] in scoped else None


def status_text_unknown(observation, status):
    return (status is None or status.get('text_geometry_status')=='unknown'
            or bool(status.get('text_geometry',{}).get('unproven_lines'))
            or any(u['path']==status['path'] for u in observation.get('unknown_text',[])))


def unknown_notices(app, observation, plan):
    """Unknown header text is not proof of absence; body clipping is separate."""
    if app in GTK_STATUS_SUMMARY:
        return status_text_unknown(observation,gtk_status_node(app,observation))
    nodes = observation.get('scoped_nodes', [])
    paths = {n['path'] for n in nodes}
    unknown = [u for u in observation.get('unknown_text', []) if u['path'] in paths]
    if not unknown:
        return False
    refresh = [n for n in nodes if n.get('rect') and n['role'] in (43,62)
               and n['name'] == REFRESH[app]]
    catalog = [n for n in nodes if n.get('rect') and n['role'] in (43,62)
               and allowed_control(app,n['name'],plan,'select')]
    if len(refresh) != 1 or len(catalog) != 1:
        return True
    bounds = observation.get('mission_content_rect',observation.get('window_rect'))
    if not bounds:
        return True
    top = refresh[0]['rect'][1]+refresh[0]['rect'][3] if app == 'ember' else bounds[1]
    bottom = catalog[0]['rect'][1]
    if bottom < top:
        return True
    x,_,w,_ = bounds
    return any(u.get('clip') and u['clip'][0] < x+w and u['clip'][0]+u['clip'][2] > x
               and u['clip'][1] < bottom and u['clip'][1]+u['clip'][3] > top for u in unknown)


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
    text = observation_notice_text(observation)
    if inspection_obstacles(text) or unknown_notices(app,observation,plan):
        return False
    if app in GTK_STATUS_SUMMARY:
        status=gtk_status_node(app,observation)
        if status_text_unknown(observation,status) or not status.get('visible'):
            return False
        value=status.get('visible_text','')
        return (value==status.get('text') and re.fullmatch(GTK_STATUS_SUMMARY[app],value) is not None)
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
    notice_rows = rows + observation.get('frost_source_rows', [])
    return bottom <= top and not any(bottom <= r['top'] < top for r in notice_rows)


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
        self.observation_deadline = None
        self.observation_probe = {}
        self.frost_scope = None
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
        if self.observation_deadline is not None:
            require(time.monotonic() < self.observation_deadline, 'Observation readiness deadline exhausted')
        if self.process:
            require(self.process.poll() is None, 'Native application exited unexpectedly')
            require(self.process_identity is not None, 'Owned app identity is not bound')
            require(self.process_identity['executable'] == self.spec['binary'], 'Wrong app executable')
            require(self.process_identity['state_dir'] == self.env['JTERM_WORKSPACE_STATE_DIR'], 'Wrong shared state')
            require(proc_identity(self.process.pid) == self.process_identity, 'Owned app process identity changed')

    def run(self, command, timeout=15):
        self.checkpoint()
        deadline = min(self.deadline, self.observation_deadline or self.deadline)
        timeout = min(timeout, deadline - time.monotonic())
        require(timeout > 0, 'Command deadline exhausted')
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
            # Only xdotool's documented no-match result is a window absence.
            # PID lookup, capture, D-Bus and other genuine tool failures remain
            # hard failures and cannot be retried as readiness.
            if (command[:2] == ['xdotool', 'search'] and result.returncode == 1
                    and not result.stdout.strip() and not result.stderr.strip()):
                raise WindowNotReady('Owned native window is not visible yet')
            raise RuntimeError('Command failed (exit %d): %s: %s' %
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
        require(len(found) <= 1, 'Ambiguous owned native window: %d candidates' % len(found))
        if not found:
            raise WindowNotReady('Missing owned native window: 0 candidates')
        window, title, rect = found[0]
        active = decimal_id(self.run(['xdotool', 'getactivewindow']), 'active window ID')
        self.window_probe['active_window'] = active
        if active != int(window):
            raise WindowNotReady('Owned target window %s is not the active visible window %s' % (window, active))
        self.window, self.window_title, self.window_rect = window, title, rect

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
        self.frost_scope = None
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
                    self.checkpoint()
                    time.sleep(min(.2, max(0, self.deadline - time.monotonic())))
            else:
                raise RuntimeError('No owned app window within startup bound; last failure: ' + attempts[-1]['error'])
        except Exception as error:
            self.startup_failure(cycle, attempts, error)
            raise
        self.coverage['stages']['launch_%d' % cycle] = dict(pid=self.process.pid, identity=self.process_identity)
        # Mapping/activation is not first paint or accessibility registration.
        # The first observe() waits on those actual signals, not a fixed sleep.

    def observe(self, label, control=False):
        deadline = min(self.deadline, time.monotonic() + OBSERVATION_SECONDS)
        attempts = []
        report = self.out / ('readiness-%03d-%s.json' %
            (self.sequence + 1, re.sub('[^a-zA-Z0-9_-]', '-', label)))
        self.observation_deadline = deadline
        try:
            for attempt in range(OBSERVATION_ATTEMPTS):
                self.checkpoint()
                self.observation_probe = {}
                try:
                    return self.observe_once(label, control=control)
                except ObservationNotReady as error:
                    if self.app == 'frost':
                        self.frost_scope = None  # A discarded frame cannot seed later scroll evidence.
                    # Security/process/host errors are never caught here, even
                    # if a transient observation happened immediately before.
                    self.checkpoint()
                    attempts.append(dict(attempt=attempt+1, error=str(error), **self.observation_probe))
                    report.write_text(json.dumps(dict(attempts=attempts,
                        scope='discarded readiness attempts only; not Mission coverage'), indent=2)+'\n')
                    if (attempt + 1 == OBSERVATION_ATTEMPTS
                            or time.monotonic() + OBSERVATION_INTERVAL >= deadline):
                        raise RuntimeError('Observation readiness bound exhausted; last failure: ' + str(error)) from error
                    time.sleep(OBSERVATION_INTERVAL)
        finally:
            self.observation_deadline = None

    def observe_once(self, label, control=False):
        self.bind_window()
        self.sequence += 1
        stem = '%03d-%s' % (self.sequence, re.sub('[^a-zA-Z0-9_-]', '-', label))
        png = self.out / (stem + '.png')
        window, window_title, window_rect = self.window, self.window_title, self.window_rect[:]
        x, y, w, h = self.window_rect
        captured_at_ns = time.time_ns()
        self.observation_probe = dict(evidence=stem, screenshot=png.name, captured_at_ns=captured_at_ns)
        self.run(['import', '-window', 'root', '-crop', '%dx%d+%d+%d' % (w, h, x, y), str(png)])
        require(png.is_file() and png.stat().st_size > 0, 'Observation screenshot was not produced')
        # A mapped, black/flat window can precede first paint. A nonuniform
        # image is only readiness, never proof of a control or Mission field.
        image = self.run(['identify', '-format', '%w %h %k', str(png)]).strip()
        require(re.fullmatch(r'[1-9][0-9]* [1-9][0-9]* [1-9][0-9]*', image) is not None,
                'Invalid observation screenshot metadata')
        width, height, colors = map(int, image.split())
        require((width, height) == (w, h), 'Observation screenshot dimensions differ from owned window')
        if self.app != 'frost':
            observation = observe_atspi(self.process.pid, self.window_rect,
                FRAME['ember'] if self.app=='ember' and self.mission_open else None,
                deadline=self.observation_deadline, frame_id=stem,
                window_binding=dict(window=int(window), pid=self.process.pid,
                                    title=window_title, rect=window_rect[:]),
                text_geometry=self.clip_text_geometry(png, stem, window_rect))
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
            observation = self.frost_readout(png, stem, rows, regions=not label.startswith('scroll-target'))
            self.scroll_rect = self.window_rect
        if self.app != 'frost':
            spatial = sorted(observation['scoped_nodes'], key=lambda n: (n.get('rect',[0,0])[1], n.get('rect',[0,0])[0]))
            observation['visible_text'] = '\n'.join(dict.fromkeys(n.get('visible_text','') for n in spatial if n.get('visible_text')))
        self.bind_window()
        if self.window != window or self.window_title != window_title or self.window_rect != window_rect:
            raise ObservationNotReady('Owned window changed during observation')
        if colors == 1:
            raise ObservationNotReady('Owned native window has not painted a nonuniform frame')
        self.checkpoint()
        observation.update(evidence=stem, screenshot=png.name, window=self.window, window_rect=self.window_rect,
                           captured_at_ns=captured_at_ns, pid=self.process.pid)
        (self.out / (stem + '.json')).write_text(json.dumps(observation, indent=2) + '\n')
        return observation

    def clip_text_geometry(self, png, stem, window_rect):
        """One sparse OCR pass of this frame, never supplied expected text."""
        frame_rows = None
        def read(node_path, clip, *, deadline):
            nonlocal frame_rows
            require(inside(clip, window_rect), 'AT-SPI OCR crop escaped captured window')
            require(time.monotonic() < deadline, 'AT-SPI OCR deadline exhausted')
            path = self.out / (stem + '.atspi-ocr.png')
            if frame_rows is None:
                self.run(['convert', str(png), '+repage', '-colorspace', 'Gray',
                          '-resize', '300%', str(path)])
                raw = self.run(['tesseract', str(path), 'stdout', '--psm', '11', 'tsv'])
                path.with_suffix('.tsv').write_text(raw)
                frame_rows = ocr_rows(raw, scale=3)
                for word in frame_rows:
                    word['left'] += window_rect[0]; word['top'] += window_rect[1]
            x,y,w,h = clip
            # Keep edge-straddling words unchanged so the observer rejects them.
            rows = [dict(r) for r in frame_rows if r['left'] < x+w and r['left']+r['width'] > x
                    and r['top'] < y+h and r['top']+r['height'] > y]
            return dict(frame_id=stem,rows=rows,image=path.name)
        return read

    def frost_readout(self, png, stem, rows, regions=True):
        # All alternate views are deterministic crops of this same owned frame.
        # Clear ImageMagick's virtual X11 page offset before any local crop.
        scope = frost_panel_scope(rows, self.window_rect)
        binding = (self.process.pid, tuple(sorted((self.process_identity or {}).items())),
                   self.window, tuple(self.window_rect))
        if not self.mission_open:
            self.frost_scope = None
        elif scope:
            self.frost_scope = (binding, scope)
        elif (self.frost_scope and self.frost_scope[0] == binding
              and not any(r['text'] == 'Missions' and r['left'] >= self.window_rect[2]/2 for r in rows)
              and not phrases(rows, 'Acceptance goals, exact versions and delivery receipts')):
            scope = self.frost_scope[1]
        else:
            self.frost_scope = None
        observation = dict(rows=rows, frost_source_rows=rows[:], frost_panel_scope=scope,
                           frost_buttons=[], identity_views=[])
        components = self.run(['convert', '-precision', '16', str(png), '+repage', '-colorspace', 'Gray', '-threshold', '22%',
            '-define', 'connected-components:verbose=true', '-connected-components', '8', 'null:'])
        components = frost_components(components, self.window_rect)
        observation['frost_components'] = components
        # Luminance joins anti-aliased white text to the blue thumb. Separate
        # chroma from brightness before connectivity; never lower the fill gate.
        # Save the exact mask as evidence. Coordinates still come only from
        # measured components, within the current owned Mission scope.
        mask = self.out / (stem + '.thumb-mask.png')
        self.run(['convert', str(png), '+repage', '-colorspace', 'sRGB',
                  '-fx', FROST_THUMB_MASK, '-colorspace', 'Gray', str(mask)])
        thumb_raw = self.run(['convert', '-precision', '16', str(mask),
            '-define', 'connected-components:verbose=true', '-connected-components', '8', 'null:'])
        observation['frost_scroll_components'] = frost_components(thumb_raw, self.window_rect)
        observation['frost_scroll_mask'] = mask.name
        # A blue body stroke is not a scrollbar. Measure the real longer rail
        # in the candidate's own observed columns, without guessed track bounds.
        for index,candidate in enumerate(observation['frost_scroll_components']):
            sx,sy,sw,sh = candidate['rect']
            if not (scope and inside(candidate['rect'],scope) and sx > self.window_rect[2]*.9
                    and 4 <= sw <= 16 and sh > sw*3 and candidate['fill'] >= .95):
                continue
            rail_path = self.out / ('%s.rail-%03d.png' % (stem,index))
            self.run(['convert', str(png), '+repage', '-crop', '%dx%d+%d+0' %
                (sw,self.window_rect[3],sx), '+repage', '-colorspace', 'sRGB',
                '-fx', FROST_RAIL_MASK, '-colorspace', 'Gray', str(rail_path)])
            rail_raw = self.run(['convert', '-precision', '16', str(rail_path),
                '-define', 'connected-components:verbose=true', '-connected-components', '8', 'null:'])
            rails = frost_components(rail_raw,[0,0,sw,self.window_rect[3]])
            rails = [dict(rect=[r['rect'][0]+sx,*r['rect'][1:]],fill=r['fill']) for r in rails]
            rails = [r for r in rails if r['fill'] >= .95 and r['rect'][2] == sw
                     and r['rect'][3] > sh and inside(candidate['rect'],r['rect']) and inside(r['rect'],scope)]
            if len(rails) == 1:
                candidate['track'] = rails[0]['rect']
                candidate['track_image'] = rail_path.name
        buttons = [c['rect'] for c in components if regions and scope and inside(c['rect'], scope)
                   and 16 <= c['rect'][3] <= 60 and c['rect'][2] >= c['rect'][3]*1.5
                   and c['fill'] >= .7]
        require(len(buttons) <= 16, 'Frost public button observation bound exceeded')

        def crop_view(rect, suffix, button=False):
            x,y,w,h = rect
            # Remove only the measured filled rectangle's one-pixel rounded
            # outline, not arbitrary OCR characters. Padding supplies white
            # background to the single-line segmenter, never invented text.
            inset, border = (1, 5) if button else (0, 0)
            crop = [x+inset, y+inset, w-2*inset, h-2*inset]
            path = self.out / (stem+'.'+suffix+'.png')
            command = ['convert', str(png), '+repage', '-crop', '%dx%d+%d+%d' %
                       (crop[2],crop[3],crop[0],crop[1]), '+repage', '-colorspace', 'Gray']
            if button:
                command += ['-threshold', '22%']
            scale = 2 if button else IDENTITY_OCR_SCALE
            command += ['-resize', str(scale*100)+'%']
            if border:
                command += ['-bordercolor', 'white', '-border', str(border*2)]
            self.run(command+[str(path)])
            raw = self.run(['tesseract', str(path), 'stdout', '--psm', '7' if button else '6', 'tsv'])
            (self.out / (stem+'.'+suffix+'.tsv')).write_text(raw)
            found = ocr_rows(raw, scale=scale)
            for word in found:
                word['left'] += crop[0]-border; word['top'] += crop[1]-border
                word['block_num'] = suffix+':'+word['block_num']
            # No words straddling a crop edge, including low-confidence ones,
            # can be silently omitted to manufacture a complete control.
            valid = all(inside([r[k] for k in ('left','top','width','height')], crop) for r in found)
            return dict(rect=rect, rows=found, text=frost_lines(found, complete=True) if valid else '',
                        image=path.name, psm=7 if button else 6, scale=scale, unclipped=valid)

        for index, rect in enumerate(sorted(buttons, key=lambda r:(r[1],r[0]))):
            view = crop_view(rect, 'button-%02d' % index, button=True)
            observation['frost_buttons'].append(view)
            source = [r for r in rows if inside([r[k] for k in ('left','top','width','height')], rect)]
            source_text = frost_lines(source, complete=True)
            if source_text in ('Refresh', 'Back', 'Inspect delivery') and view['text']:
                require(source_text == view['text'], 'Conflicting high-confidence Frost controls')
            # Replace only this same frame's measured button rectangle. Keep
            # every crop word (including low confidence) for notice detection.
            if view['unclipped'] and view['rows']:
                rows = [r for r in rows if not inside([r[k] for k in ('left','top','width','height')], rect)]
                rows += view['rows']
        identity = frost_identity_crop(rows, self.window_rect) if regions else None
        if identity:
            view = crop_view(identity, 'identity')
            # A valid but conflicting high-confidence ID in any view is a
            # hard contradiction. A '?' is not normalized into an identifier.
            ids = {r['text'] for r in [*rows, *view['rows']] if 80 <= r['conf'] <= 100
                   and re.fullmatch(r'mission:[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}', r['text'])}
            require(len(ids) <= 1, 'Conflicting high-confidence Mission identities')
            observation['identity_views'].append(view)
        observation['rows'] = rows
        # Sort complete line groups, not individual words whose tops differ.
        observation['visible_text'] = frost_lines(rows)
        return observation

    def targets(self, observation, purpose):
        if self.app != 'frost':
            result = []
            for n in observation['scoped_nodes']:
                if (n['role'] in (43, 62) and n.get('visible') and state(n, 8) and state(n, 24)
                        and allowed_control(self.app, n['name'], self.plan, purpose)):
                    result.append(dict(label=n['name'], rect=n['rect'], node=n['path']))
            if self.app == 'ember' and purpose == 'close_mission':
                close = observation.get('mission_header_close')
                result = [t for t in result if close is not None and t['node'] == close]
            return result
        rows = observation['rows']
        if purpose in ('refresh', 'close_mission'):
            label = REFRESH[self.app] if purpose == 'refresh' else 'Back'
            return [dict(t, rect=[t['rect'][0]+self.window_rect[0], t['rect'][1]+self.window_rect[1], *t['rect'][2:]])
                    for t in frost_toolbar_targets(observation, label, self.window_rect)]
        if purpose == 'inspect':
            labels = [INSPECT[self.app]]
        elif purpose == 'open':
            return [dict(target, rect=[target['rect'][0]+self.window_rect[0],
                                      target['rect'][1]+self.window_rect[1], *target['rect'][2:]])
                    for target in frost_sidebar_open_targets(rows, self.window_rect)]
        elif purpose == 'select':
            return [dict(label=self.plan['title'],
                         rect=[r[0]+self.window_rect[0],r[1]+self.window_rect[1],*r[2:]],node=None)
                    for r in frost_catalog_bounds(rows,self.plan)]
        else:
            labels = []
        result = []
        for label in labels:
            for rect in phrases(rows, label):
                if purpose == 'inspect':
                    env = phrases(rows, 'Required environment'); attention = phrases(rows, 'Needs your attention')
                    if (frost_panel_scope(rows, self.window_rect) is None or len(env) != 1 or len(attention) != 1
                            or not env[0][1]+env[0][3] <= rect[1] < attention[0][1]
                            or not any(b['text'] == label and inside(rect, b['rect'])
                                       for b in observation.get('frost_buttons', []))):
                        continue
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

    def scroll(self, amount, observation=None):
        self.bind_window()
        if observation is not None:
            require(observation.get('pid') == self.process.pid and observation.get('window') == self.window
                    and observation.get('window_rect') == self.window_rect,
                    'Owned window changed before navigation input')
            require(observation.get('mission_content_rect') == self.scroll_rect
                    and inside(self.scroll_rect,self.window_rect), 'Navigation viewport changed before wheel')
        x,y,w,h = getattr(self, 'scroll_rect', self.window_rect)
        # Public wheel over the observed viewport's gutter avoids editing and
        # avoids nested multiline Goal/JSON text inputs consuming the wheel.
        point = [x+w-12, y+h//2]
        evidence = []
        if self.app == 'frost':
            # The old window gutter landed at the scrollbar's exclusive right
            # edge and never scrolled. Wheel only over a currently observed,
            # unique Mission scrollbar thumb, never a guessed content point.
            choices = []
            for label in ('scroll-target', 'scroll-target-settled'):
                obs = self.observe(label, control=True)
                scope = obs.get('frost_panel_scope')
                thumbs = [c for c in obs.get('frost_scroll_components', []) if scope
                    and inside(c['rect'], scope) and c['rect'][0] > self.window_rect[2]*.9
                    and 4 <= c['rect'][2] <= 16 and c['rect'][3] > c['rect'][2]*3 and c['fill'] >= .95
                    and c.get('track') and inside(c['rect'],c['track']) and inside(c['track'],scope)]
                require(len(thumbs) == 1, 'No unique observed Mission scrollbar')
                require(obs.get('pid') == self.process.pid, 'Mission scrollbar belongs to another PID')
                choices.append((thumbs[0]['rect'],thumbs[0]['track'],tuple(scope),
                                obs['window'],tuple(obs['window_rect']),obs['pid']))
                evidence.append(obs['evidence'])
            require(choices[0] == choices[1], 'Mission scrollbar changed before wheel input')
            self.bind_window()
            require((self.window, tuple(self.window_rect), self.process.pid) == choices[0][3:],
                    'Owned window changed before scrollbar input')
            sx,sy,sw,sh = choices[0][0]
            point = [self.window_rect[0]+sx+sw//2, self.window_rect[1]+sy+sh//2]
        require(inside([*point,1,1], self.window_rect), 'Scroll gutter escaped owned window')
        self.inputs.append(dict(kind='wheel', amount=amount, x=point[0],y=point[1], pid=self.process.pid,
                                **({'evidence': evidence} if evidence else {})))
        self.run(['xdotool', 'mousemove', *map(str, point), 'click', '--repeat', str(abs(amount)),
                  '--delay', '40', '5' if amount > 0 else '4'])
        time.sleep(.2)

    def ember_navigation(self, observation, purpose, label=None):
        """Offscreen geometry chooses wheel direction only, never visibility."""
        require(self.app == 'ember' and self.mission_open, 'Navigation needs the owned Ember Mission')
        require(observation.get('pid') == self.process.pid and observation.get('window') == self.window
                and observation.get('window_rect') == self.window_rect, 'Navigation ownership changed')
        nodes = observation['scoped_nodes']; by_path = {n['path']: n for n in nodes}
        frames = [n for n in nodes if n['role'] == 23 and n['name'] == FRAME['ember']]
        require(len(frames) == 1, 'No unique navigation Mission frame')
        frame = frames[0]; viewport = observation.get('mission_content_rect')
        require(viewport and inside(viewport,frame['rect']) and inside(frame['rect'],self.window_rect),
                'Navigation viewport escaped owned Mission')
        def descendant(n):
            seen = set()
            while n is not None and n['path'] not in seen:
                if n['path'] == frame['path']: return True
                seen.add(n['path']); n = by_path.get(n['parent'])
            return False
        def matches(n):
            if purpose == 'freeze':
                return n['name'] == 'Freeze observed candidates'
            return allowed_control('ember',n['name'],self.plan,purpose) and (label is None or n['name'] == label)
        choices = [n for n in nodes if n['role'] in (43,62) and matches(n)]
        require(len(choices) == 1, 'No unique navigation control: '+purpose)
        n = choices[0]
        require(n.get('pid') == self.process.pid == frame.get('pid') and n.get('owner') == frame.get('owner')
                and descendant(n), 'Navigation control escaped owned Mission ancestry')
        require(purpose == 'freeze' or (state(n,8) and state(n,24)), 'Navigation control is disabled')
        rect = n.get('rect',[])
        require(len(rect) == 4 and all(type(v) is int for v in rect) and min(rect[2:]) > 0,
                'Invalid navigation control rectangle')
        if purpose == 'close_mission':
            require(n['parent'] == frame['path'] and n['path'] == observation.get('mission_header_close'),
                    'Close control is not the Mission title-bar button')
            viewport = observation.get('mission_header_rect')
            require(viewport and inside(viewport,frame['rect']), 'No owned Mission header strip')
        x,y,w,h = rect; vx,vy,vw,vh = viewport
        require(vx <= x and x+w <= vx+vw and h <= vh, 'Navigation control cannot fit viewport')
        direction = -1 if y < vy else 1 if y+h > vy+vh else 0
        distance = max(vy-y,y+h-vy-vh,0)
        binding = (self.process.pid,frame['owner'],frame['path'],n['path'],n['name'],
                   self.window,tuple(self.window_rect),tuple(viewport))
        return n,direction,distance,binding

    def locate_ember(self, purpose, label=None):
        previous = None
        for index in range(24):
            obs = self.observe('locate-%s-%02d' % (purpose,index), control=True)
            n,direction,distance,binding = self.ember_navigation(obs,purpose,label)
            if previous is not None:
                old_binding,old_distance,old_y,old_direction = previous
                require(binding == old_binding, 'Navigation binding changed')
                require(distance < old_distance and (n['rect'][1]-old_y)*old_direction < 0,
                        'Navigation made no progress toward control')
            if direction == 0:
                require(n.get('visible'), 'Contained navigation control is not visible')
                return obs,n
            previous = (binding,distance,n['rect'][1],direction)
            self.scroll(direction*4, observation=obs)
        raise RuntimeError('Navigation 24-step budget exhausted: '+purpose)

    def ember_header(self):
        """Prove the header and upward saturation, within one 24-step budget."""
        previous = None
        for index in range(24):
            obs = self.observe('header-%02d' % index, control=True)
            n,direction,distance,binding = self.ember_navigation(obs,'refresh')
            if previous is not None:
                old_binding,old_y,old_distance,old_direction = previous
                require(binding == old_binding, 'Header navigation binding changed')
                delta = n['rect'][1]-old_y
                if delta == 0:
                    require(direction == 0 and n.get('visible') and
                            bound_identity(obs['visible_text'],self.plan),
                            'Header stopped before identity and Refresh were visible')
                    return obs
                require(delta*old_direction < 0 and (old_distance == 0 or distance < old_distance),
                        'Header navigation made no progress')
            if direction == 0:
                require(n.get('visible'), 'Contained header control is not visible')
            wheel = direction if direction else -1
            previous = (binding,n['rect'][1],distance,wheel)
            self.scroll(wheel*4, observation=obs)
        raise RuntimeError('Header navigation 24-step budget exhausted')

    def rewind(self):
        if self.app == 'ember' and self.mission_open:
            return self.ember_header()
        self.scroll(-30)

    def press(self, purpose, attempts=10, label=None):
        if self.app == 'ember' and self.mission_open and purpose not in ('open','menu','select'):
            first,_ = self.locate_ember(purpose,label)
            choices = self.targets(first,purpose)
            if label is not None: choices = [t for t in choices if t['label'] == label]
            second = self.observe(purpose+'-settled',control=True)
            again = self.targets(second,purpose)
            if label is not None: again = [t for t in again if t['label'] == label]
            require(len(choices) == 1 and again == choices and
                    all(first.get(k) == second.get(k) for k in ('pid','window','window_rect')),
                    'No unique settled public control: '+purpose)
            require(not any(s in observation_notice_text(second) for s in INSPECTION_BUSY)
                    and (purpose not in ('inspect','refresh') or not any(unknown_notices(self.app,o,self.plan) for o in (first,second))),
                    'Mission notice is busy or unknown')
            self.click(choices[0],purpose,second['evidence'])
            return
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
                if (again == choices and not any(s in observation_notice_text(second) for s in INSPECTION_BUSY)
                        and (purpose not in ('inspect','refresh') or not any(unknown_notices(self.app,o,self.plan) for o in (first,second)))):
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
            for problem in inspection_obstacles(observation_notice_text(observation)):
                self.coverage['inspection_errors'].append(dict(problem=problem,evidence=observation['evidence']))
            if clean_inspection_status(self.app, observation, self.plan):
                self.coverage['inspection_status'].append(dict(stage=stage,evidence=observation['evidence']))
            for entry in ([] if unknown_notices(self.app,observation,self.plan) else inspection_validation(text, self.plan)):
                self.coverage['inspection_validation'].setdefault(entry['repository'],[]).append(
                    dict(entry,stage=stage,evidence=observation['evidence']))
        identity_texts = ([v['text'] for v in observation.get('identity_views', [])]
                          if self.app == 'frost' else [text])
        if any(bound_identity(value, self.plan) for value in identity_texts):
            self.coverage['identity_bindings'].append(dict(stage=stage,evidence=observation['evidence']))
        for name,basis in bound_core_pins(text,self.plan).items():
            self.coverage['core_pin_bindings'].setdefault(name,[]).append(
                dict(stage=stage,evidence=observation['evidence'],basis=basis))
        for name in bound_candidates(text, self.plan):
            self.coverage['candidate_bindings'].setdefault(name, []).append(dict(stage=stage,evidence=observation['evidence']))
        for key, value in self.fields.items():
            if self.app == 'frost':
                present = bool(phrases(observation['rows'], value))
                if key == 'identity':
                    present = any(bound_identity(v['text'], self.plan)
                                   for v in observation.get('identity_views', []))
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
        if self.app == 'ember': self.ember_header()
        else: self.scroll(-24)
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
        if self.app != 'ember': self.rewind()
        if self.app == 'anvil':
            self.key_close()
            self.mission_open = False
            time.sleep(.3)
            self.bind_window()
        elif self.app in ('ember', 'frost'):
            self.press('close_mission')
            self.mission_open = False
            self.frost_scope = None
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
                self.frost_scope = None
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
            obstacles = inspection_obstacles(observation_notice_text(first))
            fatal = [p for p in obstacles if p in INSPECTION_INVALID and p != 'Inspect current delivery']
            ready = (clean_inspection_status(self.app, first, self.plan) if self.app == 'frost'
                     else len(self.targets(first, 'inspect')) == 1)
            if ready:
                require(not fatal, 'Inspection rejected: ' + '; '.join(fatal))
            if ready and not obstacles and not unknown_notices(self.app,first,self.plan):
                second = self.observe('inspect-completion-%02d-settled' % attempt, control=True)
                ready_again = (clean_inspection_status(self.app, second, self.plan) if self.app == 'frost'
                               else self.targets(second,'inspect') == self.targets(first,'inspect'))
                if ready_again and not inspection_obstacles(observation_notice_text(second)) and not unknown_notices(self.app,second,self.plan):
                    self.coverage['stages']['inspection_ready'] = dict(status='settled',
                        evidence=[first['evidence'],second['evidence']],
                        basis='notice_free_header' if self.app=='frost' else 'enabled_sensitive_inspect_control')
                    return
            time.sleep(.2)
        raise RuntimeError('Inspect completion did not become visibly settled')

    def freeze_after_refresh(self):
        labels = {'anvil':'Freeze inspected commits','forge':'Freeze inspected clean commits as candidates',
                  'ember':'Freeze observed candidates','frost':'Use observed commits'}
        if self.app == 'ember':
            obs,n = self.locate_ember('freeze')
            second = self.observe('freeze-settled',control=True)
            again,_,_,binding = self.ember_navigation(second,'freeze')
            require(n == again and all(obs.get(k) == second.get(k) for k in ('pid','window','window_rect')),
                    'Freeze control changed before readback')
            disabled = not state(n,8) and not state(n,24)
            self.coverage['stages']['freeze_after_refresh'] = dict(
                status='disabled' if disabled else 'unexpected_enabled', evidence=second['evidence'])
            require(disabled, 'Refresh did not visibly disable adoption')
            return
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
                self.rewind()
                self.press('expand', label='beamiter/' + name)
        self.scan('initial')
        # No inspection can be attributed to the exact plan until its visible
        # identity and revision have been established in this app.
        if any(e['stage']=='initial' for e in self.coverage['identity_bindings']):
            self.rewind(); self.press('inspect'); self.wait_inspection_settled(); self.scan('inspected')
        else:
            self.coverage['stages']['inspected'] = 'not_run_identity_not_visible'
        self.rewind(); self.press('refresh'); self.scan('refreshed'); self.freeze_after_refresh()
        if self.close_mission():
            self.open(); self.scan('mission_reopened')
        self.close_app()
        self.coverage['stages']['normal_close_1'] = True
        self.launch(2); self.open()
        if self.app == 'ember':
            for name in REPOS:
                self.rewind()
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


def record_failure(target, error):
    target['error'] = str(error)
    diagnostics = getattr(error,'atspi_diagnostics',None)
    if diagnostics is not None:
        target['atspi_diagnostics'] = diagnostics


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
                record_failure(entry,error)
            finally:
                reader.cleanup()
                entry['coverage'] = reader.coverage
            host.check()
            (out / 'result.json').write_text(json.dumps(result, indent=2)+'\n')
        host.close(); result['host_guard_samples'] = host.samples; host = None
        if all(r['status'] == 'passed' for r in result['applications'].values()):
            result['status'] = 'passed'
    except Exception as error:
        record_failure(result,error)
    finally:
        if host:
            try:
                host.close(); result['host_guard_samples'] = host.samples
            except Exception as error:
                record_failure(result,error); result['status'] = 'failed'
        (out / 'result.json').write_text(json.dumps(result, indent=2)+'\n')
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    raise SystemExit(main())
