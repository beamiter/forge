#!/usr/bin/env python3
"""F13 minimal actual keyboard/clipboard -> Bash acceptance stage."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys

spec = importlib.util.spec_from_file_location('render_matrix', Path(__file__).with_name('test-chat-render-matrix.py'))
matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(matrix)
wait_for, measured = matrix.wait_for, matrix.measured
horizontal_clearance, PHYSICAL = matrix.horizontal_clearance, matrix.PHYSICAL
TEST = 'block_view::chat_shell_tests::chat_real_bash_keyboard_and_clipboard'
FONTS = ('DejaVu Sans Mono',)

def shell_acceptance(records):
    if len(records) != 2 or [r['path'] for r in records] != ['key', 'pst']:
        raise RuntimeError('Missing independent keyboard/clipboard results')
    first, second = records
    if (first['token'] == second['token'] or first['pid'] <= 0 or
            first['pid'] != second['pid'] or first['tty'] != second['tty'] or
            not first['tty'].startswith('/dev/pts/') or first['capsule'] != second['capsule']):
        raise RuntimeError('Wrong shell ownership, token freshness or capsule metrics')
    for record in records:
        parts = bytes.fromhex(record['accepted_hex']).split(b'\0')
        expected = [record['token'].encode(), ('中文 e\u0301 ＡＢＣ ' * 8).encode(),
                    str(record['pid']).encode(), record['tty'].encode(), b'']
        if parts != expected:
            raise RuntimeError('Actual shell accepted bytes differ from expected input')


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
            for stage in range(8):
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
            shell_acceptance(json.loads((out / 'shell-acceptance.json').read_text()))
            (out / 'result.json').write_text(json.dumps(dict(status='pass', stages=reports,
                scope='real Bash acceptance bytes; global paste shortcut and complete draft snapshot unverified'), indent=2))
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
    parser.add_argument('--output', type=Path, default=Path('gtk-paint-diagnostics/f13-shell'))
    parser.add_argument('--session', action='store_true')
    args = parser.parse_args()
    if not args.session:
        subprocess.run(['dbus-run-session', '--', sys.executable, __file__, '--session', '--output', str(args.output)], check=True)
        return
    for command in ('cargo', 'sway', 'swaymsg', 'grim', 'fc-match', 'wtype'):
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
    for scale in (1,):
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
    (out / 'PASS').write_text('1 combination / 8 captures; real Bash keyboard and clipboard smoke\n')


if __name__ == '__main__':
    main()
