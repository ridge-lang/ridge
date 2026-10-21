// Exercise the workflow YAML with paths-filter's actual glob matcher. A second
// implementation of glob semantics could agree with itself while CI skips tests.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {test} = require('node:test');
const yaml = require('js-yaml');
const picomatch = require('picomatch');

const root = path.resolve(__dirname, '../..');
const ci = yaml.load(fs.readFileSync(path.join(root, '.github/workflows/ci.yml'), 'utf8'));
const bench = yaml.load(fs.readFileSync(path.join(root, '.github/workflows/bench.yml'), 'utf8'));
const jobs = [ci.jobs['build-and-test'], ci.jobs['beam-e2e'],
  ci.jobs['lsp-acceptance'], bench.jobs['layer-a']];
const filters = jobs.map(job => {
  const step = job.steps.find(step => step.id === 'changes');
  assert.equal(step.uses, 'dorny/paths-filter@v4', 'review matcher compatibility when updating the action');
  assert.equal(step.with['predicate-quantifier'], 'every');
  const rules = yaml.load(step.with.filters).code;
  assert(Array.isArray(rules) && rules.length > 0);
  assert(rules.every(rule => typeof rule === 'string'), 'review status-specific or structured filter rules');
  // paths-filter compiles each string with dot:true and combines rules with
  // every; a changeset matches if at least one file satisfies all its rules.
  return rules.map(rule => picomatch(rule, {dot: true}));
});

const all = [true, true, true, true];
const none = [false, false, false, false];
const example = [true, true, false, false];
const dogfood = [true, false, false, false];
const cases = [
  ['canonical example', ['examples/log_analyzer.ridge'], example],
  ['nested example', ['examples/data/users-crud/src/Main.ridge'], example],
  ['example manifest', ['examples/data/users-crud/ridge.toml'], example],
  ['example runtime input', ['examples/input.log'], example],
  ['dogfood formatter corpus', ['dogfood/wc/src/Main.ridge'], dogfood],
  ['new nested dogfood source', ['dogfood/new-app/src/nested/Helper.ridge'], dogfood],
  ['root README', ['README.md'], none],
  ['example README', ['examples/README.md'], none],
  ['nested example README', ['examples/data/users-crud/README.md'], none],
  ['dogfood README', ['dogfood/wc/README.md'], none],
  ['docs', ['docs/data.md'], none],
  ['branding', ['assets/logo.svg'], none],
  ['community metadata', ['.github/ISSUE_TEMPLATE/bug_report.yml'], none],
  ['Rust source', ['crates/ridge-parser/src/lib.rs'], all],
  ['workflow', ['.github/workflows/ci.yml'], all],
  ['filter controls', ['tools/testing/test_path_filters.cjs'], all],
  ['tooling README retains existing selection', ['tools/testing/README.md'], all],
  ['Markdown fixture outside examples retains selection', ['crates/ridge-lsp/tests/fixtures/notes.md'], all],
  ['mixed docs and dogfood', ['dogfood/wc/README.md', 'dogfood/wc/src/Main.ridge'], dogfood],
  ['mixed docs and examples', ['examples/README.md', 'examples/log_analyzer.ridge'], example],
  ['mixed docs and Rust', ['README.md', 'crates/ridge-parser/src/lib.rs'], all],
  ['empty changeset', [], none],
];

for (const [name, files, expected] of cases) {
  test(name, () => {
    const actual = filters.map(rules => files.some(file => rules.every(match => match(file))));
    assert.deepEqual(actual, expected, 'job order: workspace, BEAM, LSP acceptance, Layer A');
  });
}

test('filter controls run independently of the filters under test', () => {
  const job = ci.jobs['path-filter-controls'];
  assert(job, 'the selection regression controls must run in CI');
  assert.equal(job.if, undefined);
  const install = job.steps.find(step => step.run === 'npm ci --ignore-scripts --prefix tools/testing');
  const run = job.steps.find(step => step.run === 'npm test --prefix tools/testing');
  assert(install && run);
  assert.equal(install.if, undefined);
  assert.equal(run.if, undefined);
  assert(!job.steps.some(step => step.uses?.startsWith('dorny/paths-filter@')));
});
