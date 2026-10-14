#!/usr/bin/env python3
"""Build and run the BEAM gate without changing individual Cargo test targets."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import time
import tomllib

REPO = Path(__file__).resolve().parents[2]


def read_json(path):
    return json.loads(path.read_text(encoding='utf-8'))


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n', encoding='utf-8', newline='\n')


def source_hash(path):
    # Git's checkout line endings differ across the supported runners.
    return hashlib.sha256(path.read_text(encoding='utf-8').encode()).hexdigest()


def byte_hash(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def freeze(repo):
    paths = subprocess.check_output(['git', 'ls-files', '-z'], cwd=repo).decode('utf-8').split('\0')
    paths += ['tools/testing/beam-gate.py', 'tools/testing/driver-groups.json']
    return {p: byte_hash(repo / p) for p in paths if p and (repo / p).is_file()}


def unchanged(repo, expected):
    if any(not (repo / p).is_file() or byte_hash(repo / p) != value for p, value in expected.items()):
        raise ValueError('Repository source changed during validation')


def section(raw, name):
    match = re.search(r'(?ms)^\[' + re.escape(name) + r'\]\s*\n(.*?)(?=^\[|\Z)', raw)
    if not match:
        raise ValueError('Missing manifest section: ' + name)
    return match[1]


def relocate(raw, base):
    return re.sub(r'path\s*=\s*"([^"]+)"',
                  lambda m: 'path = ' + json.dumps((base / m[1]).resolve().as_posix()), raw)


def partition(repo, policy):
    sources = {p.stem: p for p in (repo / 'crates/ridge-driver/tests').glob('*.rs')}
    grouped = [n for names in policy['groups'].values() for n in names]
    declared = grouped + policy['retained']
    if len(set(declared)) != len(declared) or set(declared) != set(sources):
        raise ValueError('Driver source partition changed; review driver-groups.json')
    if not grouped or not policy['retained']:
        raise ValueError('Empty driver partition')
    forbidden = (r'CARGO_MANIFEST_DIR|CARGO_BIN_EXE|\b(?:crate|super)::|macro_rules!|'
                 r'\b(?:set_var|remove_var|set_current_dir)\s*\(|RIDGE_TEST_PG_URL|'
                 r'(?m:^mod\s+common\s*;)|#\[path\s*=|\b(?:include|env|option_env|module_path)!')
    for name in grouped:
        if not re.fullmatch(r'[a-z][a-z0-9_]*', name):
            raise ValueError('Invalid fixture module name: ' + name)
        if re.search(forbidden, sources[name].read_text(encoding='utf-8')):
            raise ValueError('Grouped fixture needs original crate context: ' + name)
    return sources, grouped


def prepare(repo, work, policy):
    sources, grouped = partition(repo, policy)
    driver = repo / 'crates/ridge-driver'
    root_raw = (repo / 'Cargo.toml').read_text(encoding='utf-8')
    raw = (driver / 'Cargo.toml').read_text(encoding='utf-8')
    root, original = tomllib.loads(root_raw), tomllib.loads(raw)
    allowed = {'package', 'dependencies', 'dev-dependencies', 'features', 'lints'}
    if set(original) - allowed or set(original['features']) != {'beam-runtime'}:
        raise ValueError('Driver manifest topology changed; review generated workspace')
    if any(original['dependencies'].get(k) != v for k, v in original.get('dev-dependencies', {}).items()):
        raise ValueError('Driver acquired additional dev-dependencies')
    if root.get('patch') or root.get('replace'):
        raise ValueError('Workspace dependency overrides need a generator review')
    harness = work / 'harness'
    (harness / 'tests').mkdir(parents=True)
    manifest = '[package]\nname = "ridge-grouped-driver-gate"\npublish = false\nautotests = false\n'
    for key in ('version', 'edition', 'rust-version'):
        manifest += key + ' = ' + json.dumps(root['workspace']['package'][key]) + '\n'
    manifest += '[workspace]\nresolver = ' + json.dumps(root['workspace']['resolver']) + '\n'
    for name in ('workspace.dependencies', 'workspace.lints.rust', 'workspace.lints.clippy'):
        contents = section(root_raw, name)
        manifest += '[' + name + ']\n' + (relocate(contents, repo) if name.endswith('dependencies') else contents) + '\n'
    manifest += '[dependencies]\nridge-driver = { path = ' + json.dumps(driver.as_posix()) + ' }\n'
    manifest += relocate(section(raw, 'dependencies'), driver)
    # Propagate the original feature definition instead of mirroring its edges.
    manifest += '\n[features]\nbeam-runtime = ["ridge-driver/beam-runtime"]\n[lints]\nworkspace = true\n'
    for match in re.finditer(r'(?ms)^\[(profile\.[^\]]+)\]\s*\n(.*?)(?=^\[|\Z)', root_raw):
        manifest += '[' + match[1] + ']\n' + match[2] + '\n'
    for group, names in policy['groups'].items():
        if not re.fullmatch(r'[a-z][a-z0-9_]*', group):
            raise ValueError('Invalid group name')
        wrapper = ''.join('#[path = ' + json.dumps(sources[n].as_posix()) + ']\nmod ' + n + ';\n' for n in names)
        (harness / 'tests' / (group + '.rs')).write_text(wrapper, encoding='utf-8')
        manifest += '\n[[test]]\nname = ' + json.dumps(group) + '\npath = "tests/' + group + '.rs"\n'
    (harness / 'Cargo.toml').write_text(manifest, encoding='utf-8')
    for name in ('clippy.toml', '.clippy.toml', 'Cargo.lock'):
        if (repo / name).exists():
            shutil.copyfile(repo / name, harness / name)
    return harness, {n: source_hash(sources[n]) for n in sorted(grouped)}


def inventories(data, grouped=False):
    rows = []
    for suite in data['rust-suites'].values():
        if suite['status'] != 'listed':
            raise ValueError('Unlisted suite: ' + suite['binary-name'])
        for name, info in suite['testcases'].items():
            binary = suite['binary-name']
            if grouped:
                binary, name = name.split('::', 1)
            rows.append((binary, name, info['ignored']))
    if len(set((r[0], r[1]) for r in rows)) != len(rows):
        raise ValueError('Duplicate native test identity')
    return sorted(rows)


def native_listing(data, triple):
    targets = data['rust-build-meta']['target-platforms']
    if len(targets) != 1 or targets[0]['triple'] != triple:
        raise ValueError('Nextest listing target differs from catalog target')


def driver_targets(metadata, repo, policy):
    packages = [p for p in metadata['packages'] if p['name'] == 'ridge-driver']
    if len(packages) != 1:
        raise ValueError('Missing or ambiguous driver package')
    actual = {}
    for target in packages[0]['targets']:
        if target['kind'] == ['test']:
            if target.get('required-features') or not target.get('test', True):
                raise ValueError('Driver target feature/test policy changed')
            actual[target['name']] = Path(target['src_path']).resolve()
        elif target['kind'] not in (['lib'], ['custom-build']):
            raise ValueError('Unexpected driver target kind: ' + repr(target['kind']))
    expected = {n: (repo / 'crates/ridge-driver/tests' / (n + '.rs')).resolve()
                for n in policy['retained'] + [n for names in policy['groups'].values() for n in names]}
    if actual != expected:
        raise ValueError('Cargo driver targets differ from reviewed source partition')


def parity(actual, expected):
    if len(set(map(tuple, actual))) != len(actual) or sorted(map(tuple, actual)) != sorted(map(tuple, expected)):
        raise ValueError('Native case identity/ignored-state mismatch: ' + repr(set(map(tuple, actual)) ^ set(map(tuple, expected))))


def completion(body, rows, require_pg):
    body = re.sub(r'\x1b\[[0-9;]*m', '', body)
    summaries = re.findall(r'(?m)^\s*Summary\s+\[[^\]]+\]\s+(\d+) tests? run: (\d+) passed([^\n]*)', body)
    active = sum(not row[2] for row in rows)
    if len(summaries) != 1 or tuple(map(int, summaries[0][:2])) != (active, active) or not active:
        raise ValueError('Missing, filtered or incomplete nextest summary')
    guards = [line.strip() for line in body.splitlines() if re.search(r'(?:—| -) skipping\b', line)]
    if any(require_pg or 'RIDGE_TEST_PG_URL not set' not in line for line in guards):
        raise ValueError('Unavailable runtime/service: ' + repr(guards))
    return {'passed': active, 'ignored': len(rows) - active, 'service_guard_skips': guards}


def project_config(repo, work):
    raw = (repo / '.config/nextest.toml').read_text(encoding='utf-8')
    expected = {'test-groups': {'postgres': {'max-threads': 1}}, 'profile': {'default': {'overrides': [
        {'filter': 'binary(/^data_pg_/)', 'test-group': 'postgres'}]}}}
    if tomllib.loads(raw) != expected:
        raise ValueError('Nextest policy changed; review non-Postgres projection')
    path = work / 'nextest-non-postgres.toml'
    path.write_text(raw.split('[[profile.default.overrides]]')[0], encoding='utf-8')
    return path


def feature_graph(body):
    graph = {}
    for line in body.splitlines():
        if '|' not in line:
            raise ValueError('Unexpected cargo tree output: ' + line)
        package, features = line.removesuffix(' (*)').split('|', 1)
        if not package.startswith('ridge-grouped-driver-gate '):
            graph.setdefault(package, set()).update(filter(None, features.split(',')))
    return {k: sorted(v) for k, v in sorted(graph.items())}


def catalog_reference(catalog, contract, audit):
    compatible = not audit and all(catalog.get(k) == contract[k] for k in ('schema', 'target', 'features'))
    reused = {n for n, h in contract['sources'].items() if compatible and catalog.get('sources', {}).get(n) == h}
    needed = sorted(set(contract['sources']) - reused)
    rows = [tuple(row) for row in catalog.get('cases', []) if row[0] in reused]
    return needed, rows


class Commands:
    def __init__(self, repo, work, prefix):
        self.repo, self.work, self.prefix = repo, work, prefix
        self.steps = read_json(work / 'commands.json') if (work / 'commands.json').exists() else []

    def run(self, label, args):
        command = self.prefix + args
        print('+ ' + ' '.join(command), flush=True)
        start = time.monotonic()
        out_path, err_path = self.work / (label + '.stdout'), self.work / (label + '.stderr')
        with out_path.open('wb') as out, err_path.open('wb') as err:
            result = subprocess.run(command, cwd=self.repo, stdout=out, stderr=err)
        self.steps.append({'phase': label, 'command': args, 'exit': result.returncode, 'wall_seconds': time.monotonic() - start})
        write_json(self.work / 'commands.json', self.steps)
        print(err_path.read_text(encoding='utf-8', errors='replace')[-3000:], end='', flush=True)
        if result.returncode:
            raise subprocess.CalledProcessError(result.returncode, command)
        return out_path.read_text(encoding='utf-8')


def prefix_from(args):
    # Optional local scheduler; CI has its own runner isolation.
    prefix = read_json(args.command_prefix) if args.command_prefix else []
    if not isinstance(prefix, list) or any(not isinstance(arg, str) for arg in prefix):
        raise ValueError('Command prefix must be a JSON argv array')
    return prefix


def build(args):
    repo, work = REPO, args.work.resolve()
    work.mkdir(parents=True, exist_ok=False)
    frozen = freeze(repo)
    policy = read_json(repo / 'tools/testing/driver-groups.json')
    harness, hashes = prepare(repo, work, policy)
    non_pg = project_config(repo, work)
    cmd = Commands(repo, work, prefix_from(args))
    rust_info = cmd.run('rust-info', ['rustc', '-vV'])
    triple = re.search(r'(?m)^host: (.+)$', rust_info)[1]
    if triple not in {'x86_64-pc-windows-msvc', 'x86_64-unknown-linux-gnu', 'aarch64-apple-darwin', 'x86_64-apple-darwin'}:
        raise ValueError('Unsupported native target: ' + triple)
    # Do not move native builds to a different Cargo artifact directory merely
    # to name the host. A cross-build needs a separately reviewed catalog.
    configured_target = os.environ.get('CARGO_BUILD_TARGET')
    if configured_target and configured_target != triple:
        raise ValueError('Cross-target execution is unsupported: ' + configured_target)
    common = ['--features', 'beam-runtime', '--locked']
    metadata = json.loads(cmd.run('driver-targets', ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked']))
    driver_targets(metadata, repo, policy)
    selected = {
        'retained': ['-p', 'ridge-driver', '--lib'] + [x for n in policy['retained'] for x in ('--test', n)],
        'grouped': ['--manifest-path', str(harness / 'Cargo.toml'), '--target-dir', str(repo / 'target')] + [x for n in policy['groups'] for x in ('--test', n)],
        'other': ['-p', 'ridge-codegen-erl', '-p', 'ridge-cli'],
    }
    if args.inventory_only:
        selected = {'grouped': selected['grouped']}
    def registry(path):
        return {(p['name'], p['version'], p.get('source'), p.get('checksum')) for p in tomllib.loads(path.read_text(encoding='utf-8'))['package'] if p.get('source')}
    original_registry = registry(repo / 'Cargo.lock')
    # The generated package needs its own lock root; all registry versions must
    # still come from the reviewed repository lockfile.
    cmd.run('harness-lock', ['cargo', 'metadata', '--manifest-path', str(harness / 'Cargo.toml'), '--format-version', '1', '--filter-platform', triple])
    if not registry(harness / 'Cargo.lock') <= original_registry:
        raise ValueError('Generated dependency versions drifted from Cargo.lock')
    graphs = {}
    for name, selection in [('original', ['-p', 'ridge-driver']), ('grouped', ['--manifest-path', str(harness / 'Cargo.toml')])]:
        body = cmd.run(name + '-features', ['cargo', 'tree'] + selection + ['--features', 'beam-runtime', '--locked', '--target', triple, '--edges', 'normal,build,dev', '--prefix', 'none', '--format', '{p}|{f}'])
        graphs[name] = feature_graph(body)
    write_json(work / 'features.json', graphs)
    if graphs['original'] != graphs['grouped']:
        raise ValueError('Selected dependency feature graphs differ; inspect features.json')
    phase_rows = {}
    for label, selection in selected.items():
        config = repo / '.config/nextest.toml' if label == 'retained' else non_pg
        body = cmd.run(label + '-list', ['cargo', 'nextest', 'list'] + selection + common + ['--config-file', str(config), '--message-format', 'json'])
        listing = json.loads(body)
        native_listing(listing, triple)
        write_json(work / (label + '-inventory.json'), listing)
        phase_rows[label] = inventories(listing, label == 'grouped')
        if not phase_rows[label]:
            raise ValueError('Empty phase: ' + label)
    contract = {'schema': 1, 'target': triple, 'features': ['beam-runtime'], 'sources': hashes}
    catalog_path = repo / 'tools/testing/inventories' / (triple + '.json')
    catalog = read_json(catalog_path) if catalog_path.exists() else {}
    needed, reference = catalog_reference(catalog, contract, args.audit)
    reusable = not needed
    if needed:
        selection = ['-p', 'ridge-driver'] + [x for n in needed for x in ('--test', n)]
        body = cmd.run('original-grouped-list', ['cargo', 'nextest', 'list'] + selection + common + ['--config-file', str(non_pg), '--message-format', 'json'])
        listing = json.loads(body)
        native_listing(listing, triple)
        write_json(work / 'original-grouped-inventory.json', listing)
        reference += inventories(listing)
    parity(phase_rows['grouped'], reference)
    write_json(work / 'inventory-audit.json', {
        'original_sources': needed, 'reused_sources': sorted(set(hashes) - set(needed)),
        'base_catalog_sha256': byte_hash(catalog_path) if catalog_path.exists() else None,
    })
    if needed:
        write_json(work / 'inventory-candidate.json', contract | {'cases': sorted(reference)})
        print('Native parity verified. Review inventory-candidate.json before adding a reusable catalog.', flush=True)
    cmd.run('grouped-clippy', ['cargo', 'clippy', '--manifest-path', str(harness / 'Cargo.toml'), '--target-dir', str(repo / 'target'), '--all-targets'] + common + ['--', '-D', 'warnings'])
    unchanged(repo, frozen)
    write_json(work / 'plan.json', {
        'schema': 1, 'repo': str(repo), 'selected': selected, 'common': common,
        'rows': phase_rows, 'non_pg_config': str(non_pg), 'catalog_reused': reusable,
        'source_hashes': hashes, 'policy_hash': source_hash(repo / 'tools/testing/driver-groups.json'),
        'tool_hash': source_hash(Path(__file__)), 'config_hash': source_hash(repo / '.config/nextest.toml'),
        'repository_hashes': frozen, 'inventory_only': args.inventory_only,
        'generated_hashes': {str(p): byte_hash(p) for p in [*harness.rglob('*'), non_pg] if p.is_file()},
    })


def run(args):
    repo, work = REPO, args.work.resolve()
    plan = read_json(work / 'plan.json')
    write_json(work / 'results.json', {'status': 'running', 'phases': {}})
    if plan['inventory_only']:
        raise ValueError('Inventory-only build cannot stand in for the complete BEAM gate')
    unchanged(repo, plan['repository_hashes'])
    if any(not Path(p).is_file() or byte_hash(Path(p)) != h for p, h in plan['generated_hashes'].items()):
        raise ValueError('Generated workspace/configuration changed since build')
    if plan['repo'] != str(repo) or plan['tool_hash'] != source_hash(Path(__file__)) or plan['policy_hash'] != source_hash(repo / 'tools/testing/driver-groups.json') or plan['config_hash'] != source_hash(repo / '.config/nextest.toml'):
        raise ValueError('Build plan belongs to different source/tool/configuration')
    for name, expected in plan['source_hashes'].items():
        if source_hash(repo / 'crates/ridge-driver/tests' / (name + '.rs')) != expected:
            raise ValueError('Grouped source changed since build: ' + name)
    if args.require_postgres and not os.environ.get('RIDGE_TEST_PG_URL'):
        raise ValueError('--require-postgres needs RIDGE_TEST_PG_URL')
    for tool in ('erl', 'erlc'):
        if not shutil.which(tool):
            raise ValueError('Missing runtime: ' + tool)
    cmd = Commands(repo, work, prefix_from(args))
    results = {}
    for label, selection in plan['selected'].items():
        config = repo / '.config/nextest.toml' if label == 'retained' else Path(plan['non_pg_config'])
        stdout = cmd.run(label + '-run', ['cargo', 'nextest', 'run'] + selection + plan['common'] + ['--config-file', str(config), '--no-fail-fast', '--status-level', 'all', '--success-output', 'immediate'])
        stderr = (work / (label + '-run.stderr')).read_text(encoding='utf-8')
        results[label] = completion(stdout + '\n' + stderr, plan['rows'][label], args.require_postgres)
        unchanged(repo, plan['repository_hashes'])
    skipped = any(result['service_guard_skips'] for result in results.values())
    write_json(work / 'results.json', {
        'status': 'passed', 'phases': results, 'postgres_required': args.require_postgres,
        'service_coverage': 'incomplete' if skipped else 'executed', 'catalog_reused': plan['catalog_reused'],
    })


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('phase', choices=['build', 'run'])
    parser.add_argument('--work', type=Path, default=REPO / 'target/beam-gate')
    parser.add_argument('--audit', action='store_true', help='Always compare original and grouped native inventories')
    parser.add_argument('--inventory-only', action='store_true', help='Build and audit grouped targets only; cannot execute a full gate')
    parser.add_argument('--require-postgres', action='store_true', help='Reject all runtime/service guard skips')
    parser.add_argument('--command-prefix', type=Path, help='JSON argv prefix for each Rust command (local coordinator)')
    args = parser.parse_args()
    (build if args.phase == 'build' else run)(args)


if __name__ == '__main__':
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode) from error
