# Ridge for JetBrains IDEs

Ridge language support for IntelliJ IDEA, PyCharm, GoLand, WebStorm, Rider, and
every other JetBrains IDE — Community and paid alike. The plugin runs the
[`ridge-lsp`](../../crates/ridge-lsp) server through
[LSP4IJ](https://github.com/redhat-developer/lsp4ij), so it offers the same
diagnostics, hover, navigation, find-usages, rename, signature help, formatting,
completion, and call/type hierarchy you get in the VS Code extension.

## Requirements

- A JetBrains IDE, build 2024.2 or newer (verified through the latest release).
- `ridge-lsp` on `PATH` or in `~/.cargo/bin`. The
  [install script](../install) puts it there; otherwise run
  `cargo install --path crates/ridge-lsp`.
- LSP4IJ. The Marketplace installs it automatically as a dependency; when you
  side-load the plugin from a `.zip`, install LSP4IJ first from
  **Settings | Plugins | Marketplace**.

The plugin finds the server the same way the VS Code extension does: an explicit
override first, then `~/.cargo/bin/ridge-lsp`, then `PATH`. Set the override
under **Settings | Languages & Frameworks | Ridge** if the binary lives
somewhere else.

## Build

Any JDK 17 or newer launches Gradle; the build provisions a JDK 21 toolchain
automatically (2024.2+ is compiled at Java 21).

```sh
./gradlew buildPlugin      # produces build/distributions/intellij-ridge-<version>.zip
./gradlew runIde           # launches a sandbox IDE with the plugin loaded
./gradlew verifyPlugin     # runs the JetBrains plugin verifier
```

Install the packaged `.zip` through **Settings | Plugins | ⚙ | Install Plugin
from Disk…**.

## Layout

- `src/main/kotlin/lang/ridge/intellij/` — the plugin: file type, settings, and
  the LSP4IJ wiring that launches `ridge-lsp`.
- `src/main/resources/META-INF/plugin.xml` — plugin descriptor.
- `src/main/resources/icons/` — file-type and plugin icons.
