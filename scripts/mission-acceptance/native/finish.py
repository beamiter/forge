#!/usr/bin/env python3
"""Final native-scope gate, importing unchanged legacy source/head/CI checks.

Never calls legacy collect.finish or manufactures host-acceptance-result.json:
its four protocol-client contract is deliberately distinct from four native UIs.
"""
import json
from pathlib import Path
import sys
import subprocess

import provenance as p
import reader as native_reader


def validate_launches(app, coverage, host):
    stages = coverage['stages']
    identities = []
    for cycle in (1, 2):
        p.require(stages.get('normal_close_' + str(cycle)) is True,
                  'Missing normal application close: ' + app + ' cycle ' + str(cycle))
        launch = stages.get('launch_' + str(cycle))
        p.require(isinstance(launch, dict) and set(launch) == {'pid', 'identity'},
                  'Missing native launch identity: ' + app)
        process = launch['identity']
        p.require(isinstance(process, dict) and set(process) == {'pid', 'start_ticks', 'executable', 'state_dir'},
                  'Malformed native process identity: ' + app)
        p.require(type(launch['pid']) is int and launch['pid'] > 1 and
                  type(process['pid']) is int and process['pid'] == launch['pid'],
                  'Native launch PID mismatch: ' + app)
        ticks = process['start_ticks']
        p.require(isinstance(ticks, str) and ticks.isascii() and ticks.isdigit(),
                  'Invalid native process birth ticks: ' + app)
        executable = process['executable']
        p.require(isinstance(executable, str) and '\x00' not in executable and Path(executable).is_absolute(),
                  'Invalid native executable identity: ' + app)
        p.require(process['state_dir'] == host['state_dir'], 'Native launch used another shared state: ' + app)
        identities.append(process)
    p.require(identities[0]['executable'] == identities[1]['executable'], 'App executable changed on reopen: ' + app)
    p.require((identities[0]['pid'], identities[0]['start_ticks']) !=
              (identities[1]['pid'], identities[1]['start_ticks']), 'Application was not relaunched: ' + app)


def validate_results(seed, reader, state):
    p.require(seed.get('schema') == 1, 'Missing valid seed receipt')
    mission = seed.get('mission')
    host = seed.get('host')
    p.require(isinstance(mission, dict) and mission.get('id') and isinstance(mission.get('revision'), int),
              'Missing seeded Mission identity')
    p.require(isinstance(host, dict) and set(host) == {'pid', 'start_ticks', 'executable', 'state_dir'},
              'Missing production host identity')
    p.require(state.get('schema') == 1 and state.get('status') == 'passed', 'Host-state check did not pass')
    p.require(state.get('scope') == 'host_state_check_only_not_native_UI', 'Wrong host result scope')
    p.require(state.get('core') == seed.get('core') == p.collector.CORE, 'Host core identity mismatch')
    p.require(state.get('host') == host and state.get('mission') == mission, 'Production host or full plan changed')
    for key in ('full_plan_unchanged', 'native_catalog_empty', 'conversation_catalog_empty'):
        p.require(state.get(key) is True, 'Missing host invariant: ' + key)
    p.require(state.get('provider_execution') == 'skipped_by_user' and
              state.get('hosted_task_validation') == 'not_run' and
              state.get('five_repository_delivery') == 'not_completed', 'Host result exceeds approved scope')
    p.require(reader.get('schema') == 1 and reader.get('status') == 'passed', 'Native readers did not pass')
    p.require(reader.get('scope') == 'actual_native_existing_mission_read_only', 'Wrong native observation scope')
    applications = reader.get('applications')
    p.require(isinstance(applications, dict) and set(applications) == set(p.APPS), 'Exactly four native results required')
    p.require(all(isinstance(applications[name], dict) and applications[name].get('status') == 'passed'
                  for name in p.APPS), 'Missing, failed or unexecuted native application')
    # Recheck the reader's pure mandatory-observation contract, rather than
    # trusting a status string in a malformed or regression-truncated receipt.
    # This does not replay the UI or turn stored assertions into new evidence.
    for app in p.APPS:
        coverage = applications[app].get('coverage')
        p.require(isinstance(coverage, dict), 'Missing native coverage: ' + app)
        try:
            missing = native_reader.missing_coverage(app, mission, coverage)
        except (KeyError, TypeError, AttributeError) as error:
            raise ValueError('Malformed native coverage: ' + app) from error
        p.require(not missing, 'Incomplete native coverage: ' + app + ': ' + ', '.join(missing))
        validate_launches(app, coverage, host)
    p.require(reader.get('mission_id') == mission['id'] and reader.get('mission_revision') == mission['revision'],
              'Native observation is for another Mission/revision')
    p.require(reader.get('host') == host, 'Native readers observed another production host')
    p.require(reader.get('provider_execution') == 'skipped_by_user' and reader.get('full_delivery') == 'not_claimed',
              'Native reader scope exceeds approval')
    return mission, host


def finish(root, expected, artifacts):
    # These are the SAME unchanged gates as legacy prepare/finish, not relaxed
    # approximations. Perform the source/public checks even after reader failure.
    candidates = p.manifest(root, expected)
    p.collector.source_preflight(root, candidates)
    after = p.collector.receipts(candidates)
    p.write(root / 'evidence' / 'native-ci-after.json', after)
    security = p.security_receipts(candidates)
    p.write(root / 'evidence' / 'native-security-after.json', security)
    p.verify(root, expected, artifacts)
    evidence = root / 'evidence'
    seed = json.loads((evidence / 'native-seed.json').read_text())
    reader = json.loads((evidence / 'native-reader' / 'result.json').read_text())
    state = json.loads((evidence / 'native-state-check.json').read_text())
    mission, host = validate_results(seed, reader, state)
    reader_input = json.loads((evidence / 'native-reader-input.json').read_text())
    p.require(reader.get('plan_sha256') == p.digest(evidence / 'native-frozen-plan.json') and
              reader.get('manifest_sha256') == p.digest(evidence / 'native-reader-input.json'),
              'Reader input digest mismatch')
    builds = json.loads((evidence / 'native-build-receipts.json').read_text())
    for app in p.APPS:
        binary = root / 'installed' / app / '.local' / 'bin' / app
        expected_binary = builds[app]['files_sha256']['bin/' + app]
        p.require(reader_input['applications'][app]['binary'] == str(binary),
                  'Reader manifest is not bound to the installed binary: ' + app)
        for cycle in (1, 2):
            process = reader['applications'][app]['coverage']['stages']['launch_' + str(cycle)]['identity']
            p.require(process['executable'] == str(binary), 'Native launch executable differs from installed binary: ' + app)
        p.require(p.digest(binary) == expected_binary == reader_input['applications'][app]['sha256'] ==
                  reader['applications'][app].get('binary_sha256'), 'Installed/observed binary changed: ' + app)
    for binary in ('jterm-sessiond', 'mission-native-state'):
        p.require(p.digest(root / 'installed' / 'core' / 'bin' / binary) ==
                  builds['jterm_core']['files_sha256']['bin/' + binary], 'Installed core/helper changed')
    p.write(evidence / 'native-final-receipt.json', {
        'schema': 1, 'contract': 'four-native-shared-mission-readers-v1', 'status': 'passed',
        'finished_at': p.collector.now(), 'candidate_set_sha256': expected,
        'workflow': p.identity(), 'mission_id': mission['id'], 'mission_revision': mission['revision'],
        'host': host, 'applications': reader['applications'],
        'reader_limitations': reader.get('limitations', []),
        'reader_not_covered': reader.get('not_covered', []),
        'source_head_ci_recheck': 'passed', 'required_workflow_count': p.NATIVE_REQUIRED_WORKFLOW_COUNT,
        'extra_security_workflows': security, 'full_plan_unchanged': True,
        'provider_execution': 'skipped_by_user', 'hosted_task_validation': 'not_run',
        'five_repository_delivery': 'not_completed', 'full_delivery': 'not_claimed'})


def main():
    root, expected, artifacts = sys.argv[1:]
    root = Path(root).resolve()
    output = root / 'evidence' / 'native-final-receipt.json'
    p.require(not output.exists(), 'Refusing to overwrite/reuse a final receipt')
    try:
        finish(root, expected, Path(artifacts).resolve())
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        p.write(output, {'schema': 1, 'contract': 'four-native-shared-mission-readers-v1',
                         'status': 'failed', 'error': str(error), 'provider_execution': 'skipped_by_user',
                         'hosted_task_validation': 'not_run', 'five_repository_delivery': 'not_completed'})
        raise


if __name__ == '__main__':
    main()
