-- std.env — Environment variable access (Tier 3, imports std.text, std.option).
--
-- All functions require the `env` capability.
-- §3.14: os:getenv/1 returns false for unset variables; bridge returns None.

-- Look up an environment variable by name.
-- Returns Some(value) if set, None if unset.
@ffi("ridge_rt", "env_get", 1)
pub fn env get (name: Text) -> Option Text

-- Set an environment variable to a value.
@ffi("ridge_rt", "env_set", 2)
pub fn env set (name: Text) (value: Text) -> Unit

-- Return all environment variables as a Map from name to value.
@ffi("ridge_rt", "env_all", 1)
pub fn env all (_unit: Unit) -> Map Text Text
