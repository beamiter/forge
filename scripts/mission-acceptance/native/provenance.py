#!/usr/bin/env python3
"""Strict same-workflow source and native artifact provenance, never UI evidence."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('native_collector', HERE.parent / 'collect.py')
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)
APPS = ('anvil', 'forge', 'ember', 'frost')
TOOLCHAINS = {'jterm_core': '1.86.0', **{app: '1.99.1' for app in APPS}}
IDENTITY = ('GITHUB_REPOSITORY', 'GITHUB_SHA', 'GITHUB_REF', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT')
BINARIES = {name: (name,) for name in APPS}
BINARIES['jterm_core'] = ('jterm-sessiond', 'mission-native-state')
# Additional path-filtered security workflows. Manual dispatch is accepted ONLY
# for these fixed native additions; legacy collect.WORKFLOWS/event rules remain
# untouched. These bring the native before/after requirement from 20 to 22.
EXTRA_SECURITY_WORKFLOWS = {'forge': 'security-audit.yml', 'frost': 'security.yml'}
NATIVE_REQUIRED_WORKFLOW_COUNT = sum(map(len, collector.WORKFLOWS.values())) + len(EXTRA_SECURITY_WORKFLOWS)



def require(ok, message):
    if not ok:
        raise ValueError(message)


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + '\n')


def identity():
    value = {key: os.environ.get(key) for key in IDENTITY}
    require(value['GITHUB_REPOSITORY'] == 'beamiter/forge', 'Unexpected workflow repository')
    require(value['GITHUB_REF'] == 'refs/heads/master', 'Workflow must use master')
    require(bool(re.fullmatch('[0-9a-f]{40}', value['GITHUB_SHA'] or '')), 'Invalid workflow commit')
    for key in ('GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT'):
        require(bool(re.fullmatch('[1-9][0-9]*', value[key] or '')), 'Invalid run identity')
    return value


def manifest(root, expected):
    value = json.loads((root / 'frozen-candidates.json').read_text())
    collector.validate_manifest(value, expected)
    current = identity()
    require(next(r['revision'] for r in value['repositories'] if r['name'] == 'forge') == current['GITHUB_SHA'],
            'Workflow implementation must be the frozen public Forge master')
    return value


def select_security_run(item, response):
    name = item.get('name')
    require(name in EXTRA_SECURITY_WORKFLOWS and item.get('full_name') == 'beamiter/' + name,
            'Security workflow outside the two fixed native additions')
    revision = item.get('revision', '')
    require(bool(re.fullmatch('[0-9a-f]{40}', revision)), 'Security gate requires an exact commit')
    require(type(response.get('total_count')) is int and 0 <= response['total_count'] <= 100,
            'Security workflow pagination exceeds bounded receipt collector')
    runs = response.get('workflow_runs')
    require(isinstance(runs, list), 'Missing security workflow runs')
    path = '.github/workflows/' + EXTRA_SECURITY_WORKFLOWS[name]
    matching = [run for run in runs if isinstance(run, dict)
                and run.get('event') in ('push', 'workflow_dispatch')
                and run.get('head_branch') == 'master'
                and run.get('head_sha') == revision and run.get('path') == path]
    require(bool(matching), 'Missing exact-head master security workflow: ' + item['full_name'] + ' ' + path)
    for run in matching:
        require(type(run.get('id')) is int and run['id'] > 0 and
                type(run.get('run_attempt', 1)) is int and run.get('run_attempt', 1) > 0,
                'Invalid security run identity')
    latest = max(matching, key=lambda run: (run['id'], run.get('run_attempt', 1)))
    require(latest.get('status') == 'completed' and latest.get('conclusion') == 'success',
            'Latest exact-head security workflow not successful: ' + item['full_name'] + ' ' + path)
    return {key: latest.get(key) for key in ('id', 'name', 'path', 'head_sha', 'head_branch', 'event',
                                           'run_attempt', 'status', 'conclusion', 'html_url', 'created_at', 'updated_at')}


def security_receipts(candidates):
    require(NATIVE_REQUIRED_WORKFLOW_COUNT == 22, 'Native workflow requirements changed; review all gates')
    by_name = {item['name']: item for item in candidates['repositories']}
    result = []
    for name in EXTRA_SECURITY_WORKFLOWS:
        item = by_name[name]
        response = collector.fetch(item['full_name'], f"actions/runs?head_sha={item['revision']}&per_page=100")
        result.append({'repository': item['full_name'], 'candidate': item['revision'],
                       'observed_at': collector.now(), 'required_run': select_security_run(item, response)})
    return result


def security_before(root, expected):
    candidates = manifest(root, expected)
    result = security_receipts(candidates)
    write(root / 'evidence' / 'native-security-before.json', result)


def clone(root, expected, input_path):
    """Matrix jobs depend on prepare's completed exact-head public CI gate.

    Do not spend unauthenticated API rate budget repeating it five times. Each
    matrix clone is exact/clean/source-audited; prepare and finish recheck public
    heads and ALL required CI before and after native execution.
    """
    data = input_path.read_bytes()
    require(len(data) <= 64 * 1024, 'Candidate input exceeds budget')
    candidates = json.loads(data)
    collector.validate_manifest(candidates, expected)
    current = identity()
    require(next(r['revision'] for r in candidates['repositories'] if r['name'] == 'forge') == current['GITHUB_SHA'],
            'Build workflow differs from frozen Forge')
    root.mkdir(parents=True, exist_ok=False)
    (root / 'evidence').mkdir()
    write(root / 'frozen-candidates.json', candidates)
    observed = {'schema': 1, 'candidate_set_sha256': expected, 'repositories': []}
    for item in candidates['repositories']:
        repo = root / 'repositories' / item['name']
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        subprocess.run(['git', '-C', str(repo), 'remote', 'add', 'origin',
                        'https://github.com/' + item['full_name'] + '.git'], check=True)
        subprocess.run(['git', '-C', str(repo), 'fetch', '--depth=1', 'origin', item['revision']], check=True)
        subprocess.run(['git', '-C', str(repo), 'checkout', '--detach', 'FETCH_HEAD'], check=True)
        observed['repositories'].append({**item, 'path': str(repo), 'target_ref': 'refs/heads/master'})
    write(root / 'observed-heads.json', observed)
    write(root / 'evidence' / 'workflow-identity.json', current)
    collector.source_preflight(root, candidates)


def record(root, expected, name, output):
    candidates = manifest(root, expected)
    require(name in BINARIES, 'Unexpected matrix component')
    collector.source_preflight(root, candidates)
    source = root / 'repositories' / name
    files = {}
    for path in sorted(output.rglob('*')):
        require(not path.is_symlink(), 'Symlink in artifact')
        if path.is_file():
            files[path.relative_to(output).as_posix()] = digest(path)
    require(set('bin/' + name for name in BINARIES[name]).issubset(files), 'Missing production binary')
    for binary in BINARIES[name]:
        with (output / 'bin' / binary).open('rb') as stream:
            require(stream.read(4) == b'\x7fELF', 'Production executable is not an ELF binary')
    receipt = {'schema': 1, 'component': name, 'candidate_set_sha256': expected,
               'workflow': identity(), 'source': next(r for r in candidates['repositories'] if r['name'] == name),
               'cargo_lock_sha256': digest(source / 'Cargo.lock'), 'rust_toolchain': TOOLCHAINS[name],
               'files_sha256': files, 'provider_execution': 'not_run', 'native_execution': 'not_run'}
    write(output / 'build-receipt.json', receipt)


def validate_receipt(root, expected, name, artifact):
    candidates = manifest(root, expected)
    require(name in BINARIES, 'Unexpected component')
    require(artifact.is_dir() and not artifact.is_symlink(), 'Missing artifact directory')
    receipt = json.loads((artifact / 'build-receipt.json').read_text())
    require(receipt.get('schema') == 1 and receipt.get('component') == name, 'Wrong artifact component/schema')
    require(receipt.get('candidate_set_sha256') == expected, 'Wrong artifact frozen set')
    require(receipt.get('workflow') == identity(), 'Artifact is not from this exact workflow run and attempt')
    require(receipt.get('source') == next(r for r in candidates['repositories'] if r['name'] == name),
            'Artifact source revision mismatch')
    require(receipt.get('cargo_lock_sha256') == digest(root / 'repositories' / name / 'Cargo.lock'),
            'Artifact Cargo.lock mismatch')
    require(receipt.get('rust_toolchain') == TOOLCHAINS[name], 'Unexpected build toolchain')
    require(receipt.get('provider_execution') == 'not_run' and receipt.get('native_execution') == 'not_run',
            'Unexpected build scope')
    actual = {}
    for path in artifact.rglob('*'):
        require(not path.is_symlink(), 'Artifact symlink rejected')
        if path.is_file() and path.relative_to(artifact).as_posix() != 'build-receipt.json':
            actual[path.relative_to(artifact).as_posix()] = digest(path)
    require(actual == receipt.get('files_sha256'), 'Artifact file set or SHA256 mismatch')
    require(set('bin/' + binary for binary in BINARIES[name]).issubset(actual), 'Missing binary')
    for binary in BINARIES[name]:
        with (artifact / 'bin' / binary).open('rb') as stream:
            require(stream.read(4) == b'\x7fELF', 'Non-ELF production binary')
    return receipt


def verify(root, expected, artifacts):
    manifest(root, expected)
    expected_dirs = {'native-build-' + name + '-' + os.environ['GITHUB_RUN_ID'] + '-' + os.environ['GITHUB_RUN_ATTEMPT']
                     for name in BINARIES}
    require({p.name for p in artifacts.iterdir()} == expected_dirs, 'Missing/extra same-run build artifacts')
    result = {}
    for name in BINARIES:
        artifact = artifacts / ('native-build-' + name + '-' + os.environ['GITHUB_RUN_ID'] + '-' + os.environ['GITHUB_RUN_ATTEMPT'])
        result[name] = validate_receipt(root, expected, name, artifact)
    write(root / 'evidence' / 'native-build-receipts.json', result)


def extract_bundle(artifact, destination, name):
    require(name in ('anvil', 'forge'), 'Unexpected bundle application')
    archives = list((artifact / 'dist').glob('*.tar.gz'))
    require(len(archives) == 1, 'Expected exactly one release archive')
    with tarfile.open(archives[0], 'r:gz') as archive:
        members = archive.getmembers()
        roots = set()
        for item in members:
            path = Path(item.name)
            require(not path.is_absolute() and '..' not in path.parts and len(path.parts) > 0,
                    'Unsafe release archive path')
            require(item.isdir() or item.isfile(), 'Release archive links/devices are not allowed')
            roots.add(path.parts[0])
        require(len(roots) == 1 and next(iter(roots)).startswith(name + '-'), 'Unexpected bundle root')
        destination.mkdir(parents=True, exist_ok=False)
        archive.extractall(destination, filter='data')
    return destination / next(iter(roots))


def metadata_receipt(metadata_file, output, component, helper=False):
    require(component in TOOLCHAINS, 'Unknown MSRV component')
    toolchain = TOOLCHAINS[component]
    data = json.loads(metadata_file.read_text())
    resolve = data['resolve']
    nodes = {node['id']: node for node in resolve['nodes']}
    pending = [resolve['root']]
    active = set()
    while pending:
        node = pending.pop()
        if node in active:
            continue
        require(node in nodes, 'Missing resolved dependency node')
        active.add(node)
        # Build bins, not dev/test targets. Cargo's --filter-platform excludes
        # other platforms. Only normal/build dependency edges apply here.
        for edge in nodes[node]['deps']:
            if any(kind['kind'] in (None, 'build') for kind in edge['dep_kinds']):
                pending.append(edge['pkg'])
    packages = [package for package in data['packages'] if package['id'] in active]
    require(len(packages) == len(active), 'Resolved package metadata missing')
    cores = [package for package in packages if package['name'] == 'jterm_core']
    require(len(cores) == 1, 'Exactly one compiled core source required')
    source = None if component == 'jterm_core' and not helper else (
        'git+https://github.com/beamiter/jterm_core.git?rev=' + collector.CORE + '#' + collector.CORE)
    require(cores[0]['source'] == source, 'Resolved core differs from frozen production source')
    declared = []
    chosen = tuple(int(n) for n in toolchain.split('.'))
    for package in packages:
        version = package.get('rust_version')
        if version is not None:
            require(bool(re.fullmatch(r'[0-9]+\.[0-9]+(?:\.[0-9]+)?', version)), 'Unknown dependency rust-version')
            parts = tuple(int(n) for n in version.split('.'))
            required = parts + (0,) * (3 - len(parts))
            require(required <= chosen, 'Dependency MSRV exceeds explicit toolchain: ' + package['name'])
        declared.append({'name': package['name'], 'version': package['version'], 'source': package['source'],
                         'rust_version': version})
    write(output, {'schema': 1, 'toolchain': toolchain, 'target': 'x86_64-unknown-linux-gnu',
                   'metadata_sha256': digest(metadata_file), 'component': component, 'helper': helper,
                   'scope': 'declared MSRVs of resolved normal/build dependencies; not a minimum-toolchain claim',
                   'packages': sorted(declared, key=lambda package: (package['name'], package['version']))})


def reader_manifest(root, state, host_bin):
    applications = {}
    for app in APPS:
        binary = root / 'installed' / app / '.local' / 'bin' / app
        config = Path(os.environ['XDG_CONFIG_HOME']) / app / 'config.toml'
        config.parent.mkdir(parents=True, exist_ok=True)
        text = 'shell="/bin/bash"\nterminal_mode="block"\nai_enabled=false\neva_companion_enabled=false\nagent_workspace_enabled=true\n'
        if app in ('ember', 'frost'):
            text += 'restore_session=false\ninitial_width=1200\ninitial_height=850\n'
        else:
            text += 'jsh_update_check="never"\n'
        if app == 'frost':
            text += 'inspector_width=520.0\n'
        config.write_text(text)
        applications[app] = {'binary': str(binary), 'sha256': digest(binary), 'config': str(config)}
    keys = ('HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_CACHE_HOME',
            'XDG_RUNTIME_DIR', 'DISPLAY', 'DBUS_SESSION_BUS_ADDRESS')
    value = {'schema_version': 1, 'applications': applications,
             'environment': {key: os.environ[key] for key in keys},
             'host': {'binary': str(host_bin), 'sha256': digest(host_bin)}}
    require(os.environ['JTERM_WORKSPACE_STATE_DIR'] == str(state), 'Shared state environment mismatch')
    write(root / 'evidence' / 'native-reader-input.json', value)
    receipt = json.loads((root / 'evidence' / 'native-seed.json').read_text())
    write(root / 'evidence' / 'native-frozen-plan.json', receipt['mission'])


def main():
    command, *args = sys.argv[1:]
    if command == 'toolchain':
        require(args[0] in TOOLCHAINS, 'Unknown toolchain component')
        print(TOOLCHAINS[args[0]])
    elif command == 'metadata':
        metadata_receipt(Path(args[0]), Path(args[1]), args[2], len(args) == 4 and args[3] == 'helper')
    elif command == 'security-before':
        security_before(Path(args[0]).resolve(), args[1])
    elif command == 'clone':
        clone(Path(args[0]).resolve(), args[1], Path(args[2]))
    elif command == 'record':
        record(Path(args[0]).resolve(), args[1], args[2], Path(args[3]).resolve())
    elif command == 'verify':
        verify(Path(args[0]).resolve(), args[1], Path(args[2]).resolve())
    elif command == 'extract':
        print(extract_bundle(Path(args[0]), Path(args[1]), args[2]))
    elif command == 'reader-manifest':
        reader_manifest(Path(args[0]), Path(args[1]), Path(args[2]))
    else:
        raise ValueError('Unknown provenance operation')


if __name__ == '__main__':
    main()
