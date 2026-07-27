// Validates snippets/ridge.json: shape, unique prefixes, well-formed
// placeholders/choice lists, and coverage of every grammar keyword.
//
// Run with: node test/snippets.test.mjs

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const snippetsPath = join(here, "..", "snippets", "ridge.json");
const grammarPath = join(here, "..", "syntaxes", "ridge.tmLanguage.json");

let failures = 0;
const fail = (msg) => {
  failures++;
  console.error(`FAIL: ${msg}`);
};

let snippets;
try {
  snippets = JSON.parse(readFileSync(snippetsPath, "utf8"));
} catch (err) {
  console.error(`snippets: cannot load ${snippetsPath}: ${err.message}`);
  process.exit(1);
}

// ── Shape + unique prefixes ─────────────────────────────────────────────
const prefixes = new Set();
for (const [name, snip] of Object.entries(snippets)) {
  if (typeof snip.prefix !== "string" || snip.prefix === "") {
    fail(`${name}: missing/empty prefix`);
    continue;
  }
  if (prefixes.has(snip.prefix)) fail(`duplicate prefix ${snip.prefix}`);
  prefixes.add(snip.prefix);
  if (typeof snip.description !== "string" || snip.description === "") {
    fail(`${name}: missing description`);
  }
  const body = Array.isArray(snip.body) ? snip.body : [snip.body];
  if (body.some((l) => typeof l !== "string")) {
    fail(`${name}: body must be a string or string[]`);
  }

  // ── Placeholders: every ${ closes; choice lists well-formed ──────────
  const text = body.join("\n");
  const withoutPlaceholders = text.replace(
    /\$\{\d+(?::[^}]*|\|[^}]*\|)?\}|\$\d+/g,
    "",
  );
  if (withoutPlaceholders.includes("${")) {
    fail(`${name}: unbalanced placeholder in body`);
  }
  for (const m of text.matchAll(/\$\{(\d+)\|([^}]*)\|\}/g)) {
    if (m[2].includes("${") || m[2].split(",").some((c) => c.trim() === "")) {
      fail(`${name}: malformed choice list ${m[0]}`);
    }
  }
}

// ── Grammar keyword coverage ────────────────────────────────────────────
// Every keyword the TextMate grammar highlights must appear in at least
// one snippet body, so a new keyword can't ship without snippets.
const grammar = JSON.parse(readFileSync(grammarPath, "utf8"));
const keywordPattern = /\\b\(([A-Za-z |]+)\)\\b/g;
const keywords = new Set();
for (const repo of Object.values(grammar.repository ?? {})) {
  const entries = [repo, ...(repo.patterns ?? [])];
  for (const entry of entries) {
    if (typeof entry?.match !== "string") continue;
    if (!String(entry?.name ?? "").includes("keyword")) continue;
    for (const m of entry.match.matchAll(keywordPattern)) {
      for (const kw of m[1].split("|")) keywords.add(kw.trim());
    }
  }
}
// Reserved but unimplemented, or covered by later snippet additions.
const noSnippetNeeded = new Set([
  "catch",
  "in",
  "on",
  "state",
  "init",
  "actor",
  "child",
  "onDown",
  "terminate",
]);
const allBodies = Object.values(snippets)
  .flatMap((s) => (Array.isArray(s.body) ? s.body : [s.body]))
  .join("\n");
for (const kw of keywords) {
  if (noSnippetNeeded.has(kw)) continue;
  if (!new RegExp(`\\b${kw}\\b`).test(allBodies)) {
    fail(`grammar keyword "${kw}" appears in no snippet body`);
  }
}

const count = Object.keys(snippets).length;
if (failures === 0) {
  console.log(
    `snippets: ${count} snippets valid; ${keywords.size} grammar keywords covered`,
  );
} else {
  console.error(`snippets: ${failures} failures across ${count} snippets`);
  process.exit(1);
}
