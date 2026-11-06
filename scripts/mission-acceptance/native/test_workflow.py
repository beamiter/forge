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
