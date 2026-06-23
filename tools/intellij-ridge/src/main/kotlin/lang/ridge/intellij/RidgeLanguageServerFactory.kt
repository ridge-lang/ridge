package lang.ridge.intellij

import com.intellij.openapi.project.Project
import com.redhat.devtools.lsp4ij.LanguageServerFactory
import com.redhat.devtools.lsp4ij.client.features.LSPClientFeatures
import com.redhat.devtools.lsp4ij.server.StreamConnectionProvider

/** Wires `ridge-lsp` into LSP4IJ, with Ridge-specific semantic-token colours. */
class RidgeLanguageServerFactory : LanguageServerFactory {
    override fun createConnectionProvider(project: Project): StreamConnectionProvider =
        RidgeLspStreamConnectionProvider()

    override fun createClientFeatures(): LSPClientFeatures =
        LSPClientFeatures().setSemanticTokensFeature(RidgeSemanticTokensFeature())
}
