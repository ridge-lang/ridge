"""Controls for inventory loss, unavailable services and generated configuration."""
import importlib.util
import io
from pathlib import Path
import tempfile
import tomllib
import unittest

spec = importlib.util.spec_from_file_location('beam_gate', Path(__file__).with_name('beam-gate.py'))
gate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate)


class InventoryControls(unittest.TestCase):
    rows = [('fixture', 'active', False), ('fixture', 'ignored', True)]

    def test_nextest_unicode_can_be_printed_from_windows_legacy_console(self):
        buffer = io.BytesIO()
        stream = io.TextIOWrapper(buffer, encoding='cp1252')
        gate.utf8_output(stream)
        print('─ Summary ✓', file=stream, flush=True)
        self.assertEqual(buffer.getvalue().decode('utf-8'), '─ Summary ✓\n')

    def test_parity_both_directions_and_ignored_states(self):
        gate.parity(self.rows, self.rows)
        for changed in [self.rows[:1], self.rows + [('extra', 'new', False)],
                        [self.rows[0], ('fixture', 'ignored', False)], self.rows + self.rows[:1]]:
            with self.subTest(changed=changed), self.assertRaises(ValueError):
                gate.parity(changed, self.rows)

    def test_native_suite_normalization_and_duplicates(self):
        suite = {'status': 'listed', 'binary-name': 'runtime', 'testcases': {'fixture::active': {'ignored': False}}}
        data = {'rust-suites': {'first': suite}}
        self.assertEqual(gate.inventories(data, True), self.rows[:1])
        data['rust-suites']['duplicate'] = suite
        with self.assertRaises(ValueError):
            gate.inventories(data, True)
        suite['status'] = 'skipped'
        with self.assertRaises(ValueError):
            gate.inventories(data, True)

    def test_completion_singular_plural_and_guarded_service(self):
        for spelling in ('test', 'tests'):
            body = f'Summary [1s] 1 {spelling} run: 1 passed, 1 skipped'
            self.assertEqual(gate.completion(body, self.rows, True)['passed'], 1)
        body += '\nRIDGE_TEST_PG_URL not set — skipping database'
        self.assertEqual(len(gate.completion(body, self.rows, False)['service_guard_skips']), 1)
        with self.assertRaises(ValueError):
            gate.completion(body, self.rows, True)

    def test_partial_run_or_unavailable_runtime_cannot_pass(self):
        for body in ['Everything passed', 'Summary [1s] 0 tests run: 0 passed',
                     'Summary [1s] 2 tests run: 1 passed, 1 failed',
                     'Summary [1s] 1 test run: 1 passed\nerl/erlc not on PATH — skipping fixture',
                     'Summary [1s] 1 test run: 1 passed\nSummary [1s] 1 test run: 1 passed']:
            with self.subTest(body=body), self.assertRaises(ValueError):
                gate.completion(body, self.rows, False)

    def test_feature_graph_detects_feature_loss(self):
        original = gate.feature_graph('dependency v1|a,b\ndependency v1|a,b (*)\n')
        grouped = gate.feature_graph('ridge-grouped-driver-gate v1|beam-runtime\ndependency v1|a,b\n')
        self.assertEqual(original, grouped)
        self.assertNotEqual(original, gate.feature_graph('dependency v1|a\n'))

    def test_cross_target_listing_rejected(self):
        data = {'rust-build-meta': {'target-platforms': [{'triple': 'native'}]}}
        gate.native_listing(data, 'native')
        with self.assertRaises(ValueError):
            gate.native_listing(data, 'other')

    def test_incremental_reference_never_reuses_changed_or_other_platform_sources(self):
        contract = {'schema': 1, 'target': 'native', 'features': ['beam-runtime'], 'sources': {'stable': 'same', 'changed': 'new', 'added': 'new'}}
        catalog = contract | {'sources': {'stable': 'same', 'changed': 'old'}, 'cases': [('stable', 'kept', False), ('changed', 'old', False)]}
        needed, rows = gate.catalog_reference(catalog, contract, False)
        self.assertEqual(needed, ['added', 'changed'])
        self.assertEqual(rows, [('stable', 'kept', False)])
        for other in [catalog | {'target': 'foreign'}, catalog | {'features': []}, {}]:
            needed, rows = gate.catalog_reference(other, contract, False)
            self.assertEqual(needed, ['added', 'changed', 'stable'])
            self.assertEqual(rows, [])
        self.assertEqual(gate.catalog_reference(catalog, contract, True), (['added', 'changed', 'stable'], []))

    def test_cargo_directory_target_cannot_disappear_from_partition(self):
        policy = {'groups': {'runtime': ['grouped']}, 'retained': ['retained']}
        def target(name):
            return {'name': name, 'kind': ['test'], 'src_path': str(gate.REPO / 'crates/ridge-driver/tests' / (name + '.rs'))}
        metadata = {'packages': [{'name': 'ridge-driver', 'targets': [target('grouped'), target('retained')]}]}
        gate.driver_targets(metadata, gate.REPO, policy)
        metadata['packages'][0]['targets'].append(target('directory/main'))
        with self.assertRaises(ValueError):
            gate.driver_targets(metadata, gate.REPO, policy)


class GenerationControls(unittest.TestCase):
    def test_source_hash_is_checkout_independent_and_changes_on_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'fixture.rs'
            path.write_bytes(b'first\nsecond\n')
            digest = gate.source_hash(path)
            path.write_bytes(b'first\r\nsecond\r\n')
            self.assertEqual(digest, gate.source_hash(path))
            path.write_bytes(b'changed\nsecond\n')
            self.assertNotEqual(digest, gate.source_hash(path))

    def test_source_change_and_deletion_invalidate_build_plan(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            path = repo / 'source.rs'
            path.write_bytes(b'original')
            frozen = {'source.rs': gate.byte_hash(path)}
            gate.unchanged(repo, frozen)
            path.write_bytes(b'changed')
            with self.assertRaises(ValueError):
                gate.unchanged(repo, frozen)
            path.unlink()
            with self.assertRaises(ValueError):
                gate.unchanged(repo, frozen)

    def test_real_manifest_profiles_and_effective_clippy_config_survive(self):
        policy = gate.read_json(gate.REPO / 'tools/testing/driver-groups.json')
        with tempfile.TemporaryDirectory() as tmp:
            harness, hashes = gate.prepare(gate.REPO, Path(tmp), policy)
            manifest = tomllib.loads((harness / 'Cargo.toml').read_text(encoding='utf-8'))
            original = tomllib.loads((gate.REPO / 'Cargo.toml').read_text(encoding='utf-8'))
            self.assertEqual(manifest['workspace']['lints'], original['workspace']['lints'])
            self.assertEqual(manifest.get('profile'), original.get('profile'))
            self.assertEqual((harness / 'clippy.toml').read_bytes(), (gate.REPO / 'clippy.toml').read_bytes())
            self.assertEqual(set(hashes), {n for names in policy['groups'].values() for n in names})
            config = gate.project_config(gate.REPO, Path(tmp))
            self.assertEqual(tomllib.loads(config.read_text())['test-groups']['postgres']['max-threads'], 1)
            self.assertNotIn('overrides', tomllib.loads(config.read_text()).get('profile', {}).get('default', {}))

    def test_new_or_context_sensitive_fixture_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            sources = repo / 'crates/ridge-driver/tests'
            sources.mkdir(parents=True)
            (sources / 'grouped.rs').write_text('#[test]\nfn active() {}')
            (sources / 'retained.rs').write_text('')
            policy = {'groups': {'analysis': ['grouped']}, 'retained': ['retained']}
            gate.partition(repo, policy)
            (sources / 'new.rs').write_text('')
            with self.assertRaises(ValueError):
                gate.partition(repo, policy)
            (sources / 'new.rs').unlink()
            (sources / 'grouped.rs').write_text('env!("CARGO_MANIFEST_DIR")')
            with self.assertRaises(ValueError):
                gate.partition(repo, policy)


if __name__ == '__main__':
    unittest.main()
