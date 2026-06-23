// Snapshot test for the TextMate grammar (syntaxes/ridge.tmLanguage.json).
//
// VS Code colors Ridge by running this grammar through the Oniguruma regex
// engine. A hand-rolled regex pass could not reproduce begin/end nesting,
// pattern priority, or backreferences (the raw-string closer `\1`), so this
// drives the SAME engine VS Code uses — vscode-textmate over vscode-oniguruma
// — across a representative fixture and asserts the resulting scopes against a
// committed snapshot. A grammar edit that re-scopes a token shows up here as a
// diff instead of shipping as a silent coloring regression.
//
// Run with:        node test/grammar.test.mjs
// Update snapshot: UPDATE_SNAPSHOTS=1 node test/grammar.test.mjs

import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { createRequire } from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

// vscode-oniguruma and vscode-textmate are CommonJS; require them so their
// exports land directly (an ESM namespace import hides them behind interop).
const oniguruma = require("vscode-oniguruma");
const vsctm = require("vscode-textmate");

const grammarPath = join(here, "..", "syntaxes", "ridge.tmLanguage.json");
const fixturePath = join(here, "fixtures", "sample.ridge");
const snapshotPath = join(here, "__snapshots__", "grammar.tokens.txt");
const SCOPE = "source.ridge";

// vscode-oniguruma ships the regex engine as a WebAssembly module; load it from
// the installed package so the path stays correct regardless of the hoist layout.
const wasmBin = readFileSync(require.resolve("vscode-oniguruma/release/onig.wasm"));
await oniguruma.loadWASM(wasmBin);

const onigLib = Promise.resolve({
  createOnigScanner: (patterns) => new oniguruma.OnigScanner(patterns),
  createOnigString: (s) => new oniguruma.OnigString(s),
});

const registry = new vsctm.Registry({
  onigLib,
  loadGrammar: async (scopeName) => {
    if (scopeName !== SCOPE) return null;
    return vsctm.parseRawGrammar(readFileSync(grammarPath, "utf8"), grammarPath);
  },
});

const grammar = await registry.loadGrammar(SCOPE);
if (!grammar) {
  console.error(`grammar: failed to load scope ${SCOPE} from ${grammarPath}`);
  process.exit(1);
}

// Tokenize the fixture line by line, threading the rule stack so multi-line
// constructs (block comments, triple strings) resolve correctly. Each
// meaningful (non-whitespace) token becomes one stable, diffable line:
//   "<line>  <json-quoted text>  <scope> <scope> ..."
const lines = readFileSync(fixturePath, "utf8").split(/\r?\n/);
let ruleStack = vsctm.INITIAL;
const rows = [];
for (let i = 0; i < lines.length; i++) {
  const line = lines[i];
  const result = grammar.tokenizeLine(line, ruleStack);
  for (const tok of result.tokens) {
    const text = line.slice(tok.startIndex, tok.endIndex);
    if (text.trim() === "") continue; // skip whitespace-only tokens
    const lineNo = String(i + 1).padStart(3, " ");
    rows.push(`${lineNo}  ${JSON.stringify(text)}  ${tok.scopes.join(" ")}`);
  }
  ruleStack = result.ruleStack;
}
const actual = rows.join("\n") + "\n";

// Compare (or refresh) the snapshot and return the exit code. The loaded
// Oniguruma WASM leaves a libuv handle pending; calling process.exit() while it
// is closing trips an assertion on Windows, so set process.exitCode and let the
// event loop drain naturally instead.
function report() {
  if (process.env.UPDATE_SNAPSHOTS) {
    writeFileSync(snapshotPath, actual);
    console.log(`grammar tokens: snapshot written (${rows.length} tokens) -> ${snapshotPath}`);
    return 0;
  }

  if (!existsSync(snapshotPath)) {
    console.error(
      `grammar: no snapshot at ${snapshotPath}.\n` +
        "Generate it with: UPDATE_SNAPSHOTS=1 node test/grammar.test.mjs",
    );
    return 1;
  }

  // Compare on normalized line endings so a CRLF checkout of the snapshot does
  // not read as a difference (the token scopes are what this guards).
  const norm = (s) => s.replace(/\r\n/g, "\n");
  const expected = norm(readFileSync(snapshotPath, "utf8"));
  const normalized = norm(actual);
  if (normalized === expected) {
    console.log(`grammar tokens: ${rows.length} tokens match snapshot`);
    return 0;
  }

  // Report the first few differing lines so the regression is obvious without
  // scrolling the whole token stream.
  const a = normalized.split("\n");
  const e = expected.split("\n");
  const max = Math.max(a.length, e.length);
  let shown = 0;
  console.error("grammar tokens: snapshot mismatch");
  for (let i = 0; i < max && shown < 12; i++) {
    if (a[i] !== e[i]) {
      console.error(`  line ${i + 1}:`);
      console.error(`    expected: ${e[i] ?? "<missing>"}`);
      console.error(`    actual:   ${a[i] ?? "<missing>"}`);
      shown++;
    }
  }
  console.error(
    "\nIf the grammar change is intentional, refresh the snapshot:\n" +
      "  UPDATE_SNAPSHOTS=1 node test/grammar.test.mjs",
  );
  return 1;
}

process.exitCode = report();
