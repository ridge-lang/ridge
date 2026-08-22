//! What the editor shows when a reader hovers a built-in type name.
//!
//! Every other hover card is lifted from a declaration. A built-in has none —
//! there is no `type Text = …` anywhere to read — so hovering one used to
//! return nothing at all, which reads as "the editor does not know what this
//! is" rather than "this is part of the language". The table below is the
//! replacement: a signature line whose type parameters have names instead of
//! single letters, one sentence saying what the type is, and, where there is
//! one, the property that catches people out.
//!
//! Three rules for anything added here.
//!
//! The prose says what the language guarantees and never how a value is laid
//! out at runtime. A card that mentions a representation is a card that has to
//! be rewritten for the next backend, and it teaches a reader something they
//! cannot rely on.
//!
//! Parameters get real names. `Result a e` is exactly the card a reader does
//! not need — they can already see the two letters. `Result value error` is
//! the answer to the question they hovered to ask.
//!
//! The table is total. The test at the bottom interns a fresh arena and walks
//! every built-in in it, so a type added to the language without a card fails
//! there rather than reaching a user as silence. The only names it excuses are
//! the synthetic per-arity function constructors, and it recognises those by
//! asking the function that creates them rather than by carrying a list that
//! can go stale.

/// One built-in type's hover card.
pub struct BuiltinCard {
    /// The name with its type parameters spelled out — `Map key value` rather
    /// than `Map k v`.
    pub signature: &'static str,
    /// One sentence: what the type is.
    pub summary: &'static str,
    /// The property worth knowing before reaching for it, or `""` when the
    /// summary already says everything there is to say.
    pub note: &'static str,
}

/// The card for `name`, or `None` when the name is not a built-in type.
///
/// The lookup is a linear scan over a table of a few dozen entries, run once
/// per hover — a mouse-rest event, not a keystroke — so there is nothing here
/// worth indexing.
#[must_use]
pub fn builtin_card(name: &str) -> Option<&'static BuiltinCard> {
    CARDS.iter().find(|(n, _)| *n == name).map(|(_, c)| c)
}

/// Every built-in type, with the card shown on hover.
pub static CARDS: &[(&str, BuiltinCard)] = &[
    // ── Scalars ──────────────────────────────────────────────────────────────
    (
        "Int",
        BuiltinCard {
            signature: "Int",
            summary: "A 64-bit signed integer.",
            note: "Holds -9223372036854775808 through 9223372036854775807. Arithmetic that would leave that range raises instead of wrapping or widening, so a program gets the number it computed or an error, and never a different number reported as success. `Int.wrappingAdd` and `Int.saturatingAdd` are there for the cases that want one of the other answers.",
        },
    ),
    (
        "Float",
        BuiltinCard {
            signature: "Float",
            summary: "A 64-bit IEEE 754 double.",
            note: "Not the type for money: `0.1` has no exact double, and the error compounds over a sum. `Decimal` is.",
        },
    ),
    (
        "Bool",
        BuiltinCard {
            signature: "Bool",
            summary: "Either `true` or `false`.",
            note: "",
        },
    ),
    (
        "Text",
        BuiltinCard {
            signature: "Text",
            summary: "A UTF-8 string.",
            note: "",
        },
    ),
    (
        "Unit",
        BuiltinCard {
            signature: "Unit",
            summary: "The type with a single value, written `()`.",
            note: "What a function returns when it runs for its effect and has no answer to give.",
        },
    ),
    (
        "Decimal",
        BuiltinCard {
            signature: "Decimal",
            summary: "An exact base-10 number with arbitrary precision.",
            note: "Written with an `m` suffix, as in `19.99m`. This is the type for money.",
        },
    ),
    (
        "Uuid",
        BuiltinCard {
            signature: "Uuid",
            summary: "An RFC 4122 identifier.",
            note: "There is no literal for one; `Uuid.generate` and `Uuid.fromText` are where they come from.",
        },
    ),
    (
        "Bytes",
        BuiltinCard {
            signature: "Bytes",
            summary: "A raw byte string.",
            note: "There is no literal for one; `Bytes.fromHex`, `Bytes.fromUtf8` and `Bytes.generate` are where they come from.",
        },
    ),
    // ── Time ─────────────────────────────────────────────────────────────────
    (
        "Timestamp",
        BuiltinCard {
            signature: "Timestamp",
            summary: "A point on the wall clock.",
            note: "Opaque, with no literal: `std.time` is where one comes from. To measure how long something took, reach for `Instant` instead — the wall clock can jump backwards when it is adjusted, and a span measured across that jump is wrong.",
        },
    ),
    (
        "Instant",
        BuiltinCard {
            signature: "Instant",
            summary: "A reading of the monotonic clock.",
            note: "It only ever moves forward, so a span between two readings is never negative — which is what makes it, and not `Timestamp`, the way to time something. It means nothing as an absolute time, and nothing across a restart.",
        },
    ),
    (
        "Date",
        BuiltinCard {
            signature: "Date",
            summary: "A calendar day — year, month, and day of month — with no time of day and no zone.",
            note: "Kept apart from `Timestamp` on purpose: a date has no hours for an offset to shift.",
        },
    ),
    (
        "Time",
        BuiltinCard {
            signature: "Time",
            summary: "A time of day — hour, minute, second, and an optional fraction — with no date and no zone.",
            note: "It deliberately does not combine with a `Date` into an instant. That needs a zone, and reading the pair as UTC instead is the bug other languages spent years unwinding. An absolute point in time is a `Timestamp`.",
        },
    ),
    (
        "Duration",
        BuiltinCard {
            signature: "Duration",
            summary: "A span of time in whole milliseconds: `{ ms: Int }`.",
            note: "It can be negative, the same way the difference between two timestamps can run backwards.",
        },
    ),
    // ── Containers ───────────────────────────────────────────────────────────
    (
        "List",
        BuiltinCard {
            signature: "List item",
            summary: "An immutable sequence.",
            note: "",
        },
    ),
    (
        "Map",
        BuiltinCard {
            signature: "Map key value",
            summary: "An immutable map from keys to values.",
            note: "",
        },
    ),
    (
        "Set",
        BuiltinCard {
            signature: "Set item",
            summary: "An immutable set.",
            note: "",
        },
    ),
    (
        "Option",
        BuiltinCard {
            signature: "Option value",
            summary: "Either `Some value` or `None`.",
            note: "The absence lives in the type, so the compiler asks about it at every use rather than leaving it to a convention.",
        },
    ),
    (
        "Result",
        BuiltinCard {
            signature: "Result value error",
            summary: "Either `Ok value` or `Err error`.",
            note: "A failure is an ordinary value here, which is what makes a signature honest about the calls that can fail.",
        },
    ),
    // ── Core nominal types ───────────────────────────────────────────────────
    (
        "Error",
        BuiltinCard {
            signature: "Error",
            summary: "A failure carrying a code and a message: `{ code: Text, message: Text }`.",
            note: "The standard failure value across the stdlib, and usually what an `Err` holds.",
        },
    ),
    (
        "Ordering",
        BuiltinCard {
            signature: "Ordering",
            summary: "`Less`, `Equal`, or `Greater`.",
            note: "What a comparison answers and what a sort reads.",
        },
    ),
    (
        "JsonValue",
        BuiltinCard {
            signature: "JsonValue",
            summary: "A JSON document as a value: `JNull`, `JBool`, `JInt`, `JFloat`, `JText`, `JList`, or `JObject`.",
            note: "In scope everywhere, so JSON can be built and matched without an import.",
        },
    ),
    (
        "Output",
        BuiltinCard {
            signature: "Output",
            summary: "What a finished process left behind: `{ stdout: Text, stderr: Text, exitCode: Int }`.",
            note: "",
        },
    ),
    (
        "Parsed",
        BuiltinCard {
            signature: "Parsed",
            summary: "Command-line arguments after parsing: `{ flags: Map Text Text, switches: List Text, positionals: List Text }`.",
            note: "",
        },
    ),
    // ── Actors ───────────────────────────────────────────────────────────────
    (
        "Handle",
        BuiltinCard {
            signature: "Handle actor",
            summary: "A reference to a running actor.",
            note: "Holding one is what lets a caller send to that actor, so passing it around is how reach is granted.",
        },
    ),
    (
        "ChildSpec",
        BuiltinCard {
            signature: "ChildSpec actor",
            summary: "How a supervisor should start one child.",
            note: "`child ActorName (args…)` builds one.",
        },
    ),
    (
        "Supervisor",
        BuiltinCard {
            signature: "Supervisor actor",
            summary: "A reference to a running supervisor.",
            note: "`supervise` in `std.actor` returns one.",
        },
    ),
    (
        "Monitor",
        BuiltinCard {
            signature: "Monitor",
            summary: "A subscription to another actor's exit.",
            note: "",
        },
    ),
    // ── Types that keep trusted and untrusted text apart ─────────────────────
    (
        "Sql",
        BuiltinCard {
            signature: "Sql",
            summary: "A finished SQL statement.",
            note: "Opaque, and built only by the query layer. The values a statement uses travel beside it as bound parameters rather than inside it, which is what stops a value from ever being read as SQL.",
        },
    ),
    (
        "Html",
        BuiltinCard {
            signature: "Html",
            summary: "A fragment of HTML.",
            note: "Opaque, so it cannot be confused with the `Text` it was built from and cannot be taken apart outside the module that makes it.",
        },
    ),
    (
        "SecureCookie",
        BuiltinCard {
            signature: "SecureCookie",
            summary: "A cookie built with safe defaults: `{ name, value, secure, httpOnly, sameSite, maxAge, path }`.",
            note: "Opaque. Start from the `secureCookie` factory and adjust it with the `with*` setters, so a cookie cannot be assembled with a flag quietly left off.",
        },
    ),
    (
        "SqlValue",
        BuiltinCard {
            signature: "SqlValue",
            summary: "One value on its way to or from a database column, tagged with the SQL type it travels as.",
            note: "Opaque: reach it through the `toSql` and `fromSql` methods rather than by building a variant.",
        },
    ),
    // ── Quotation ────────────────────────────────────────────────────────────
    (
        "Quote",
        BuiltinCard {
            signature: "Quote signature",
            summary: "A lambda captured as data rather than compiled to a call.",
            note: "Pass a lambda where a `Quote` is expected and the compiler keeps the body instead of lowering it to a closure. That is how a predicate written in Ridge becomes SQL: a later pass walks the captured tree.",
        },
    ),
    (
        "QExpr",
        BuiltinCard {
            signature: "QExpr",
            summary: "One node of a captured expression tree.",
            note: "In scope everywhere, and built by the compiler rather than by hand. `std.query` walks one to render it or to compile it to parameterized SQL.",
        },
    ),
    // ── Schema descriptors, produced by deriving ─────────────────────────────
    (
        "Column",
        BuiltinCard {
            signature: "Column entity value",
            summary: "A typed reference to one column, from `deriving (Table)`.",
            note: "`name` and `table` are readable from it.",
        },
    ),
    (
        "Table",
        BuiltinCard {
            signature: "Table entity",
            summary: "A table's metadata from `deriving (Table)`: its name and its column names in order.",
            note: "",
        },
    ),
    (
        "Schema",
        BuiltinCard {
            signature: "Schema",
            summary: "The structural descriptor from `deriving (Schema)`: the type's name, its table, and one entry per field.",
            note: "",
        },
    ),
    (
        "FieldSchema",
        BuiltinCard {
            signature: "FieldSchema",
            summary: "One field's entry in a `deriving (Schema)` descriptor: the field name, the column it maps to, its type, and whether it is optional.",
            note: "",
        },
    ),
    // ── Type-level extractors the compiler reduces ───────────────────────────
    //
    // These appear in the query builder's signatures and in a type error before
    // they reduce, which is exactly when a reader wants to know they are not
    // something to write. Every card says so in the same words.
    (
        "Ret",
        BuiltinCard {
            signature: "Ret projection",
            summary: "The value a projection returns.",
            note: "Written by the compiler, not by hand: `Ret (fn row -> value)` reduces to `value`.",
        },
    ),
    (
        "Rows",
        BuiltinCard {
            signature: "Rows query",
            summary: "The row a query yields.",
            note: "Written by the compiler, not by hand: for a plain query it reduces to the entity, and for a join to the combined row.",
        },
    ),
    (
        "JoinCond",
        BuiltinCard {
            signature: "JoinCond query other",
            summary: "The condition shape a join expects.",
            note: "Written by the compiler, not by hand: it reduces to a function from the query's row and the other side to `Bool`.",
        },
    ),
    (
        "JoinResult",
        BuiltinCard {
            signature: "JoinResult query other",
            summary: "The row an inner join yields.",
            note: "Written by the compiler, not by hand: both sides are present, so neither is optional.",
        },
    ),
    (
        "LeftJoinResult",
        BuiltinCard {
            signature: "LeftJoinResult query other",
            summary: "The row a LEFT outer join yields.",
            note: "Written by the compiler, not by hand: the right side may be missing, so it arrives optional.",
        },
    ),
    (
        "RightJoinResult",
        BuiltinCard {
            signature: "RightJoinResult query other",
            summary: "The row a RIGHT outer join yields.",
            note: "Written by the compiler, not by hand: the left side may be missing, so it arrives optional.",
        },
    ),
    (
        "FullJoinResult",
        BuiltinCard {
            signature: "FullJoinResult query other",
            summary: "The row a FULL outer join yields.",
            note: "Written by the compiler, not by hand: either side may be missing, so both arrive optional.",
        },
    ),
    (
        "InsertShape",
        BuiltinCard {
            signature: "InsertShape entity",
            summary: "What an insert takes: the entity without the columns the database fills in.",
            note: "Written by the compiler, not by hand. It reduces to the entity itself when nothing about it is generated.",
        },
    ),
];

#[cfg(test)]
mod tests {
    use super::{builtin_card, CARDS};
    use ridge_types::{fn_tycon_name, BuiltinTyCons, TyConArena, FN_ARITY_COUNT};

    /// A synthetic per-arity function constructor, recognised by asking the
    /// function that creates them. A list of literals here would go stale the
    /// day the arity ceiling moves; this cannot.
    fn is_fn_dispatch_key(name: &str) -> bool {
        (0..FN_ARITY_COUNT).any(|n| fn_tycon_name(n) == name)
    }

    /// Every built-in the compiler interns has a card, and the only ones
    /// excused are the function-type dispatch keys, which are never written in
    /// source and so can never be hovered.
    ///
    /// This is the check that makes the table total. It reads the built-ins
    /// from the one function that creates them, so a type added to the
    /// language shows up here whether or not anyone remembered this file.
    #[test]
    fn every_builtin_has_a_card() {
        let mut arena = TyConArena::new();
        let _ = BuiltinTyCons::allocate(&mut arena);

        let missing: Vec<&str> = arena
            .all()
            .iter()
            .filter(|d| d.def_span.is_none())
            .map(|d| d.name.as_str())
            .filter(|n| !is_fn_dispatch_key(n))
            .filter(|n| builtin_card(n).is_none())
            .collect();

        assert!(
            missing.is_empty(),
            "these built-in types would hover as nothing: {missing:?}\n\
             Add a card in this file, or — if the type is machinery a reader can \
             never write — say so in its card rather than leaving it out."
        );
    }

    /// The reverse direction. A card for a name the compiler does not intern is
    /// dead text that will never be shown, and usually means a rename landed on
    /// one side only.
    #[test]
    fn every_card_names_a_real_builtin() {
        let mut arena = TyConArena::new();
        let _ = BuiltinTyCons::allocate(&mut arena);

        let stale: Vec<&str> = CARDS
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| !arena.all().iter().any(|d| d.name == *n))
            .collect();

        assert!(
            stale.is_empty(),
            "cards for types that do not exist: {stale:?}"
        );
    }

    /// The `Int` card states the range as digits, and digits drift. Read them
    /// back out and check them against the type they describe, so the card
    /// cannot outlive the bound it claims.
    #[test]
    fn the_int_card_states_the_real_range() {
        let card = builtin_card("Int").expect("Int is carded");
        assert!(
            card.note.contains(&i64::MIN.to_string()),
            "the Int card should state the lower bound {}, got: {}",
            i64::MIN,
            card.note
        );
        assert!(
            card.note.contains(&i64::MAX.to_string()),
            "the Int card should state the upper bound {}, got: {}",
            i64::MAX,
            card.note
        );
    }

    /// Every card is a sentence, not a fragment: a summary that does not end in
    /// a full stop reads as truncated in the hover popup.
    #[test]
    fn every_summary_is_a_sentence() {
        for (name, card) in CARDS {
            assert!(
                card.summary.ends_with('.'),
                "the {name} summary should end in a full stop: {}",
                card.summary
            );
            assert!(
                card.note.is_empty() || card.note.ends_with('.'),
                "the {name} note should end in a full stop: {}",
                card.note
            );
            assert!(
                card.signature.starts_with(name),
                "the {name} signature should lead with the type's own name: {}",
                card.signature
            );
        }
    }

    /// No card names a runtime. A hover card is read as a statement about the
    /// language, so a sentence about how a value is laid out would be both a
    /// promise the language does not make and the first thing to rewrite for
    /// another backend.
    #[test]
    fn no_card_names_a_runtime() {
        const LEAKS: &[&str] = &[
            "BEAM", "OTP", "erlang", "Erlang", "beam", "bignum", "fixnum", "binary",
        ];
        for (name, card) in CARDS {
            for leak in LEAKS {
                assert!(
                    !card.summary.contains(leak) && !card.note.contains(leak),
                    "the {name} card mentions `{leak}`, which is a representation \
                     detail rather than something the language promises"
                );
            }
        }
    }
}
