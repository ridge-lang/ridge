package lang.ridge.intellij

import com.intellij.openapi.application.PathManager
import com.intellij.openapi.diagnostic.logger
import org.jetbrains.plugins.textmate.api.TextMateBundleProvider
import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

/**
 * Ships the Ridge TextMate grammar so JetBrains IDEs colour keywords, strings,
 * comments, and numbers — the lexical tokens `ridge-lsp` does not emit as LSP
 * semantic tokens. It is the same `ridge.tmLanguage.json` the VS Code extension
 * uses (copied in at build time), so both editors stay in sync.
 *
 * TextMate needs a real directory, but the bundle travels inside the plugin
 * jar, so its known files are unpacked into the IDE system directory on first
 * request.
 */
class RidgeTextMateBundleProvider : TextMateBundleProvider {
    override fun getBundles(): List<TextMateBundleProvider.PluginBundle> {
        val dir = unpackBundle() ?: return emptyList()
        return listOf(TextMateBundleProvider.PluginBundle("Ridge", dir))
    }

    private fun unpackBundle(): Path? {
        val target = Path.of(PathManager.getSystemPath(), "ridge", "textmate")
        return try {
            for (relative in BUNDLE_FILES) {
                val resource = "/textmate/$relative"
                val input = javaClass.getResourceAsStream(resource)
                if (input == null) {
                    LOG.warn("Ridge TextMate bundle resource missing: $resource")
                    return null
                }
                val out = target.resolve(relative)
                Files.createDirectories(out.parent)
                input.use { Files.copy(it, out, StandardCopyOption.REPLACE_EXISTING) }
            }
            target
        } catch (e: Exception) {
            LOG.warn("Failed to unpack the Ridge TextMate bundle", e)
            null
        }
    }

    private companion object {
        val LOG = logger<RidgeTextMateBundleProvider>()
        val BUNDLE_FILES = listOf(
            "package.json",
            "language-configuration.json",
            "syntaxes/ridge.tmLanguage.json",
        )
    }
}
