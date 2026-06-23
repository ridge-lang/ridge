package lang.ridge.intellij

import com.intellij.openapi.util.SystemInfo
import java.io.File

/**
 * Finds the `ridge-lsp` binary, following the same chain as the VS Code
 * extension so both editors behave identically:
 *
 *  1. the user's explicit override ([RidgeSettings.lspPath]),
 *  2. the canonical `cargo install` destination `~/.cargo/bin`,
 *  3. each directory on `PATH`,
 *  4. the bare name, letting the OS resolve it (or fail with a clear error).
 *
 * The environment an IDE inherits from the launcher does not always carry
 * `~/.cargo/bin` on `PATH`, which is why step 2 checks it explicitly.
 */
object RidgeLspResolver {
    fun resolve(): String {
        val binName = if (SystemInfo.isWindows) "ridge-lsp.exe" else "ridge-lsp"

        val configured = RidgeSettings.getInstance().lspPath
        if (configured.isNotBlank() && File(configured).isFile) {
            return configured
        }

        val cargoBin = File(System.getProperty("user.home"))
            .resolve(".cargo")
            .resolve("bin")
            .resolve(binName)
        if (cargoBin.isFile) {
            return cargoBin.absolutePath
        }

        val path = System.getenv("PATH").orEmpty()
        for (dir in path.split(File.pathSeparatorChar)) {
            if (dir.isBlank()) continue
            val candidate = File(dir, binName)
            if (candidate.isFile) {
                return candidate.absolutePath
            }
        }

        return binName
    }
}
