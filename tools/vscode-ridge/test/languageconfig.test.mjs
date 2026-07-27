// Validates language-configuration.json: wordPattern behavior, auto-closing
// pairs, folding mode, and that indentation rules compile.
//
// Run with: node test/languageconfig.test.mjs

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const config = JSON.parse(
  readFileSync(join(here, "..", "language-configuration.json"), "utf8"),
);

let failures = 0;
const check = (ok, msg) => {
  if (!ok) {
    failures++;
    console.error(`FAIL: ${msg}`);
  }
};

// ── wordPattern ─────────────────────────────────────────────────────────
check(typeof config.wordPattern === "string", "wordPattern missing");
const wp = new RegExp(config.wordPattern, "g");
const wordAt = (line) => (line.match(wp) ?? []).join(",");
check(
  wordAt("camelCase PascalCase snake_case") === "camelCase,PascalCase,snake_case",
  `wordPattern identifiers: ${wordAt("camelCase PascalCase snake_case")}`,
);
check(wordAt("a -> b") === "a,b", "wordPattern must not swallow ->");
check(wordAt("x |> y") === "x,y", "wordPattern must not swallow |>");

// ── autoClosingPairs ────────────────────────────────────────────────────
const pairs = config.autoClosingPairs ?? [];
const opens = pairs.map((p) => p.open);
check(new Set(opens).size === opens.length, "duplicate autoClosingPairs opens");
check(opens.includes('"""'), "triple-quote auto-close missing");
for (const p of pairs) {
  check(
    typeof p.close === "string" && p.close !== "",
    `pair ${p.open} has no close`,
  );
  for (const ctx of p.notIn ?? []) {
    check(
      ["string", "comment"].includes(ctx),
      `pair ${p.open}: bad notIn ${ctx}`,
    );
  }
}

// ── folding ─────────────────────────────────────────────────────────────
check(
  config.folding?.offSide === true,
  "folding.offSide must be true (offside language)",
);

// ── indentation rules compile ───────────────────────────────────────────
new RegExp(config.indentationRules.increaseIndentPattern);
new RegExp(config.indentationRules.decreaseIndentPattern);
check(
  config.indentationRules.increaseIndentPattern.includes("\\|>"),
  "increaseIndentPattern must cover pipeline |> continuation",
);

if (failures === 0) {
  console.log("languageconfig: all checks passed");
} else {
  console.error(`languageconfig: ${failures} failures`);
  process.exit(1);
}
