#!/usr/bin/env python3
"""F13 stage one. Real Wayland output scaling; synthetic PTY rendering only."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import time

TEST = 'block_view::chat_render_tests::chat_native_unicode_render_geometry'
FONTS = ('DejaVu Sans Mono', 'Liberation Mono')
PHYSICAL = (2400, 1800)


def wait_for(probe, label, process=None, seconds=30):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        value = probe()
        if value:
            return value
        if process is not None and process.poll() is not None:
            raise RuntimeError(f'{label}: process exited {process.returncode}')
        time.sleep(.05)
    raise RuntimeError('Timed out: ' + label)


def measured(outputs, tree, pid, scale, window):
    active = [o for o in outputs if o.get('active') is True]
    if len(active) != 1:
        raise RuntimeError('Expected one active output')
    output = active[0]
    extent = output.get('rect', {})
    if (output.get('scale') != scale or output.get('transform') != 'normal' or
            output.get('current_mode', {}).get('width') != PHYSICAL[0] or
            output.get('current_mode', {}).get('height') != PHYSICAL[1] or
            extent != dict(x=0, y=0, width=int(PHYSICAL[0]/scale), height=int(PHYSICAL[1]/scale))):
        raise RuntimeError('Actual fractional output geometry differs from requested scale')
    matches = []
    def visit(node, floating=False):
        if node.get('name') == 'Forge F13 native render':
            matches.append((node, floating))
        for child in node.get('nodes', []):
            visit(child, floating)
        for child in node.get('floating_nodes', []):
            visit(child, True)
    visit(tree)
    if len(matches) != 1:
        raise RuntimeError('Expected exactly one fixture surface')
    node, floating = matches[0]
    r = node.get('rect', {})
    if (node.get('pid') != pid or node.get('focused') is not True or not floating or
            node.get('border') != 'none' or [r.get('width'), r.get('height')] != window or
            r.get('x', -1) < 0 or r.get('y', -1) < 0 or
            r['x'] + r['width'] > extent['width'] or r['y'] + r['height'] > extent['height']):
        raise RuntimeError('Actual owned window identity, focus or bounds mismatch')
    return dict(output_name=output["name"], output_scale=scale, physical=PHYSICAL, output_rect=extent,
                node_id=node['id'], pid=pid, window_rect=r)



def horizontal_clearance(report):
    """Independent native grid extent must fit before the painted scrollbar."""
    h = report['horizontal']
    x = h['native_content_origin'][0]
    width = report['vte_grid'][0] * report['cell'][0]
    right = x + width
    bar = h['scrollbar_bounds']
    available = min(h['clip_width'], bar[0]) if h['scrollbar_paints'] else h['clip_width']
    if (right != h['grid_right'] or available != h['unobscured_right'] or
            available - right != h['clearance'] or
            max(0, right - available) != h['overlap']):
        raise RuntimeError('Inconsistent native horizontal measurements')
    # Both edges are in the same GTK logical coordinate system. Tolerate
    # floating arithmetic noise only, never a visible logical/physical pixel.
    epsilon = 1e-6
    if x < -epsilon or right > available + epsilon:
        raise RuntimeError(f'Native terminal grid intersects painted scrollbar/clip by {right - available:g} logical pixels')


def stable_gutter(states):
    expected = [(width, shown) for width in (800, 480, 800) for shown in (False, True, False)]
    if len(states) != len(expected):
        raise RuntimeError('Missing native gutter transitions')
    columns = {}
    gutters = set()
    for state, (width, shown) in zip(states, expected):
        if (state['window_width'], state['scrollback']) != (width, shown):
            raise RuntimeError('Unexpected native gutter transition')
        if state['scrollbar_opacity'] != int(shown) or state['scrollbar_targetable'] != shown:
            raise RuntimeError('Hidden scrollbar paint or pointer interception')
        gutter = state['gutter']
        gutters.add(gutter)
        x, allocated_width = state['native_bounds']
        if (gutter <= 0 or allocated_width != state['clip_width'] - gutter or
                x < -1e-6 or x + allocated_width > state['scrollbar_left'] + 1e-6 or
                state['grid_right'] > state['scrollbar_left'] + 1e-6):
            raise RuntimeError('Native allocated gutter does not clear scrollbar')
        if columns.setdefault(width, state['columns']) != state['columns']:
            raise RuntimeError('Scrollbar appearance changed native columns')
    if len(gutters) != 1 or columns[480] >= columns[800]:
        raise RuntimeError('Unstable gutter or ineffective real resize')


def run_case(binary, out, scale, font):
    out.mkdir()
    runtime = out / 'runtime'
    runtime.mkdir(mode=0o700)
    env = os.environ.copy()
    for key in ('DISPLAY', 'WAYLAND_DISPLAY', 'SWAYSOCK', 'GDK_SCALE', 'GDK_DPI_SCALE',
                'GTK_THEME', 'GTK_IM_MODULE'):
        env.pop(key, None)
    for key, folder in [('XDG_CONFIG_HOME', 'config'), ('XDG_DATA_HOME', 'data'),
                        ('XDG_STATE_HOME', 'state'), ('XDG_CACHE_HOME', 'cache')]:
        env[key] = str(out / folder)
        Path(env[key]).mkdir()
    env.update(XDG_RUNTIME_DIR=str(runtime), WLR_BACKENDS='headless', WLR_RENDERER='pixman',
               WLR_HEADLESS_OUTPUTS='1', WLR_LIBINPUT_NO_DEVICES='1', GDK_BACKEND='wayland',
               GSK_RENDERER='cairo', GTK_A11Y='none', LIBGL_ALWAYS_SOFTWARE='1',
               FORGE_F13_OUTPUT=str(out), FORGE_F13_FONT=font, RUST_TEST_THREADS='1')
    config = out / 'sway.conf'
    config.write_text(f'''xwayland disable
output * mode 2400x1800 position 0 0 scale {scale} transform normal
seat seat0 fallback true
focus_follows_mouse no
for_window [title="^Forge F13 native render$"] floating enable, border none
''')
    processes = []
    def run(args):
        return subprocess.run(args, env=env, check=True, capture_output=True, text=True, timeout=10).stdout
    def query(kind):
        return json.loads(run(['swaymsg', '-r', '-t', 'get_' + kind]))
    with (out / 'sway.log').open('w') as sway_log, (out / 'fixture.log').open('w') as fixture_log:
        try:
            sway = subprocess.Popen(['sway', '-d', '-c', str(config)], env=env, stdout=sway_log, stderr=subprocess.STDOUT)
            processes.append(sway)
            sock = wait_for(lambda: next((p for p in runtime.glob('wayland-*') if p.is_socket()), None), 'Wayland socket', sway)
            ipc = wait_for(lambda: next(runtime.glob('sway-ipc.*.sock'), None), 'Sway IPC', sway)
            env.update(WAYLAND_DISPLAY=sock.name, SWAYSOCK=str(ipc))
            fixture = subprocess.Popen([str(binary), TEST, '--ignored', '--exact', '--nocapture'],
                                       env=env, stdout=fixture_log, stderr=subprocess.STDOUT)
            processes.append(fixture)
            reports = []
            for stage in range(18):
                path = out / f'stage-{stage}.json'
                wait_for(path.exists, f'fixture stage {stage}', fixture)
                report = json.loads(path.read_text())
                if report['stage'] != stage or report['font'] != font:
                    raise RuntimeError('Unexpected fixture stage identity')
                before_outputs, before_tree = query('outputs'), query('tree')
                before = measured(before_outputs, before_tree, fixture.pid, scale, report['window'])
                image = out / f'stage-{stage}.png'
                subprocess.run(['grim', '-o', before['output_name'], str(image)], env=env, check=True, timeout=10)
                png = image.read_bytes()
                if png[:8] != b'\x89PNG\r\n\x1a\n' or struct.unpack('>II', png[16:24]) != PHYSICAL:
                    raise RuntimeError('Capture physical dimensions mismatch')
                after_outputs, after_tree = query('outputs'), query('tree')
                after = measured(after_outputs, after_tree, fixture.pid, scale, report['window'])
                if before != after:
                    raise RuntimeError('Geometry changed across native capture')
                for label, value in [('outputs-before', before_outputs), ('tree-before', before_tree),
                                     ('outputs-after', after_outputs), ('tree-after', after_tree)]:
                    (out / f'stage-{stage}-{label}.json').write_text(json.dumps(value, indent=2))
                reports.append(dict(native=report, compositor=before, screenshot_sha256=hashlib.sha256(png).hexdigest()))
                (out / f'stage-{stage}.ack').touch()
            if fixture.wait(timeout=20) != 0:
                raise RuntimeError('Native fixture failed; see fixture.log')
            stable_gutter(json.loads((out / 'gutter-stability.json').read_text()))
            tiny = json.loads((out / 'tiny-gutter.json').read_text())
            if len(tiny) != 4 or [r['clip_width'] for r in tiny[:3]] != [1, 10, 17]:
                raise RuntimeError('Missing actual tiny GTK allocation positive controls')
            for state in tiny:
                if (state['request'] != state['clip_width'] - state['gutter'] or
                        state['request'] <= 0 or state['columns'] <= 0 or
                        state['native_width'] > max(state['request'], state['native_minimum']) + 1e-6):
                    raise RuntimeError('Tiny allocation retained stale native geometry')
            if any(r['gutter'] != 0 or r['columns'] >= tiny[-1]['columns'] for r in tiny[:3]):
                raise RuntimeError('Tiny allocation did not degrade and restore')
            (out / 'result.json').write_text(json.dumps(dict(status='pass', stages=reports,
                scope='synthetic PTY render only; screenshots require visual review; shell draft/paste unverified'), indent=2))
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, default=Path('gtk-paint-diagnostics/f13'))
    parser.add_argument('--session', action='store_true')
    args = parser.parse_args()
    if not args.session:
        subprocess.run(['dbus-run-session', '--', sys.executable, __file__, '--session', '--output', str(args.output)], check=True)
        return
    for command in ('cargo', 'sway', 'swaymsg', 'grim', 'fc-match'):
        if shutil.which(command) is None:
            raise RuntimeError('Missing native prerequisite: ' + command)
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    faces = {}
    for font in FONTS:
        face = subprocess.check_output(['fc-match', '-f', '%{family}\n%{file}\n', font], text=True).splitlines()
        if font not in face[0].split(','):
            raise RuntimeError('Requested font is not installed: ' + font)
        faces[font] = dict(family=face[0], path=face[1], sha256=hashlib.sha256(Path(face[1]).read_bytes()).hexdigest())
    build = subprocess.run(['cargo', 'test', '--lib', '--all-features', '--locked', '--no-run', '--message-format=json'],
                           check=True, capture_output=True, text=True)
    (out / 'cargo-artifacts.jsonl').write_text(build.stdout)
    paths = {item['executable'] for line in build.stdout.splitlines() if (item := json.loads(line)).get('reason') == 'compiler-artifact'
             and item.get('executable') and item.get('profile', {}).get('test') and 'lib' in item.get('target', {}).get('kind', [])}
    if len(paths) != 1:
        raise RuntimeError('Expected one exact lib-test binary')
    binary = Path(paths.pop()).resolve(strict=True)
    listed = subprocess.check_output([str(binary), '--ignored', '--list'], text=True)
    if TEST + ': test' not in listed.splitlines():
        raise RuntimeError('F13 native fixture not registered')
    (out / 'identity.json').write_text(json.dumps(dict(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
        commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(), fonts=faces), indent=2))
    for scale in (1, 1.5, 2):
        for index, font in enumerate(FONTS):
            run_case(binary, out / f'scale-{scale}-font-{index}', scale, font)
    failures = []
    for case in sorted(out.glob('scale-*')):
        report = json.loads((case / 'result.json').read_text())
        for stage in report['stages']:
            try:
                horizontal_clearance(stage['native'])
            except RuntimeError as error:
                failures.append(dict(case=case.name, stage=stage['native']['stage'], error=str(error)))
    (out / 'horizontal-clearance.json').write_text(json.dumps(dict(failures=failures), indent=2))
    if failures:
        raise RuntimeError(f'{len(failures)} native horizontal-clearance regressions; all captures retained')
    (out / 'PASS').write_text('6 combinations / 108 captures; synthetic rendering only\n')


if __name__ == '__main__':
    main()
