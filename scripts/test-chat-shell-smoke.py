#!/usr/bin/env python3
"""F13 minimal actual keyboard/clipboard -> Bash acceptance stage."""
import argparse
import hashlib
import importlib.util
import json
import os
import re
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile

spec = importlib.util.spec_from_file_location('render_matrix', Path(__file__).with_name('test-chat-render-matrix.py'))
matrix = importlib.util.module_from_spec(spec)
spec.loader.exec_module(matrix)
wait_for, measured = matrix.wait_for, matrix.measured
horizontal_clearance, PHYSICAL = matrix.horizontal_clearance, matrix.PHYSICAL
TEST = 'block_view::chat_shell_tests::chat_real_bash_keyboard_and_clipboard'
METADATA_TEST = 'block_view::chat_shell_tests::chat_real_bash_history_metadata'
FONTS = ('DejaVu Sans Mono',)

# The nonempty F24 keymap is parsed before command execution. The first command
# blocks on our unwritten stdin pipe; it is NEVER released, even on cleanup.
SEAT_KEYBOARD_ARGS = ['wtype', '-', '-k', 'F24']


def keyboard_trace_ready(trace):
    if re.search(r'-> zwp_virtual_keyboard_v1[@#]\d+\.(key|modifiers)\(', trace):
        raise RuntimeError('Seat-only keyboard emitted input')
    return bool(re.search(r'-> zwp_virtual_keyboard_v1[@#]\d+\.keymap\(', trace))


def keyboard_seat_ready(inputs, seats):
    keyboards = [i for i in inputs if i.get('type') == 'keyboard']
    if len(keyboards) != 1 or len(inputs) != 1:
        return False
    identifier = keyboards[0].get('identifier')
    return bool(identifier and len(seats) == 1 and seats[0].get('name') == 'seat0'
                and seats[0].get('capabilities', 0) & 2
                and [d.get('identifier') for d in seats[0].get('devices', [])] == [identifier])


def stop_owned(process):
    # Kill/reap BEFORE closing stdin: EOF must never release a queued key.
    if process.poll() is None:
        try:
            process.terminate()
        except ProcessLookupError:
            pass  # Already exited between poll and terminate; still reap it.
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                process.kill()
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
    if process.stdin is not None and not process.stdin.closed:
        process.stdin.close()


def finish_outcome(out, result, original_error, processes):
    errors = []
    for process in reversed(processes):
        try:
            stop_owned(process)
        except Exception as error:
            errors.append(f'cleanup: {error!r}')
    try:
        if not keyboard_trace_ready((out / 'seat-keyboard.log').read_text()):
            raise RuntimeError('Missing persistent keyboard keymap trace')
    except Exception as error:
        errors.append(f'keyboard trace: {error!r}')
    passed = result is not None and original_error is None and not errors
    outcome = dict(result or {})
    outcome.update(status='pass' if passed else 'fail',
                   original_error=repr(original_error) if original_error is not None else None,
                   cleanup_errors=errors)
    if not passed:
        (out / 'result.json').write_text(json.dumps(outcome, indent=2))
    if errors and original_error is None:
        raise RuntimeError('Owned fixture cleanup failed: ' + '; '.join(errors))
    return outcome


def shell_acceptance(records, metadata=False):
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
        if metadata:
            identity = record['metadata_identity']
            execution_id = identity['execution_id']
            command = ("printf '%s\\0' '" + record['token'] + "' '" +
                       ('中文 e\u0301 ＡＢＣ ' * 8) +
                       "' \"$BASHPID\" \"$(tty)\" \"$__forge_command_id\" \"$BASH_COMMAND\" > \"$HOME/" + record['path'] + "\"")
            expected[-1:] = [execution_id.encode(), command.encode(), b'']
            if (not re.fullmatch(r'forge-bash-' + str(record['pid']) + r'-[1-9][0-9]*', execution_id)
                    or identity['expected_command'] != command or identity['capsule_command'] != command
                    or identity['stored_command_exact'] is not True):
                raise RuntimeError('Metadata command identity/provenance differs from accepted Bash')
            events = identity['events']
            if (len(events) != 3 or [event['phase'] for event in events] != ['start', 'end', 'record']
                    or any(event['execution_id'] != execution_id for event in events)):
                raise RuntimeError('Missing same-execution C/D/record chain')
            start, end, complete = events
            if (start['parsed_command'] != command or start['resolved_command'] != command
                    or complete['command'] != command
                    or start['command_source'] != 'ShellReported'
                    or complete['command_source'] != 'ShellReported'
                    or complete['completion_provenance'] != 'ShellReported'
                    or start['block_id'] != identity['block_id']
                    or complete['block_id'] != identity['block_id']
                    or end['correlation'] != 'MatchedUntrusted' or end['exit_code'] != 0):
                raise RuntimeError('Wrong parsed command, exactness or capsule correlation')
        if parts != expected:
            raise RuntimeError('Actual shell accepted bytes differ from expected input')
    if metadata:
        if first['metadata_identity']['execution_id'] == second['metadata_identity']['execution_id']:
            raise RuntimeError('Reused execution id')
        if first['metadata_identity']['block_id'] == second['metadata_identity']['block_id']:
            raise RuntimeError('Reused capsule id')


def metadata_events_acceptance(records, events):
    expected = [event for record in records for event in record['metadata_identity']['events']]
    if len(events) != 6 or events != expected:
        raise RuntimeError('Extra or reordered accepted command lifecycles')


def private_runtime():
    # AF_UNIX sockaddr paths include Sway's generated IPC basename. Keep the
    # runtime independent of checkout/artifact depth and inherited TMPDIR.
    return tempfile.TemporaryDirectory(prefix='forge-f13-', dir='/tmp')


def run_case(binary, out, scale, font, metadata=False):
    try:
        with private_runtime() as runtime:
            outcome = _run_case(binary, out, scale, font, Path(runtime), metadata)
    except BaseException as error:
        if out.is_dir():
            report = out / 'result.json'
            previous = json.loads(report.read_text()) if report.exists() else {}
            previous.update(status='fail', outer_error=repr(error))
            report.write_text(json.dumps(previous, indent=2))
        raise
    # Only publish pass after process AND owned runtime-directory cleanup.
    (out / 'result.json').write_text(json.dumps(outcome, indent=2))


def _run_case(binary, out, scale, font, runtime, metadata=False):
    out.mkdir()
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
    result = None
    original_error = None
    def run(args):
        return subprocess.run(args, env=env, check=True, capture_output=True, text=True, timeout=10).stdout
    def query(kind):
        return json.loads(run(['swaymsg', '-r', '-t', 'get_' + kind]))
    with (out / 'sway.log').open('w') as sway_log, (out / 'fixture.log').open('w') as fixture_log, (out / 'seat-keyboard.log').open('w') as keyboard_log:
        try:
            sway = subprocess.Popen(['sway', '-d', '-c', str(config)], env=env, stdout=sway_log, stderr=subprocess.STDOUT)
            processes.append(sway)
            sock = wait_for(lambda: next((p for p in runtime.glob('wayland-*') if p.is_socket()), None), 'Wayland socket', sway)
            ipc = wait_for(lambda: next(runtime.glob('sway-ipc.*.sock'), None), 'Sway IPC', sway)
            env.update(WAYLAND_DISPLAY=sock.name, SWAYSOCK=str(ipc))
            initial_inputs, initial_seats = query('inputs'), query('seats')
            (out / 'keyboard-before.json').write_text(json.dumps(dict(inputs=initial_inputs, seats=initial_seats)))
            if (initial_inputs or len(initial_seats) != 1 or initial_seats[0].get('name') != 'seat0'
                    or initial_seats[0].get('capabilities') != 0 or initial_seats[0].get('devices')):
                raise RuntimeError('Expected fresh empty owned headless seat')
            keyboard = subprocess.Popen(SEAT_KEYBOARD_ARGS, env=dict(env, WAYLAND_DEBUG='client'),
                                        stdin=subprocess.PIPE, stdout=keyboard_log, stderr=keyboard_log)
            processes.append(keyboard)
            def seat_ready():
                trace = (out / 'seat-keyboard.log').read_text()
                inputs, seats = query('inputs'), query('seats')
                if keyboard_trace_ready(trace) and keyboard_seat_ready(inputs, seats):
                    (out / 'keyboard-ready.json').write_text(json.dumps(dict(inputs=inputs, seats=seats)))
                    return True
                return False
            wait_for(seat_ready, 'persistent native keyboard seat', keyboard)
            fixture = subprocess.Popen([str(binary), METADATA_TEST if metadata else TEST, '--ignored', '--exact', '--nocapture'],
                                       env=env, stdout=fixture_log, stderr=subprocess.STDOUT)
            processes.append(fixture)
            wait_for((out / 'input-ready.json').exists, 'GTK focus before input', fixture)
            ready = json.loads((out / 'input-ready.json').read_text())
            actual = measured(query('outputs'), query('tree'), fixture.pid, scale, ready['window'])
            seats = query('seats')
            if (not seat_ready() or seats[0].get('focus') != actual['node_id'] or
                    not ready['has_focus'] or not ready['window_active']):
                raise RuntimeError('Owned view/seat/GTK focus is not ready for input')
            (out / 'input-ready-compositor.json').write_text(json.dumps(dict(native=ready, compositor=actual, seats=seats)))
            (out / 'input-ready.ack').touch()
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
                if keyboard.poll() is not None or not keyboard_trace_ready((out / 'seat-keyboard.log').read_text()):
                    raise RuntimeError('Seat keyboard lost or contaminated')
                reports.append(dict(native=report, compositor=before, screenshot_sha256=hashlib.sha256(png).hexdigest()))
                (out / f'stage-{stage}.ack').touch()
            if fixture.wait(timeout=20) != 0:
                raise RuntimeError('Native fixture failed; see fixture.log')
            accepted = json.loads((out / 'shell-acceptance.json').read_text())
            shell_acceptance(accepted, metadata)
            if metadata:
                metadata_events_acceptance(accepted, json.loads((out / 'metadata-events.json').read_text()))
            result = dict(stages=reports, history_metadata=metadata,
                scope=('owned simple printf accepted BASH_COMMAND bytes, parsed C/D id, ShellReported record and capsule identity; global paste shortcut unverified' if metadata else
                       'real Bash acceptance bytes; global paste shortcut and complete draft snapshot unverified'))
        except BaseException as error:
            original_error = error
            raise
        finally:
            outcome = finish_outcome(out, result, original_error, processes)
    return outcome



def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, default=Path('gtk-paint-diagnostics/f13-shell'))
    parser.add_argument('--session', action='store_true')
    parser.add_argument('--metadata', action='store_true', help='Separate history-enabled same-execution identity lane')
    args = parser.parse_args()
    if not args.session:
        subprocess.run(['dbus-run-session', '--', sys.executable, __file__, '--session', '--output', str(args.output)] + (['--metadata'] if args.metadata else []), check=True)
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
    selected_test = METADATA_TEST if args.metadata else TEST
    if selected_test + ': test' not in listed.splitlines():
        raise RuntimeError('F13 native fixture not registered')
    (out / 'identity.json').write_text(json.dumps(dict(binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
        test=selected_test, history_metadata=args.metadata, commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(), fonts=faces), indent=2))
    for scale in (1,):
        for index, font in enumerate(FONTS):
            run_case(binary, out / f'scale-{scale}-font-{index}', scale, font, args.metadata)
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
    (out / 'PASS').write_text('1 combination / 8 captures; ' +
        ('history-enabled same-execution command identity\n' if args.metadata else
         'real Bash keyboard and clipboard smoke\n'))


if __name__ == '__main__':
    main()
