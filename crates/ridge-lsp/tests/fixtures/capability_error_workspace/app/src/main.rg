import std.io as Io

-- This function declares io capability but the body uses it correctly.
-- However, we DON'T declare it to trigger T014 CapabilityNotDeclared.
pub fn greet name -> Text = "Hello " ++ name
