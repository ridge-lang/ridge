-- Independent project in the `acme` workspace (no cross-project imports —
-- cross-project type seeding is deferred (Phase 4 §7 user-module
-- cross-imports note).  The workspace-pipeline DoD (§9.5) is exercised by
-- having multiple projects under one `ridge.toml`, each typechecking via
-- `typecheck_workspace`.
type Greeting = {
    salutation: Text,
    target:     Text
}

pub fn render (g: Greeting) -> Text =
    $"${g.salutation}, ${g.target}!"

pub fn helloWorld () -> Text =
    render (Greeting { salutation = "Hello", target = "World" })
