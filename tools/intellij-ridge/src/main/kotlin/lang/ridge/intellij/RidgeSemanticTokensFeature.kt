package lang.ridge.intellij

import com.intellij.openapi.editor.colors.TextAttributesKey
import com.intellij.psi.PsiFile
import com.redhat.devtools.lsp4ij.client.features.LSPSemanticTokensFeature

/**
 * Colours the semantic-token kinds the stock IntelliJ schemes leave grey —
 * type names, parameters, and call-site function references — to match the VS
 * Code extension. Everything else keeps LSP4IJ's default colouring (function
 * and method declarations, fields, and so on).
 */
class RidgeSemanticTokensFeature : LSPSemanticTokensFeature() {
    override fun getTextAttributesKey(
        tokenType: String,
        tokenModifiers: List<String>,
        file: PsiFile,
    ): TextAttributesKey? = when (tokenType) {
        "type", "class", "enum", "interface", "struct", "typeParameter" -> RidgeColors.TYPE
        "parameter" -> RidgeColors.PARAMETER
        "function" -> RidgeColors.FUNCTION
        else -> super.getTextAttributesKey(tokenType, tokenModifiers, file)
    }
}
