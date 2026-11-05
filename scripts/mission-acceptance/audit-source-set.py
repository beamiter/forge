#!/usr/bin/env python3
"""Read-only source-set audit. This is not native or hosted validation evidence."""
import datetime
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tomllib
NAMES = ('jterm_core', 'anvil', 'forge', 'ember', 'frost')
DEPENDENCY_KINDS = ('dependencies', 'dev-dependencies', 'build-dependencies')
CORE_URLS = ('https://github.com/beamiter/jterm_core', 'https://github.com/beamiter/jterm_core.git')

def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()

def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

def reject_overrides(data, name):
    for source in data.get('patch', {}).values():
        for alias, dependency in source.items():
            if alias == 'jterm_core' or (isinstance(dependency, dict) and dependency.get('package') == 'jterm_core'):
                raise ValueError(f'{name}: core patch')
    if data.get('replace') or data.get('paths'):
        raise ValueError(f'{name}: replace or paths override')
    if any(('replace-with' in value for value in data.get('source', {}).values())):
        raise ValueError(f'{name}: source replacement')

def checked_bundle_path(root, relative):
    item = Path(relative)
    if item.is_absolute() or '..' in item.parts:
        raise ValueError('Bundle path escapes repository')
    result = root / item
    if result.is_symlink() or not result.resolve().is_relative_to(root.resolve()):
        raise ValueError('Bundle path follows an external target')
    return result

def dependency_tables(document, location='root'):
    """Walk Cargo's root, workspace and target tables, not package metadata."""
    for kind in DEPENDENCY_KINDS:
        if kind in document:
            table = document[kind]
            if not isinstance(table, dict):
                raise ValueError(f'{location}.{kind}: expected a dependency table')
            yield f'{location}.{kind}', table
    if 'workspace' in document:
        workspace = document['workspace']
        if not isinstance(workspace, dict):
            raise ValueError(f'{location}.workspace: expected a table')
        yield from dependency_tables(workspace, f'{location}.workspace')
    if 'target' in document:
        target = document['target']
        if not isinstance(target, dict):
            raise ValueError(f'{location}.target: expected a table')
        for selector, table in target.items():
            if not isinstance(table, dict):
                raise ValueError(f'{location}.target.{selector}: expected a table')
            yield from dependency_tables(table, f'{location}.target.{selector}')

def manifest_pins(manifest, core, require_core=True):
    pins = []
    for location, table in dependency_tables(manifest):
        for alias, dependency in table.items():
            package = dependency.get('package') if isinstance(dependency, dict) else None
            source = dependency.get('git') if isinstance(dependency, dict) else None
            if alias != 'jterm_core' and package != 'jterm_core' and source not in CORE_URLS:
                continue
            if alias != 'jterm_core' or package not in (None, 'jterm_core'):
                raise ValueError(f'{location}.{alias}: core aliases are not allowed')
            if not isinstance(dependency, dict) or dependency.get('git') != CORE_URLS[1]:
                raise ValueError(f'{location}.{alias}: explicit canonical core Git source required')
            if dependency.get('rev') != core or any(key in dependency for key in ('path', 'workspace', 'branch', 'tag')):
                raise ValueError(f'{location}.{alias}: core source pin mismatch or inheritance/override')
            pins.append({'section':location, 'revision':dependency['rev']})
    if require_core and not any(pin['section'] == 'root.dependencies' for pin in pins):
        raise ValueError('No explicit production root core dependency')
    return pins

def inspect(repository, core):
    path = Path(repository['path'])
    revision = git(path, 'rev-parse', 'HEAD')
    if revision != repository['revision']:
        raise ValueError('Snapshot changed')
    if git(path, 'status', '--porcelain=v1', '--untracked-files=all'):
        raise ValueError('Dirty candidate')
    manifest = tomllib.loads((path / 'Cargo.toml').read_text())
    reject_overrides(manifest, 'Cargo.toml')
    if (path / '.cargo').is_symlink():
        raise ValueError('Cargo configuration directory symlink')
    for name in ('.cargo/config', '.cargo/config.toml'):
        config = path / name
        if config.is_symlink():
            raise ValueError('Cargo configuration symlink')
        if config.is_file():
            reject_overrides(tomllib.loads(config.read_text()), name)
    pins = manifest_pins(manifest, core, require_core=repository['name'] != 'jterm_core')
    if repository['name'] != 'jterm_core':
        lock = tomllib.loads((path / 'Cargo.lock').read_text())
        cores = [p for p in lock['package'] if p['name'] == 'jterm_core']
        if len(cores) != 1:
            raise ValueError('Source contract mismatch')
        if cores[0].get('source') != f'git+https://github.com/beamiter/jterm_core.git?rev={core}#{core}':
            raise ValueError('Source contract mismatch')
    receipts = []
    for name in ('host-adapter-bundle', 'process-adapter-bundle'):
        receipt_path = path / f'vendor/{name}.json'
        if receipt_path.is_file():
            receipt = json.loads(receipt_path.read_text())
            if receipt.get('public_core_base') != core:
                raise ValueError(f'Stale {name} core receipt')
            for relative, expected in receipt['files_sha256'].items():
                if digest(checked_bundle_path(path, relative)) != expected:
                    raise ValueError(f'Changed bundle file {relative}')
            receipts.append({'path': str(receipt_path.relative_to(path)), 'sha256': digest(receipt_path), 'checked_files': len(receipt['files_sha256'])})
    return {'repository': repository['full_name'], 'candidate': revision, 'tree': git(path, 'rev-parse', 'HEAD^{tree}'), 'clean': True, 'manifest_sha256': digest(path / 'Cargo.toml'), 'lock_sha256': digest(path / 'Cargo.lock'), 'core_requirements': pins, 'adapter_receipts': receipts, 'source_contract': 'passed'}

def main():
    root = Path(sys.argv[1]).resolve()
    inputs = json.loads((root / 'observed-heads.json').read_text())
    core = next(r['revision'] for r in inputs['repositories'] if r['name'] == 'jterm_core')
    results = []
    for repository in inputs['repositories']:
        try:
            results.append(inspect(repository, core))
        except (ValueError, OSError, subprocess.SubprocessError) as error:
            results.append({'repository': repository['full_name'], 'source_contract': 'failed', 'error': str(error)})
    canonical = json.dumps([(r['full_name'], r['revision']) for r in sorted(inputs['repositories'], key=lambda r: NAMES.index(r['name']))], separators=(',', ':')).encode()
    report = {'observed_at': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'candidate_set_sha256': hashlib.sha256(canonical).hexdigest(), 'scope': 'Read-only exact Git snapshot, root Cargo manifest dependency tables, source pins and adapter files. No build, arbitrary workspace-member manifest audit, native runtime, CI import, task validation or delivery completion is claimed.', 'repositories': results}
    (root / 'evidence' / 'source-set-result.json').write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps(report, indent=2))
    raise SystemExit(any(r['source_contract'] != 'passed' for r in results))

if __name__ == '__main__':
    main()
