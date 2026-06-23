package lang.ridge.intellij

import com.intellij.execution.configurations.GeneralCommandLine
import com.redhat.devtools.lsp4ij.server.OSProcessStreamConnectionProvider

/**
 * Launches `ridge-lsp` over stdio. The server speaks only stdio (no TCP, no
 * sockets), so a plain child process with piped stdin/stdout is all LSP4IJ
 * needs. [GeneralCommandLine] takes care of Windows executable resolution and
 * argument quoting.
 */
class RidgeLspStreamConnectionProvider : OSProcessStreamConnectionProvider() {
    init {
        commandLine = GeneralCommandLine(RidgeLspResolver.resolve())
    }
}
