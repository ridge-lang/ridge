// Activation/manifest coherence test for the VS Code extension.
//
// Most "the extension does nothing" reports trace back to a manifest that no
// longer agrees with the files it points at: an activation event that names a
// language id nothing contributes, a grammar `path` that moved, a `scopeName`
// that drifted from the grammar file's own, or a bundle that fails to load.
// None of these need a running editor to catch. This validates the static
// contributions and then loads the bundled entry point (with `vscode` stubbed)
// to confirm it still exports the activation hooks — a faithful activation
// check without the cost and flakiness of launching Electron in CI.
//
// Run with: node test/activation.test.mjs   (run `pnpm run bundle` first, or
//            use `pnpm test`, so out/extension.js exists for the load smoke).

import { readFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";
import { createRequire } from "node:module";
import Module from "node:module";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..");
const require = createRequire(import.meta.url);

let failures = 0;
const fail = (msg) => {
  failures++;
  console.error(`FAIL ${msg}`);
};
const check = (cond, msg) => {
  if (!cond) fail(msg);
};

const readJson = (p) => JSON.parse(readFileSync(p, "utf8"));

// --- package.json: required fields -----------------------------------------
const pkg = readJson(join(root, "package.json"));
for (const field of ["name", "publisher", "version", "engines", "main", "contributes"]) {
  check(pkg[field] !== undefined, `package.json is missing "${field}"`);
}
check(
  pkg.engines && typeof pkg.engines.vscode === "string",
  'package.json "engines.vscode" must be a version range',
);

// --- the contributed language ----------------------------------------------
const languages = pkg.contributes?.languages ?? [];
const ridgeLang = languages.find((l) => l.id === "ridge");
check(ridgeLang !== undefined, 'no language with id "ridge" is contributed');
if (ridgeLang) {
  check(
    Array.isArray(ridgeLang.extensions) && ridgeLang.extensions.includes(".ridge"),
    'the "ridge" language must contribute the ".ridge" extension',
  );
  check(
    typeof ridgeLang.configuration === "string",
    'the "ridge" language must reference a language-configuration file',
  );
  if (typeof ridgeLang.configuration === "string") {
    const cfgPath = join(root, ridgeLang.configuration);
    check(existsSync(cfgPath), `language configuration not found: ${ridgeLang.configuration}`);
    if (existsSync(cfgPath)) {
      try {
        readJson(cfgPath); // must be valid JSON or the editor ignores it
      } catch (err) {
        fail(`language configuration is not valid JSON: ${String(err)}`);
      }
    }
  }
}

// --- activation events agree with the contributed language ------------------
const activation = pkg.activationEvents ?? [];
check(
  activation.includes("onLanguage:ridge"),
  'activationEvents must include "onLanguage:ridge" so the server starts on a .ridge file',
);

// --- the contributed grammar matches the grammar file -----------------------
const grammars = pkg.contributes?.grammars ?? [];
const ridgeGrammar = grammars.find((g) => g.language === "ridge");
check(ridgeGrammar !== undefined, 'no grammar is contributed for the "ridge" language');
if (ridgeGrammar) {
  check(
    ridgeGrammar.scopeName === "source.ridge",
    `contributed grammar scopeName is "${ridgeGrammar.scopeName}", expected "source.ridge"`,
  );
  const grammarPath = join(root, ridgeGrammar.path);
  check(existsSync(grammarPath), `grammar file not found: ${ridgeGrammar.path}`);
  if (existsSync(grammarPath)) {
    const grammar = readJson(grammarPath);
    check(
      grammar.scopeName === ridgeGrammar.scopeName,
      `grammar file scopeName "${grammar.scopeName}" does not match the contributed "${ridgeGrammar.scopeName}"`,
    );
    // Every `include` must resolve to a repository rule (or a self/base
    // reference); a dangling include silently drops a whole class of tokens.
    const repo = grammar.repository ?? {};
    const includes = [];
    const collect = (node) => {
      if (Array.isArray(node)) {
        node.forEach(collect);
      } else if (node && typeof node === "object") {
        if (typeof node.include === "string") includes.push(node.include);
        Object.values(node).forEach(collect);
      }
    };
    collect(grammar.patterns ?? []);
    collect(repo);
    for (const inc of includes) {
      if (inc === "$self" || inc === "$base" || inc.startsWith("source.")) continue;
      const key = inc.startsWith("#") ? inc.slice(1) : inc;
      check(
        Object.prototype.hasOwnProperty.call(repo, key),
        `grammar include "${inc}" has no matching repository rule`,
      );
    }
  }
}

// --- semantic-token contributions are internally consistent -----------------
const tokenTypes = (pkg.contributes?.semanticTokenTypes ?? []).map((t) => t.id);
const tokenScopes = pkg.contributes?.semanticTokenScopes ?? [];
for (const entry of tokenScopes) {
  for (const id of Object.keys(entry.scopes ?? {})) {
    check(
      tokenTypes.includes(id),
      `semanticTokenScopes maps "${id}", which is not declared in semanticTokenTypes`,
    );
  }
}

// --- contributed commands back the code-lens client commands ----------------
const commands = pkg.contributes?.commands ?? [];
const commandIds = new Set(commands.map((c) => c.command));
for (const id of ["ridge.run", "ridge.test"]) {
  check(
    commandIds.has(id),
    `package.json must contribute the "${id}" command the code lenses invoke`,
  );
}
// Those commands are lens-only (they need arguments), so they are hidden from
// the palette — a no-arg invocation would run the CLI without a target.
const palette = pkg.contributes?.menus?.commandPalette ?? [];
for (const id of ["ridge.run", "ridge.test"]) {
  const entry = palette.find((m) => m.command === id);
  check(
    entry !== undefined && entry.when === "false",
    `"${id}" must be hidden from the command palette (when: "false")`,
  );
}

// --- load smoke: the bundled entry point still exports the hooks ------------
const mainPath = resolve(root, pkg.main);
if (!existsSync(mainPath)) {
  fail(
    `bundle not found: ${pkg.main}. Build it first with "pnpm run bundle" ` +
      '(the "pnpm test" script does this automatically).',
  );
} else {
  // The bundle keeps `vscode` external (esbuild --external:vscode), so it is
  // unresolvable outside the extension host. The inlined language client also
  // touches `vscode` at module-load time (e.g. `class X extends vscode.Y`), so
  // a plain object stub is not enough. Hand back a recursive, constructable
  // Proxy: every property is itself a stub function with a real prototype, so
  // it works whether the bundle reads a value, calls it, constructs it, or
  // extends it. We only inspect the exports here, never call into the API.
  const makeStub = () => {
    const fn = function () {};
    return new Proxy(fn, {
      get(target, prop) {
        if (prop === "prototype") return target.prototype;
        if (prop === "then") return undefined; // not a thenable
        if (typeof prop === "symbol") return target[prop];
        if (!(prop in target)) target[prop] = makeStub();
        return target[prop];
      },
      apply: () => makeStub(),
      construct: () => ({}),
    });
  };
  const vscodeStub = makeStub();
  const realLoad = Module._load;
  Module._load = function (request, parent, isMain) {
    if (request === "vscode") return vscodeStub;
    return realLoad.call(this, request, parent, isMain);
  };
  try {
    const ext = require(mainPath);
    check(typeof ext.activate === "function", "bundle does not export an activate() function");
    check(typeof ext.deactivate === "function", "bundle does not export a deactivate() function");
    check(
      typeof ext.activate === "function" && ext.activate.length >= 1,
      "activate() should take an extension context argument",
    );
  } catch (err) {
    fail(`failed to load the bundled entry point: ${String(err)}`);
  } finally {
    Module._load = realLoad;
  }
}

if (failures === 0) {
  console.log("activation: manifest, grammar, and bundle checks passed");
} else {
  console.error(`activation: ${failures} check(s) failed`);
  process.exit(1);
}
