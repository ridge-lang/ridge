package lang.ridge.intellij

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.openapi.vfs.VirtualFile
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

    // Opt into the code lenses the server gates behind this flag. Without it the
    // server serves none, so an editor that can't run the lens commands never
    // shows inert lenses; the Run/Run-test lenses are handled by the
    // `ridge.run` / `ridge.test` actions registered in plugin.xml.
    override fun getInitializationOptions(rootUri: VirtualFile?): Any =
        mapOf(
            "codeLens" to
                mapOf(
                    "references" to true,
                    "implementations" to true,
                    "run" to true,
                    "runTest" to true,
                ),
        )
}
