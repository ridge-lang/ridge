-- Shared domain types and pure helpers re-used across acme apps.
pub type User = {
    id:    Int,
    name:  Text
}

pub fn greeting (u: User) -> Text =
    $"Hello, ${u.name}!"

pub fn nameOf (u: User) -> Text =
    u.name
