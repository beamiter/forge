#!/usr/bin/env python3
"""Pure contracts: no network, installation, host, native app or provider runs."""
import copy
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from unittest.mock import patch

import finish
import provenance as p

HERE = Path(__file__).resolve().parent
WORKFLOW = HERE.parents[2] / '.github/workflows/four-native-mission.yml'
ENV = {'GITHUB_REPOSITORY': 'beamiter/forge', 'GITHUB_SHA': '3' * 40,
       'GITHUB_REF': 'refs/heads/master', 'GITHUB_RUN_ID': '123', 'GITHUB_RUN_ATTEMPT': '1'}


class ObserverCompatibilityJobTests(unittest.TestCase):
    def job(self):
        source = (HERE.parents[2] / '.github/workflows/ci.yml').read_text()
        start = source.index('  native-observer-compatibility:\n')
        match = re.search(r'\n  [a-z][a-z0-9-]*:\n', source[start + 1:])
        self.assertIsNotNone(match)
        return source[start:start + 1 + match.start()]

    def test_only_read_permission_and_pinned_no_credential_checkout(self):
        job = self.job()
        self.assertIn('    permissions:\n      contents: read\n', job)
        self.assertIn('persist-credentials: false', job)
        for action in re.findall(r'uses: ([^\s]+)', job):
            self.assertRegex(action, r'^[a-zA-Z0-9_-]+/[a-zA-Z0-9_-]+@[0-9a-f]{40}$')
        for forbidden in ('write', 'secrets.', 'workflow_dispatch', 'git push', 'create_commit'):
            self.assertNotIn(forbidden, job)

    def test_image_tools_exist_before_normal_and_optimized_contracts(self):
        job = self.job()
        install = job.index('timeout 350 bash scripts/mission-acceptance/native/build.sh packages runtime')
        tools = job.index('command -v convert tesseract')
        normal = job.index('python3 -B -m unittest discover')
        optimized = job.index('PYTHONOPTIMIZE=1 python3 -B -m unittest discover')
        self.assertLess(install, tools)
        self.assertLess(tools, normal)
        self.assertLess(normal, optimized)
        self.assertIn('timeout-minutes: 6', job)

    def test_opt_in_fixture_uses_private_bus_display_and_clean_environment(self):
        job = self.job()
        for required in ('env -i PATH=/usr/bin:/bin', 'HOME="$private/home"',
                         'XDG_CONFIG_HOME="$private/config"', 'XDG_DATA_HOME="$private/data"',
                         'XDG_STATE_HOME="$private/state"', 'XDG_CACHE_HOME="$private/cache"',
                         'XDG_RUNTIME_DIR="$private/runtime"', 'JTERM_GTK_GEOMETRY_SMOKE=1',
                         'dbus-run-session -- xvfb-run --auto-servernum', '-nolisten tcp',
                         "-p 'test_gtk_geometry_smoke.py'"):
            self.assertIn(required, job)
        for forbidden in ('run.sh', 'jterm-sessiond', 'mission-native-state', 'DISPLAY=$',
                          'DBUS_SESSION_BUS_ADDRESS=$'):
            self.assertNotIn(forbidden, job)

    def test_artifact_is_only_the_fixture_evidence_directory(self):
        job = self.job()
        paths = re.findall(r'^          path: (.+)$', job, re.MULTILINE)
        self.assertEqual(paths, ['${{ runner.temp }}/native-observer-compatibility/'])
        self.assertIn('JTERM_GTK_GEOMETRY_SMOKE_OUT="$evidence"', job)
        self.assertIn('if: ${{ always() }}', job)
        self.assertIn('retention-days: 7', job)


class CleanupTrapTests(unittest.TestCase):
    def cleanup_script(self):
        source = (HERE / 'run.sh').read_text()
        start = source.index('cleanup() {\n')
        end = source.index('trap cleanup EXIT\n', start) + len('trap cleanup EXIT\n')
        return source[start:end]

    def test_exit_trap_uses_only_recorded_positive_child_pids(self):
        # These shell functions record calls only. They do not signal processes.
        script = '''set -Eeuo pipefail
kill() { printf 'kill:%s\\n' "$*"; return 1; }
wait() { printf 'wait:%s\\n' "$*"; return 1; }
host=321; wm=0; display=654
''' + self.cleanup_script() + 'exit 7\n'
        result = subprocess.run(['bash', '-c', script], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout.splitlines(), ['kill:321', 'wait:321', 'kill:654', 'wait:654'])
        self.assertEqual(result.stderr, '')

    def test_exit_trap_before_startup_never_signals_process_group(self):
        script = '''set -Eeuo pipefail
kill() { printf 'unexpected kill:%s\\n' "$*"; return 1; }
wait() { printf 'unexpected wait:%s\\n' "$*"; return 1; }
host=0; wm=0; display=0
''' + self.cleanup_script() + 'exit 9\n'
        result = subprocess.run(['bash', '-c', script], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 9)
        self.assertEqual(result.stdout, '')
        self.assertEqual(result.stderr, '')


class GtkDependencyAcquisitionTests(unittest.TestCase):
    workflow_file = 'ci.yml'
    step_name = 'Install GTK display test dependencies'
    expected_install = [
        'install', '--no-install-recommends', '-y', 'pkg-config', 'libgtk-4-dev',
        'libadwaita-1-dev', 'libvte-2.91-gtk4-dev', 'vim', 'ripgrep', 'libpcre2-dev',
        'xvfb', 'xdotool', 'xauth', 'libxtst6', 'python3', 'gdb', 'dbus-x11',
        'sway', 'wtype', 'grim', 'fonts-dejavu-core', 'fonts-liberation',
        'fonts-noto-cjk', 'fonts-noto-color-emoji']

    def step(self):
        workflow = (HERE.parents[2] / '.github/workflows' / self.workflow_file).read_text()
        start = workflow.index('      - name: ' + self.step_name + '\n')
        end = workflow.index('\n      - name:', start + 1)
        return workflow[start:end]

    def run_step(self, failure, fail_at='update'):
        step = self.step()
        self.assertIn('        timeout-minutes: 6\n', step)
        self.assertIn('        shell: bash\n', step)
        body = step.split('        run: |\n', 1)[1]
        script = '\n'.join(line[10:] for line in body.splitlines()) + '\n'
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            log = root / 'calls.jsonl'
            # Execute the actual step shell with an inert sudo replacement.
            # It records apt's arguments and source bytes; it never runs apt.
            fake = root / 'sudo'
            fake.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
if not args or args[0] != 'apt-get':
    raise SystemExit(91)
source = next(a.split('=', 1)[1] for a in args if a.startswith('Dir::Etc::sourcelist='))
with open(os.environ['APT_LOG'], 'a') as stream:
    stream.write(json.dumps({'args': args, 'source': source, 'text': pathlib.Path(source).read_text(),
                            'mode': pathlib.Path(source).stat().st_mode & 0o777}) + '\\n')
raise SystemExit(int(os.environ['APT_FAILURE']) if os.environ['APT_FAIL_AT'] in args else 0)
''')
            fake.chmod(0o755)
            env = {**os.environ, 'PATH': str(root) + os.pathsep + os.environ['PATH'],
                   'TMPDIR': directory, 'APT_LOG': str(log), 'APT_FAILURE': str(failure),
                   'APT_FAIL_AT': fail_at}
            result = subprocess.run(['bash', '-c', script], env=env, capture_output=True,
                                    text=True, timeout=5)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(result.returncode, failure, result.stderr)
            for call in calls:
                self.assertEqual(Path(call['source']).parent, root)
                self.assertEqual(call['mode'], 0o644)
                self.assertFalse(Path(call['source']).exists(), 'Temporary apt source was not cleaned')
            return calls

    def test_official_signed_sources_bounds_and_all_native_packages(self):
        calls = self.run_step(0)
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0]['args'][-1], 'update')
        sources = [
            'deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu noble main universe',
            'deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://archive.ubuntu.com/ubuntu noble-updates main universe',
            'deb [signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://security.ubuntu.com/ubuntu noble-security main universe',
        ]
        for call in calls:
            self.assertEqual(call['text'].splitlines(), sources)
            self.assertEqual(call['source'], calls[0]['source'])
        options = ['apt-get', '-o', 'Dir::Etc::sourcelist=' + calls[0]['source'],
                   '-o', 'Dir::Etc::sourceparts=-', '-o', 'Acquire::Retries=2',
                   '-o', 'Acquire::http::Timeout=30', '-o', 'Acquire::https::Timeout=30']
        self.assertEqual(calls[0]['args'], options + ['update'])
        self.assertEqual(calls[1]['args'], options + self.expected_install)

    def test_failed_update_stops_before_install_and_cleans_source(self):
        calls = self.run_step(42)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]['args'][-1], 'update')

    def test_failed_install_preserves_failure_and_cleans_source(self):
        calls = self.run_step(43, fail_at='install')
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0]['args'][-1], 'update')
        self.assertIn('install', calls[1]['args'])


class LayoutDependencyAcquisitionTests(GtkDependencyAcquisitionTests):
    # Run the same production-shell failure injections for both workflows.
    workflow_file = 'ui-layout.yml'
    step_name = 'Install GTK, VTE, and isolated display'
    expected_install = [
        'install', '--no-install-recommends', '--yes', 'pkg-config', 'libgtk-4-dev',
        'libadwaita-1-dev', 'libvte-2.91-gtk4-dev', 'libpcre2-dev', 'dbus-x11',
        'xauth', 'xvfb', 'fonts-dejavu-core', 'fonts-noto-cjk']

    def test_layout_job_and_required_test_gates_are_preserved(self):
        workflow = (HERE.parents[2] / '.github/workflows' / self.workflow_file).read_text()
        self.assertIn('permissions:\n  contents: read\n', workflow)
        self.assertIn('concurrency:\n  group: ui-layout-${{ github.ref }}\n  cancel-in-progress: true\n', workflow)
        self.assertIn('    runs-on: ubuntu-24.04\n    timeout-minutes: 30\n', workflow)
        self.assertNotIn('continue-on-error:', workflow)
        self.assertNotIn('        if:', workflow)
        self.assertIn('      - name: Compile application tests\n'
                      '        run: cargo test --locked --lib --all-features --no-run\n', workflow)
        required = workflow.split('          tests=(\n', 1)[1].split('          )\n', 1)[0]
        self.assertEqual(required.split(), [
            'pty::tests::live_display_fixture_reaps_its_idle_child_on_drop',
            'block_view::chat::tests::composer_keeps_three_input_rows_and_clearance_within_the_pane',
            'block_view::chat::tests::real_editor_minimum_uses_native_cell_height_and_bounded_canvas_budget',
            'block_view::chat::tests::bounded_editor_reveals_caret_without_changing_native_ring_or_history',
            'block_view::developer_block::tests::block_input_rows_keep_three_lines_plus_clearance_and_follow_native_content',
            'block_view::chat_layout_tests::chat_bubbles_keep_alignment_through_resize_and_font_changes',
            'ui::reference_conversation::tests::user_capsules_follow_reading_edge_and_wrap_in_narrow_windows',
            'block_view::chat::tests::chat_moves_only_the_native_clip_and_keeps_history_anchor',
            'block_view::tests::multiline_native_caret_fits_editor_and_footer',
            'block_view::tests::chat_docking_keeps_pty_rows_and_native_hit_geometry_stable'])
        tail = workflow.split('          )\n', 1)[1]
        self.assertEqual(tail, '''          listed_tests="$(cargo test --locked --lib --all-features -- --list --include-ignored)"
          failed=0
          for test_name in "${tests[@]}"; do
            if ! grep -Fxq "${test_name}: test" <<< "$listed_tests"; then
              printf 'Required layout test not found: %s\\n' "$test_name" >&2
              failed=1
              continue
            fi
            if ! dbus-run-session -- xvfb-run --auto-servernum \\
              cargo test --locked --lib --all-features "$test_name" \\
              -- --exact --include-ignored --test-threads=1; then
              failed=1
            fi
          done
          exit "$failed"
''')


class RuntimeFailureDiagnosticsTests(unittest.TestCase):
    def test_actual_shell_tail_preserves_both_exit_codes(self):
        source = (HERE / 'run.sh').read_text()
        tail = source[source.index('# Preserve both independent failures;'):]
        for reader_code, state_code in ((0, 0), (1, 2), (0, 2), (1, 0)):
            with self.subTest(reader=reader_code, state=state_code), tempfile.TemporaryDirectory() as directory:
                script = 'reader_status=$1; state_status=$2; evidence=$3\n' + tail
                result = subprocess.run(['bash', '-c', script, 'test-exit-codes', str(reader_code),
                                         str(state_code), directory], capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, reader_code or state_code, result.stderr)
                self.assertEqual(json.loads((Path(directory) / 'native-exit-codes.json').read_text()), {
                    'schema': 1, 'scope': 'process_exit_codes_only_not_verification',
                    'reader_exit_code': reader_code, 'state_check_exit_code': state_code})

    def test_host_and_reader_errors_remain_separate_diagnostics(self):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            (evidence / 'native-reader').mkdir()
            (evidence / 'native-reader/result.json').write_text(json.dumps({
                'applications': {'anvil': {'status': 'failed', 'error': 'No owned app window'},
                                 'foreign': {'error': 'not an allowed application'}}}))
            (evidence / 'native-state-check.json').write_text(json.dumps({
                'status': 'failed', 'error': 'conversation_catalog_not_empty'}))
            (evidence / 'native-exit-codes.json').write_text(json.dumps({
                'scope': 'process_exit_codes_only_not_verification',
                'reader_exit_code': 1, 'state_check_exit_code': 2}))
            details = finish.failure_details(evidence)
            self.assertEqual(details, {'scope': 'diagnostic_only_not_verification',
                'application_errors': {'anvil': 'No owned app window'},
                'host_state_error': 'conversation_catalog_not_empty',
                'reader_exit_code': 1, 'state_check_exit_code': 2})

    def test_missing_oversize_or_symlink_diagnostics_do_not_become_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            evidence = Path(directory)
            expected = {'scope': 'diagnostic_only_not_verification', 'application_errors': {},
                        'host_state_error': 'no bounded error receipt'}
            self.assertEqual(finish.failure_details(evidence), expected)
            (evidence / 'native-state-check.json').write_text('x' * (1024 * 1024 + 1))
            self.assertEqual(finish.failure_details(evidence), expected)
            (evidence / 'native-state-check.json').unlink()
            (evidence / 'target.json').write_text('{"error":"must not follow"}')
            (evidence / 'native-state-check.json').symlink_to(evidence / 'target.json')
            self.assertEqual(finish.failure_details(evidence), expected)
            (evidence / 'native-state-check.json').unlink()
            os.mkfifo(evidence / 'native-state-check.json')
            self.assertEqual(finish.failure_details(evidence), expected)


def candidates():
    return {'schema': 1, 'contract': 'five-repository-host-v1', 'frozen': True,
            'repositories': [{'name': name, 'full_name': 'beamiter/' + name,
                              'revision': p.collector.CORE if name == 'jterm_core' else str(i) * 40}
                             for i, name in enumerate(p.collector.NAMES, 1)]}


def complete_coverage(app, mission):
    # Schema-only fixture. These names explicitly are not native observations.
    stages = ['initial', 'inspected', 'refreshed', 'app_reopened']
    if app != 'forge':
        stages.append('mission_reopened')
    observations = [{'stage': stage, 'evidence': 'pure-contract-fixture-' + stage} for stage in stages]
    names = [repo['identity']['full_name'] for repo in mission['repositories']]
    cores = [repo['identity']['full_name'] for repo in mission['repositories'] if repo.get('required_core_pin')]
    return {'fields': {}, 'identity_bindings': copy.deepcopy(observations),
            'candidate_bindings': {name: copy.deepcopy(observations) for name in names},
            'core_pin_bindings': {name: copy.deepcopy(observations) for name in cores},
            'stages': {'mission_close': app != 'forge', 'inspected': 'scan_complete',
                       'normal_close_1': True, 'normal_close_2': True,
                       **{'launch_' + str(cycle): {'pid': 1000 + cycle,
                            'identity': {'pid': 1000 + cycle, 'start_ticks': str(2000 + cycle),
                                         'executable': '/tmp/pure-contract-fixture/installed/' + app + '/.local/bin/' + app,
                                         'state_dir': '/tmp/state'}} for cycle in (1, 2)},
                       'inspection_ready': {'status': 'settled'},
                       'freeze_after_refresh': {'status': 'disabled' if app != 'frost' else 'not_verified'}},
            'reasons': [{'evidence': 'pure-contract-fixture-needs-you'}],
            'inspection_status': [{'stage': 'inspected', 'evidence': 'pure-contract-fixture-settled'}],
            'inspection_errors': [],
            'inspection_validation': {name: [{'stage': 'inspected', 'evidence': 'pure-contract-fixture-not-run'}]
                                      for name in names}}


def results():
    mission = {'id': 'shared-plan', 'revision': 2, 'title': 'Pure receipt contract fixture',
               'goal': 'No native execution', 'required_environment': 'isolated-linux-native',
               'repositories': [{'identity': {'full_name': item['full_name']},
                                  'candidate': item['revision'], 'baseline': item['revision'],
                                  'path': '/tmp/pure-contract-fixture/' + item['name'],
                                  'target_ref': 'refs/heads/master', 'required_checks': ['validation'],
                                  'tasks': [], 'required_core_pin': None if item['name'] == 'jterm_core' else p.collector.CORE}
                                 for item in candidates()['repositories']]}

    host = {'pid': 123, 'start_ticks': '456', 'executable': '/tmp/host', 'state_dir': '/tmp/state'}
    seed = {'schema': 1, 'core': p.collector.CORE, 'mission': mission, 'host': host}
    state = {'schema': 1, 'status': 'passed', 'scope': 'host_state_check_only_not_native_UI',
             'core': p.collector.CORE, 'host': host, 'mission': mission, 'full_plan_unchanged': True,
             'native_catalog_empty': True, 'conversation_catalog_empty': True,
             'provider_execution': 'skipped_by_user', 'hosted_task_validation': 'not_run',
             'five_repository_delivery': 'not_completed'}
    reader = {'schema': 1, 'status': 'passed', 'scope': 'actual_native_existing_mission_read_only', 'applications': {app: {'status': 'passed', 'coverage': complete_coverage(app, mission)} for app in p.APPS},
              'mission_id': mission['id'], 'mission_revision': mission['revision'], 'host': host,
              'provider_execution': 'skipped_by_user', 'full_delivery': 'not_claimed'}
    return copy.deepcopy(seed), copy.deepcopy(reader), copy.deepcopy(state)


class FinalScopeTests(unittest.TestCase):
    def test_complete_separate_native_scope(self):
        seed, reader, state = results()
        self.assertEqual(finish.validate_results(seed, reader, state), (seed['mission'], seed['host']))


    def test_passed_status_cannot_hide_missing_or_malformed_coverage(self):
        for app in p.APPS:
            for coverage in (None, {}, [], 'passed', {'fields': {}, 'candidate_bindings': {}, 'stages': {}}):
                data = list(results()); data[1]['applications'][app]['coverage'] = coverage
                with self.subTest(app=app, coverage=coverage), self.assertRaises(ValueError):
                    finish.validate_results(*data)
            data = list(results()); del data[1]['applications'][app]['coverage']
            with self.subTest(app=app, absent=True), self.assertRaises(ValueError):
                finish.validate_results(*data)

    def test_passed_status_cannot_hide_any_required_observation_stage(self):
        for app in p.APPS:
            stages = ['initial', 'inspected', 'refreshed', 'app_reopened']
            if app != 'forge': stages.append('mission_reopened')
            for stage in stages:
                for kind in ('identity_bindings', 'candidate_bindings', 'core_pin_bindings'):
                    data = list(results()); coverage = data[1]['applications'][app]['coverage']
                    target = coverage[kind] if kind == 'identity_bindings' else coverage[kind]['beamiter/anvil']
                    target[:] = [event for event in target if event['stage'] != stage]
                    with self.subTest(app=app, stage=stage, kind=kind), self.assertRaises(ValueError):
                        finish.validate_results(*data)

    def test_passed_status_cannot_hide_truncated_reason_or_inspection_evidence(self):
        for app in p.APPS:
            for field, value in [('reasons', []), ('inspection_status', []), ('inspection_validation', {}),
                                 ('inspection_errors', ['pure-test-error'])]:
                data = list(results()); data[1]['applications'][app]['coverage'][field] = value
                with self.subTest(app=app, field=field), self.assertRaises(ValueError):
                    finish.validate_results(*data)
            for stage, value in [('inspected', True), ('inspection_ready', {'status': 'pending'})]:
                data = list(results()); data[1]['applications'][app]['coverage']['stages'][stage] = value
                with self.subTest(app=app, stage=stage), self.assertRaises(ValueError):
                    finish.validate_results(*data)


    def test_close_and_relaunch_receipts_are_mandatory(self):
        for app in p.APPS:
            for cycle in (1, 2):
                for value in (None, False, 1):
                    data = list(results())
                    data[1]['applications'][app]['coverage']['stages']['normal_close_' + str(cycle)] = value
                    with self.subTest(app=app, cycle=cycle, close=value), self.assertRaises(ValueError):
                        finish.validate_results(*data)
                for value in (None, {}, {'pid': 1000 + cycle}):
                    data = list(results())
                    data[1]['applications'][app]['coverage']['stages']['launch_' + str(cycle)] = value
                    with self.subTest(app=app, cycle=cycle, launch=value), self.assertRaises(ValueError):
                        finish.validate_results(*data)

    def test_malformed_or_unrestarted_process_identity_fails(self):
        changes = [('pid', 1), ('pid', True), ('pid', 999), ('start_ticks', 'not-a-birth-tick'),
                   ('start_ticks', '１２３'), ('start_ticks', 2001), ('executable', 'relative-app'),
                   ('executable', '/tmp/other-app'), ('state_dir', '/tmp/other-state')]
        for app in p.APPS:
            for key, value in changes:
                data = list(results())
                data[1]['applications'][app]['coverage']['stages']['launch_1']['identity'][key] = value
                with self.subTest(app=app, key=key, value=value), self.assertRaises(ValueError):
                    finish.validate_results(*data)
            data = list(results()); stages = data[1]['applications'][app]['coverage']['stages']
            stages['launch_2'] = copy.deepcopy(stages['launch_1'])
            with self.subTest(app=app, unchanged_process=True), self.assertRaises(ValueError):
                finish.validate_results(*data)

    def test_every_missing_field_fails(self):
        for index, item in enumerate(results()):
            for key in item:
                data = list(results()); del data[index][key]
                with self.subTest(result=index, key=key), self.assertRaises((ValueError, KeyError, TypeError)):
                    finish.validate_results(*data)

    def test_failed_or_unrun_application_fails(self):
        for app in p.APPS:
            for status in ('failed', 'not_run', 'skipped', None):
                data = list(results()); data[1]['applications'][app]['status'] = status
                with self.subTest(app=app, status=status), self.assertRaises(ValueError):
                    finish.validate_results(*data)

    def test_failed_host_receipt_preserves_reason_and_never_passes(self):
        seed, reader, state = results()
        state.update(status='failed', error='conversation_catalog_not_empty')
        with self.assertRaisesRegex(ValueError, 'Host-state check did not pass: conversation_catalog_not_empty'):
            finish.validate_results(seed, reader, state)

    def test_missing_extra_application_fails(self):
        for name in p.APPS:
            data = list(results()); del data[1]['applications'][name]
            with self.assertRaises(ValueError): finish.validate_results(*data)
        data = list(results()); data[1]['applications']['fixture'] = {'status': 'passed'}
        with self.assertRaises(ValueError): finish.validate_results(*data)

    def test_changed_plan_host_identity_and_scope_fail(self):
        mutations = [(1, 'mission_id', 'other'), (1, 'mission_revision', 3),
                     (1, 'provider_execution', 'passed'), (1, 'full_delivery', 'completed'),
                     (1, 'host', {}), (2, 'host', {}), (2, 'mission', {}),
                     (2, 'full_plan_unchanged', False), (2, 'native_catalog_empty', False),
                     (2, 'conversation_catalog_empty', False), (2, 'core', '0' * 40),
                     (2, 'hosted_task_validation', 'passed'), (2, 'five_repository_delivery', 'completed')]
        for index, key, value in mutations:
            data = list(results()); data[index][key] = value
            with self.subTest(index=index, key=key), self.assertRaises(ValueError):
                finish.validate_results(*data)

    def test_legacy_finish_result_never_passes_native_gate(self):
        seed, _, state = results()
        with self.assertRaises(ValueError):
            finish.validate_results(seed, {'status': 'passed', 'delivery': 'incomplete',
                                          'provider_execution': 'not_run'}, state)


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.root = Path(self.temp.name)
        self.env = patch.dict(os.environ, ENV); self.env.start()
        self.manifest = candidates(); self.expected = p.collector.digest_set(self.manifest)
        p.write(self.root / 'frozen-candidates.json', self.manifest)
        (self.root / 'evidence').mkdir()
        self.source = self.root / 'repositories/anvil'; self.source.mkdir(parents=True)
        (self.source / 'Cargo.lock').write_text('# Pure test fixture, not runtime evidence\n')
        self.artifact = self.root / 'artifact'; (self.artifact / 'bin').mkdir(parents=True)
        (self.artifact / 'bin/anvil').write_bytes(b'\x7fELFpure-provenance-test-only')
        self.receipt = {'schema': 1, 'component': 'anvil', 'candidate_set_sha256': self.expected,
                        'workflow': ENV, 'source': self.manifest['repositories'][1],
                        'cargo_lock_sha256': p.digest(self.source / 'Cargo.lock'),
                        'rust_toolchain': '1.99.1', 'files_sha256': {'bin/anvil': p.digest(self.artifact / 'bin/anvil')},
                        'provider_execution': 'not_run', 'native_execution': 'not_run'}
        self.save()

    def tearDown(self):
        self.env.stop(); self.temp.cleanup()

    def save(self):
        p.write(self.artifact / 'build-receipt.json', self.receipt)

    def verify(self):
        return p.validate_receipt(self.root, self.expected, 'anvil', self.artifact)

    def test_valid_exact_same_run_receipt(self):
        self.assertEqual(self.verify(), self.receipt)

    def test_stale_foreign_attempt_ref_and_workflow_sha_rejected(self):
        for key, value in [('GITHUB_RUN_ID', '122'), ('GITHUB_RUN_ATTEMPT', '2'),
                           ('GITHUB_REF', 'refs/pull/1/merge'), ('GITHUB_REPOSITORY', 'other/forge'),
                           ('GITHUB_SHA', '9' * 40)]:
            self.receipt['workflow'] = {**ENV, key: value}; self.save()
            with self.subTest(key=key), self.assertRaises(ValueError): self.verify()

    def test_digest_component_lock_toolchain_source_scope_rejected(self):
        for key, value in [('candidate_set_sha256', '0' * 64), ('component', 'forge'),
                           ('cargo_lock_sha256', '0' * 64), ('rust_toolchain', 'stable'),
                           ('source', {}), ('provider_execution', 'passed'), ('native_execution', 'passed')]:
            previous = self.receipt[key]; self.receipt[key] = value; self.save()
            with self.subTest(key=key), self.assertRaises(ValueError): self.verify()
            self.receipt[key] = previous

    def test_missing_extra_and_modified_file_rejected(self):
        (self.artifact / 'extra').write_text('extra')
        with self.assertRaises(ValueError): self.verify()
        (self.artifact / 'extra').unlink()
        (self.artifact / 'bin/anvil').write_bytes(b'\x7fELFchanged')
        with self.assertRaises(ValueError): self.verify()
        (self.artifact / 'bin/anvil').unlink()
        with self.assertRaises(ValueError): self.verify()

    def test_symlink_rejected_even_with_matching_bytes(self):
        target = self.root / 'outside'; target.write_bytes((self.artifact / 'bin/anvil').read_bytes())
        (self.artifact / 'bin/anvil').unlink(); (self.artifact / 'bin/anvil').symlink_to(target)
        with self.assertRaises(ValueError): self.verify()

    def test_non_elf_rejected_even_with_matching_hash(self):
        path = self.artifact / 'bin/anvil'; path.write_text('#!/bin/sh\nexit 0\n')
        self.receipt['files_sha256']['bin/anvil'] = p.digest(path); self.save()
        with self.assertRaises(ValueError): self.verify()

    def test_unfrozen_injected_and_workflow_mismatched_sources_fail(self):
        for change in ('unfrozen', 'injection', 'other-workflow'):
            value = candidates()
            if change == 'unfrozen': value['frozen'] = False
            elif change == 'injection': value['repositories'][1]['revision'] = '$(touch /tmp/never)'
            else: value['repositories'][2]['revision'] = '9' * 40
            p.write(self.root / 'frozen-candidates.json', value)
            with self.subTest(change=change), self.assertRaises(ValueError):
                p.manifest(self.root, p.collector.digest_set(value))

    def test_unsafe_archive_never_extracted(self):
        (self.artifact / 'dist').mkdir()
        path = self.artifact / 'dist/anvil-test.tar.gz'
        for filename, kind in [('../escape', tarfile.REGTYPE), ('/absolute', tarfile.REGTYPE),
                               ('anvil-test/link', tarfile.SYMTYPE), ('anvil-test/dev', tarfile.CHRTYPE)]:
            with tarfile.open(path, 'w:gz') as archive:
                info = tarfile.TarInfo(filename); info.type = kind; info.size = 0
                archive.addfile(info, io.BytesIO())
            with self.subTest(filename=filename), self.assertRaises(ValueError):
                p.extract_bundle(self.artifact, self.root / 'unpacked', 'anvil')
            self.assertFalse((self.root / 'unpacked').exists())



class MetadataTests(unittest.TestCase):
    def fixture(self):
        core = 'git+https://github.com/beamiter/jterm_core.git?rev=' + p.collector.CORE + '#' + p.collector.CORE
        return {'resolve': {'root': 'app', 'nodes': [
                    {'id': 'app', 'deps': [{'pkg': 'core', 'dep_kinds': [{'kind': None}]},
                                          {'pkg': 'test-only', 'dep_kinds': [{'kind': 'dev'}]}]},
                    {'id': 'core', 'deps': []}, {'id': 'test-only', 'deps': []}]},
                'packages': [{'id': 'app', 'name': 'ember', 'version': '0.4.0', 'source': None, 'rust_version': '1.95'},
                             {'id': 'core', 'name': 'jterm_core', 'version': '0.2.0', 'source': core, 'rust_version': '1.86'},
                             {'id': 'test-only', 'name': 'test-fixture', 'version': '1.0.0', 'source': None, 'rust_version': '9.0'}]}

    def check(self, data, component='ember'):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); metadata = root / 'metadata.json'; out = root / 'receipt.json'
            p.write(metadata, data); p.metadata_receipt(metadata, out, component)
            return json.loads(out.read_text())

    def test_native_toolchain_exceeds_declared_egui_msrv(self):
        receipt = self.check(self.fixture())
        self.assertEqual(receipt['toolchain'], '1.99.1')
        self.assertEqual({pkg['name'] for pkg in receipt['packages']}, {'ember', 'jterm_core'})

    def test_future_msrv_or_wrong_core_fails(self):
        for change in ('future', 'wrong-core', 'missing-node'):
            data = self.fixture()
            if change == 'future': data['packages'][0]['rust_version'] = '1.100'
            elif change == 'wrong-core': data['packages'][1]['source'] = 'path+fixture'
            else: data['resolve']['nodes'] = data['resolve']['nodes'][:1]
            with self.subTest(change=change), self.assertRaises(ValueError): self.check(data)

    def test_core_msrv_is_not_native_global_default(self):
        data = self.fixture(); data['packages'][1]['source'] = None
        with self.assertRaises(ValueError): self.check(data, 'jterm_core')



class SecurityGateTests(unittest.TestCase):
    def item(self, name='forge'):
        return next(item for item in candidates()['repositories'] if item['name'] == name)

    def run_record(self, name='forge', **changes):
        item = self.item(name)
        return {'id': 10, 'run_attempt': 1, 'name': 'Security',
                'path': '.github/workflows/' + p.EXTRA_SECURITY_WORKFLOWS[name],
                'head_sha': item['revision'], 'head_branch': 'master', 'event': 'push',
                'status': 'completed', 'conclusion': 'success', **changes}

    def select(self, runs, name='forge', count=None):
        return p.select_security_run(self.item(name), {'total_count': len(runs) if count is None else count,
                                                       'workflow_runs': runs})

    def test_only_two_fixed_security_gates_allow_push_or_dispatch(self):
        self.assertEqual(p.EXTRA_SECURITY_WORKFLOWS, {'forge': 'security-audit.yml', 'frost': 'security.yml'})
        self.assertEqual(p.NATIVE_REQUIRED_WORKFLOW_COUNT, 21)
        for name in p.EXTRA_SECURITY_WORKFLOWS:
            for event in ('push', 'workflow_dispatch'):
                result = self.select([self.run_record(name, event=event)], name)
                self.assertEqual(result['event'], event)
        with self.assertRaises(ValueError):
            p.select_security_run(self.item('anvil'), {'total_count': 0, 'workflow_runs': []})
        self.assertEqual(p.collector.MANUAL_WORKFLOWS,
                         frozenset({('anvil', 'link-recovery.yml'), ('forge', 'flatpak.yml')}))

    def test_wrong_commit_branch_path_event_and_missing_runs_fail(self):
        changes = [{'head_sha': '0' * 40}, {'head_branch': 'feature'},
                   {'path': '.github/workflows/ci.yml'}, {'path': '.github/workflows/security.yml'},
                   {'event': 'pull_request'}, {'event': 'schedule'}, {'event': 'workflow_run'}]
        for change in changes:
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.select([self.run_record(**change)])
        with self.assertRaises(ValueError): self.select([])

    def test_pending_latest_failure_and_rerun_failure_never_fall_back(self):
        for changes in ({'status': 'queued', 'conclusion': None},
                        {'status': 'in_progress', 'conclusion': None},
                        {'status': 'completed', 'conclusion': 'failure'},
                        {'status': 'completed', 'conclusion': 'cancelled'},
                        {'status': 'completed', 'conclusion': 'skipped'}):
            for newer in ({'id': 11}, {'id': 10, 'run_attempt': 2}):
                with self.subTest(changes=changes, newer=newer), self.assertRaises(ValueError):
                    self.select([self.run_record(), self.run_record(**newer, **changes)])

    def test_latest_success_selected_across_allowed_events(self):
        result = self.select([self.run_record(conclusion='failure'),
                              self.run_record(id=11, event='workflow_dispatch')])
        self.assertEqual((result['id'], result['event']), (11, 'workflow_dispatch'))

    def test_pagination_and_invalid_run_identity_fail(self):
        with self.assertRaises(ValueError): self.select([self.run_record()], count=101)
        for changes in ({'id': '10'}, {'id': 0}, {'run_attempt': '2'}, {'run_attempt': 0}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.select([self.run_record(**changes)])

    def test_two_fixed_public_calls_and_receipt_binding(self):
        def fetch(full_name, suffix):
            name = full_name.split('/')[1]
            self.assertIn(name, ('forge', 'frost'))
            self.assertEqual(suffix, 'actions/runs?head_sha=' + self.item(name)['revision'] + '&per_page=100')
            return {'total_count': 1, 'workflow_runs': [self.run_record(name, event='workflow_dispatch')]}
        with patch.object(p.collector, 'fetch', side_effect=fetch) as mocked:
            receipts = p.security_receipts(candidates())
        self.assertEqual(mocked.call_count, 2)
        self.assertEqual({record['repository'] for record in receipts}, {'beamiter/forge', 'beamiter/frost'})
        self.assertTrue(all(record['candidate'] == record['required_run']['head_sha'] for record in receipts))

    def test_failed_before_gate_does_not_write_success_receipt(self):
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, ENV):
            root = Path(temp); (root / 'evidence').mkdir(); values = candidates()
            p.write(root / 'frozen-candidates.json', values)
            with patch.object(p.collector, 'fetch', return_value={'total_count': 0, 'workflow_runs': []}):
                with self.assertRaises(ValueError): p.security_before(root, p.collector.digest_set(values))
            self.assertFalse((root / 'evidence/native-security-before.json').exists())


class GeneratedConfigTests(unittest.TestCase):
    def test_frost_only_inspector_uses_supported_full_hash_width(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp); (root / 'evidence').mkdir()
            for app in p.APPS:
                binary = root / 'installed' / app / '.local/bin' / app
                binary.parent.mkdir(parents=True); binary.write_bytes(b'pure-test-not-executable')
            host = root / 'host'; host.write_bytes(b'pure-test-not-executable')
            state = root / 'state'
            p.write(root / 'evidence/native-seed.json', {'mission': {'id': 'pure-config-test'}})
            env = {key: str(root / key.lower()) for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME',
                   'XDG_STATE_HOME', 'XDG_CACHE_HOME', 'XDG_RUNTIME_DIR')}
            env.update(DISPLAY=':97', DBUS_SESSION_BUS_ADDRESS='unix:path=/tmp/test-only',
                       JTERM_WORKSPACE_STATE_DIR=str(state))
            with patch.dict(os.environ, env):
                p.reader_manifest(root, state, host)
            for app in p.APPS:
                config = tomllib.loads((Path(env['XDG_CONFIG_HOME']) / app / 'config.toml').read_text())
                if app == 'frost': self.assertEqual(config['inspector_width'], 520.0)
                else: self.assertNotIn('inspector_width', config)


class WorkflowSourceTests(unittest.TestCase):
    def test_manual_readonly_pinned_actions(self):
        text = WORKFLOW.read_text()
        self.assertIn('  workflow_dispatch:', text)
        self.assertNotRegex(text, r'(?m)^  (push|pull_request|schedule|workflow_run):')
        self.assertIn('permissions:\n  contents: read\n', text)
        self.assertEqual(text.count('persist-credentials: false'), 3)
        actions = re.findall(r'uses: (\S+)', text)
        self.assertGreaterEqual(len(actions), 6)
        self.assertTrue(all(re.fullmatch(r'[A-Za-z0-9_/-]+@[0-9a-f]{40}', action) for action in actions))
        self.assertNotIn('secrets.', text)
        self.assertNotRegex(text, r'(?m)^\s+(github-token|run-id|repository):')

    def test_all_five_same_run_components_and_data_only_inputs(self):
        text = WORKFLOW.read_text()
        for name in p.BINARIES:
            self.assertIn('component: ' + name, text)
        self.assertEqual(text.count('toolchain: 1.86.0'), 1)
        self.assertEqual(text.count('toolchain: 1.99.1'), 4)
        self.assertEqual(p.TOOLCHAINS['jterm_core'], '1.86.0')
        self.assertTrue(all(p.TOOLCHAINS[app] == '1.99.1' for app in p.APPS))
        self.assertEqual(text.count('${{ inputs.'), 2)
        self.assertIn('pattern: native-build-*-${{ github.run_id }}-${{ github.run_attempt }}', text)
        self.assertIn('native-compile-evidence-', text)
        self.assertIn('needs: [preflight, build]', text)
        self.assertEqual(text.count('collect.py prepare'), 2)
        self.assertEqual(text.count('provenance.py security-before'), 2)
        self.assertNotIn('collect.py finish', text)
        self.assertNotIn('continue-on-error', text)

    def test_production_locked_builds_and_explicit_toolchain(self):
        text = (HERE / 'build.sh').read_text()
        for command in ('cargo +1.86.0 build --release --locked --bin jterm-sessiond',
                        'cargo +1.99.1 build --release --locked',
                        'cargo +1.99.1 build --release --all-features --locked',
                        'cargo +1.99.1 build -p ember --release --all-features --locked',
                        'cargo +1.99.1 build --release --locked --bin frost'):
            self.assertIn(command, text)
        self.assertNotIn('rustup default', text)
        for gate in ('fmt --manifest-path', 'clippy --locked --manifest-path', 'test --locked --manifest-path'):
            self.assertIn('cargo +1.86.0 ' + gate, text)
        self.assertIn('--bin mission-native-state -- -D warnings', text)
        self.assertIn('metadata --locked --format-version 1 --filter-platform', text)
        self.assertIn('dependency-msrv.json', text)
        self.assertIn('Acquire::Retries=2', text)
        self.assertIn('Acquire::https::Timeout=30', text)
        self.assertIn('Dir::Etc::sourceparts=-', text)
        self.assertIn('https://archive.ubuntu.com/ubuntu', text)
        self.assertIn('https://security.ubuntu.com/ubuntu', text)

    def test_runtime_isolated_and_distinct_final_gate(self):
        text = (HERE / 'run.sh').read_text()
        self.assertIn('exec env -i', text)
        self.assertIn('dbus-run-session', text)
        self.assertIn("props.Set('org.a11y.Status', 'ScreenReaderEnabled'", text)
        self.assertIn('"$host_bin" --jterm-session-host', text)
        self.assertIn('"$helper" seed', text)
        self.assertIn('"$helper" check', text)
        self.assertIn('cmp "$source/bin/$app" "$install_home/.local/bin/$app"', text)
        self.assertNotIn('five-repository-mission-acceptance "$root"', text)
        final = (HERE / 'finish.py').read_text()
        self.assertIn('p.collector.source_preflight(root, candidates)', final)
        self.assertIn('p.collector.receipts(candidates)', final)
        self.assertIn('security = p.security_receipts(candidates)', final)
        self.assertIn("'native-security-after.json'", final)
        self.assertNotIn('p.collector.finish(', final)
        self.assertIn("'provider_execution': 'skipped_by_user'", final)
        self.assertIn("'applications': reader['applications']", final)
        self.assertIn("'reader_not_covered': reader.get('not_covered', [])", final)


if __name__ == '__main__':
    unittest.main()
