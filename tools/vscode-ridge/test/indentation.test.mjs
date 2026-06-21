// Checks the indentation rules in language-configuration.json against lines
// drawn from the example programs under examples/.
//
// Ridge is an offside-rule language: the lexer turns indentation into
// Indent/Dedent tokens, so blocks open after a handful of introducers rather
// than after a brace. The editor mirrors that here. A line that ends with a
// block introducer (`=`, `->`, `<-`, `then`, `else`, bare `try`) or is a
// `match` head indents the line below it; the lone `else` of an
// `if ... then ... else` dedents back to the `if`.
//
// The brace form `try { ... }` and other bracket-driven indentation
// (`(`, `[`, `{`) come from the `brackets` configuration, not these
// regexes, so they are not exercised here.
//
// Run with: node test/indentation.test.mjs

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const config = JSON.parse(
  readFileSync(join(here, "..", "language-configuration.json"), "utf8"),
);

const rules = config.indentationRules;
if (!rules || !rules.increaseIndentPattern || !rules.decreaseIndentPattern) {
  console.error("language-configuration.json is missing indentationRules");
  process.exit(1);
}

const increase = new RegExp(rules.increaseIndentPattern);
const decrease = new RegExp(rules.decreaseIndentPattern);

// Lines after which the next line should indent one level.
const shouldIncrease = [
  "fn levelRank (l: Level) -> Int =",
  "fn env io fs main () -> Result Unit Text =",
  "actor Limiter =",
  "    init (cap: Int) (rate: Float) =",
  "    on time allow () -> Bool =",
  "class Encode a =",
  "instance Eq Int =",
  "pub class Refinable q p | q -> p =",
  "fn toJson (x: a) -> Text where Encode a =",
  "    let entries =",
  "    match l",
  "    match (List.length entries)",
  "        let r = match xs",
  "        if capped >= 1.0 then",
  "        if received == 5 then",
  "        else",
  "    guard (List.length args >= 2) else",
  "    pairs |> List.forEach (fn (h, c) ->",
  "            (\"POST\", \"/shorten\") ->",
  "        lastRefill <-",
  "    try",
];

// Lines that complete a statement and must not pull the next line in.
const shouldNotIncrease = [
  "let x = 1",
  "    let limiter   = spawn Limiter 10 2.0",
  "const requestsPerWorker: Int = 20",
  "    state received:      Int = 0",
  "        Info -> 0",
  "            tokens <- capped - 1.0",
  "        lastRefill <- now",
  "    Io.println \"Done.\"",
  "import std.io   as Io",
  "type Level = Info | Warn | Error",
  "    levelRank entry.level >= levelRank threshold",
  "    workers |> List.forEach (fn w -> w ! run ())",
  "    let capped   = if refilled > big then big else refilled",
  "    Io.println \"no match\"",
  "    received <- received + 1",
  "    try {",
  "    let again = retry",
  "    let n = country",
];

// The lone `else` of an if/then/else dedents to align with the `if`.
const shouldDecrease = ["        else", "    else", "        else  -- fallthrough"];

// The trailing `else` of a `guard`, an inline `else`, and the split
// `else <body>` form keep their indentation.
const shouldNotDecrease = [
  "    guard (List.length args >= 2) else",
  "        else return false",
  "               else row)",
  "    elseBranch x",
];

let failures = 0;
const expect = (lines, re, want, label) => {
  for (const line of lines) {
    if (re.test(line) !== want) {
      failures++;
      console.error(`FAIL [${label}] expected ${want}: ${JSON.stringify(line)}`);
    }
  }
};

expect(shouldIncrease, increase, true, "increase");
expect(shouldNotIncrease, increase, false, "increase");
expect(shouldDecrease, decrease, true, "decrease");
expect(shouldNotDecrease, decrease, false, "decrease");

const total =
  shouldIncrease.length +
  shouldNotIncrease.length +
  shouldDecrease.length +
  shouldNotDecrease.length;

if (failures === 0) {
  console.log(`indentation rules: ${total} cases passed`);
} else {
  console.error(`indentation rules: ${failures} of ${total} cases failed`);
  process.exit(1);
}
